//! The background task behind [`RawDevice::connect`](crate::raw::RawDevice::connect):
//! connect, subscribe, start the headset streaming, and feed notifications
//! through a [`DecodePipeline`] into the data channel — reconnecting when the
//! link drops, and stopping wherever it is when asked to.

use crate::raw::device::{
    lock_stats, DeviceInfo, RawDeviceHandle, ReconnectPolicy, StreamDebugStats,
};
use crate::raw::discovery::{
    resolve_device, serial_candidates, shared_bluetooth, BATTERY_LEVEL_CHARACTERISTIC,
    EMOTIV_CONTROL_SERVICE, EMOTIV_DATA_SERVICE, EMOTIV_EPOC_X_AUX_CHARACTERISTIC,
    EMOTIV_EPOC_X_EEG_CHARACTERISTIC, EMOTIV_EPOC_X_SERVICE,
};
use crate::raw::pipeline::DecodePipeline;
use crate::raw::platform;
use crate::raw::types::{DecryptedData, DeviceState, HeadsetModel};
use anyhow::{anyhow, Result};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};
use uuid::Uuid;
use webbluetooth::future::{select, Either};
use webbluetooth::stream::{select_all, BoxStream, SelectAll, StreamExt};
use webbluetooth::uuid::BluetoothUuid;
use webbluetooth::{BluetoothDevice, RemoteGattCharacteristic};

#[cfg(target_os = "windows")]
const BLE_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(not(target_os = "windows"))]
const BLE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn the streaming task for `info`; returns the data channel and the
/// handle that controls it.
pub(crate) fn start(
    info: DeviceInfo,
    policy: ReconnectPolicy,
) -> (mpsc::Receiver<DecryptedData>, RawDeviceHandle) {
    let (tx, rx) = mpsc::channel(256);
    let (state_tx, state_rx) = watch::channel(DeviceState::Connecting);
    let stop = Arc::new(Notify::new());
    let stats = Arc::new(Mutex::new(StreamDebugStats::default()));

    let task = Streamer {
        info,
        policy,
        tx,
        stop: Arc::clone(&stop),
        state: state_tx,
        stats: Arc::clone(&stats),
    };
    platform::spawn(task.run());

    (rx, RawDeviceHandle::new(state_rx, stop, stats))
}

/// Subscribe to every characteristic in `chars`, merged into one stream of
/// `(characteristic UUID, value)`.
///
/// Web Bluetooth subscribes per characteristic and yields bare values, so the
/// UUID is attached here. A characteristic that refuses is logged and skipped.
async fn subscribe_all(
    chars: &[RemoteGattCharacteristic],
) -> SelectAll<BoxStream<'static, (Uuid, Vec<u8>)>> {
    let mut streams = Vec::with_capacity(chars.len());
    for ch in chars {
        let uuid = Uuid::from(*ch.uuid());
        match ch.start_notifications().await {
            Ok(values) => streams.push(values.map(move |value| (uuid, value)).boxed()),
            Err(err) => log::warn!("Failed to subscribe {}: {}", uuid, err),
        }
    }
    select_all(streams)
}

/// How a session ended without an error.
enum SessionEnd {
    /// The link dropped (or every notification stream ended).
    LinkLost,
    /// Nobody is reading the data channel any more.
    ReceiverClosed,
}

/// The background task behind [`RawDevice::connect`].
struct Streamer {
    info: DeviceInfo,
    policy: ReconnectPolicy,
    tx: mpsc::Sender<DecryptedData>,
    stop: Arc<Notify>,
    state: watch::Sender<DeviceState>,
    stats: Arc<Mutex<StreamDebugStats>>,
}

impl Streamer {
    async fn run(self) {
        // Each phase is raced against a stop request, so `disconnect()` takes
        // effect wherever the task is — scanning, connecting, streaming or
        // waiting to reconnect. The device is resolved before streaming starts
        // and held out here, so the link can be closed however streaming ends.
        let device = match unless_stopped(&self.stop, self.resolve()).await {
            Some(Ok(device)) => Some(device),
            Some(Err(e)) => {
                log::error!("Device streaming error: {e:#}");
                None
            }
            None => {
                log::info!("Disconnect requested.");
                None
            }
        };

        if let Some(device) = &device {
            match unless_stopped(&self.stop, self.stream_with_reconnect(device)).await {
                Some(Err(e)) => log::error!("Device streaming error: {e:#}"),
                None => log::info!("Disconnect requested."),
                Some(Ok(())) => {}
            }
            device.gatt().disconnect();
        }
        let _ = self.state.send(DeviceState::Disconnected);
    }

    async fn resolve(&self) -> Result<BluetoothDevice> {
        let bluetooth = shared_bluetooth().await?;
        resolve_device(&bluetooth, &self.info).await
    }

    async fn stream_with_reconnect(&self, device: &BluetoothDevice) -> Result<()> {
        let mut pipeline = DecodePipeline::new(self.info.model, &serial_candidates(&self.info))?;
        let mut first = true;
        let mut attempt = 0u32;

        loop {
            let delivered_before = pipeline.stats().decoded;
            match self.session(device, &mut pipeline).await {
                Ok(SessionEnd::ReceiverClosed) => return Ok(()),
                Ok(SessionEnd::LinkLost) => {
                    log::info!("BLE link lost.");
                    if pipeline.stats().decoded > delivered_before {
                        attempt = 0;
                    }
                }
                // Failing to connect at all is the caller's to hear about.
                Err(e) if first => return Err(e),
                Err(e) => log::warn!("Reconnect attempt {attempt} failed: {e:#}"),
            }
            first = false;

            if attempt >= self.policy.max_attempts {
                if self.policy.max_attempts > 0 {
                    log::warn!("Giving up after {attempt} reconnect attempts.");
                }
                return Ok(());
            }
            let delay = self.policy.delay(attempt);
            attempt += 1;
            let _ = self.state.send(DeviceState::Reconnecting);
            lock_stats(&self.stats).reconnects += 1;
            log::info!(
                "Reconnecting in {:.1} s (attempt {attempt}/{})",
                delay.as_secs_f64(),
                self.policy.max_attempts
            );
            platform::sleep(delay).await;
        }
    }

    /// One BLE link: connect, subscribe, start, and stream until it ends.
    async fn session(
        &self,
        device: &BluetoothDevice,
        pipeline: &mut DecodePipeline,
    ) -> Result<SessionEnd> {
        let _ = self.state.send(DeviceState::Connecting);
        let gatt = device.gatt();

        // Windows resolves the BLE address and discovers GATT services as part
        // of connect(); WinRT can take several seconds for each queued request.
        if !gatt.connected() {
            platform::timeout(BLE_CONNECT_TIMEOUT, gatt.connect())
                .await
                .map_err(|_| {
                    anyhow!(
                        "BLE connect() timed out after {} s",
                        BLE_CONNECT_TIMEOUT.as_secs()
                    )
                })??;
        }

        // One discovery pass over every granted service.
        let all_chars = platform::timeout(Duration::from_secs(15), gatt.get_characteristics())
            .await
            .map_err(|_| anyhow!("service discovery timed out after 15 s"))??;

        let notify_chars = select_notify_characteristics(&all_chars);
        if notify_chars.is_empty() {
            gatt.disconnect();
            return Err(anyhow!(
                "No notifiable BLE characteristics found for device"
            ));
        }

        let mut notifications = subscribe_all(&notify_chars).await;
        if notifications.is_empty() {
            gatt.disconnect();
            return Err(anyhow!("Could not subscribe to any BLE characteristic"));
        }
        let writes = send_start_stream_command(&all_chars).await;
        {
            let mut s = lock_stats(&self.stats);
            s.subscribed_characteristics =
                notify_chars.iter().map(|c| c.uuid().to_string()).collect();
            s.start_command_writes += writes as u64;
        }

        pipeline.resume();
        let _ = self.state.send(DeviceState::Streaming);
        let mut silence_timeouts = 0u64;

        loop {
            match platform::timeout(Duration::from_secs(5), notifications.next()).await {
                Ok(Some((uuid, payload))) => {
                    silence_timeouts = 0;
                    if should_decode_notification(
                        self.info.model,
                        uuid,
                        pipeline.eeg_uuid().is_some(),
                    ) {
                        let decoded = pipeline.push(uuid, &payload);
                        self.record(pipeline, uuid, payload.len());
                        for data in decoded {
                            if self.tx.send(data).await.is_err() {
                                gatt.disconnect();
                                return Ok(SessionEnd::ReceiverClosed);
                            }
                        }
                    } else {
                        self.record(pipeline, uuid, payload.len());
                    }
                }
                // Every notification stream ends when the device disconnects.
                Ok(None) => return Ok(SessionEnd::LinkLost),
                Err(_) => {
                    silence_timeouts += 1;
                    lock_stats(&self.stats).timeout_count += 1;
                    if !gatt.connected() {
                        return Ok(SessionEnd::LinkLost);
                    }

                    // Resubscribe before dropping the old streams, so there is
                    // no window in which nothing is subscribed.
                    let fresh = subscribe_all(&notify_chars).await;
                    if !fresh.is_empty() {
                        notifications = fresh;
                    }
                    let w = send_start_stream_command(&all_chars).await;
                    lock_stats(&self.stats).start_command_writes += w as u64;
                    let p = pipeline.stats();
                    log::warn!(
                        "No BLE notifications for 5s (received={}, decoded={}, silence_timeouts={}); re-issued subscribe/start",
                        p.received, p.decoded, silence_timeouts,
                    );
                }
            }
        }
    }

    /// Mirror the pipeline's view into the shared stats.
    fn record(&self, pipeline: &DecodePipeline, uuid: Uuid, payload_len: usize) {
        let p = pipeline.stats();
        let mut s = lock_stats(&self.stats);
        s.received_notifications = p.received;
        s.decoded_packets = p.decoded;
        s.decrypt_failures = p.failures;
        s.motion_packets = p.motion_packets;
        s.last_notify_uuid = Some(uuid.to_string());
        s.last_payload_len = payload_len;
        s.active_serial_candidate = pipeline.locked_key();
        s.active_notify_uuid = pipeline.eeg_uuid().map(|u| u.to_string());
    }
}

/// Run `future` unless a stop is requested first; `None` if it was.
async fn unless_stopped<F: Future>(stop: &Notify, future: F) -> Option<F::Output> {
    let future = std::pin::pin!(future);
    let stopped = std::pin::pin!(stop.notified());
    match select(future, stopped).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(_) => None,
    }
}

/// Write the start-stream command to every writable characteristic of the
/// control service. Returns how many writes succeeded.
async fn send_start_stream_command(chars: &[RemoteGattCharacteristic]) -> usize {
    let control_service = BluetoothUuid::from(EMOTIV_CONTROL_SERVICE);
    let payloads: [&[u8]; 2] = [&[0x01_u8], &[0x01_u8, 0x00_u8]];
    let mut writes = 0usize;

    for ch in chars
        .iter()
        .filter(|ch| *ch.service_uuid() == control_service)
    {
        let props = ch.properties();
        if !(props.write() || props.write_without_response()) {
            continue;
        }
        for payload in payloads {
            let result = if props.write_without_response() {
                ch.write_value_without_response(payload).await
            } else {
                ch.write_value_with_response(payload).await
            };
            match result {
                Ok(()) => writes += 1,
                Err(e) => log::debug!("start-stream write to {} failed: {e}", ch.uuid()),
            }
        }
    }

    writes
}

/// Notifiable characteristics of the data service, or — if it has none —
/// every notifiable characteristic except Battery Level.
fn select_notify_characteristics(
    chars: &[RemoteGattCharacteristic],
) -> Vec<RemoteGattCharacteristic> {
    let data_service = BluetoothUuid::from(EMOTIV_DATA_SERVICE);
    let battery_level = BluetoothUuid::from(BATTERY_LEVEL_CHARACTERISTIC);
    let notifiable = |ch: &&RemoteGattCharacteristic| {
        let props = ch.properties();
        props.notify() || props.indicate()
    };

    let mut epoc_x_preferred: Vec<_> = chars
        .iter()
        .filter(notifiable)
        .filter(|ch| {
            epoc_x_notification_rank(Uuid::from(*ch.service_uuid()), Uuid::from(*ch.uuid()))
                .is_some()
        })
        .cloned()
        .collect();
    epoc_x_preferred.sort_by_key(|ch| {
        epoc_x_notification_rank(Uuid::from(*ch.service_uuid()), Uuid::from(*ch.uuid()))
            .unwrap_or(u8::MAX)
    });
    if !epoc_x_preferred.is_empty() {
        return epoc_x_preferred;
    }

    let preferred: Vec<_> = chars
        .iter()
        .filter(notifiable)
        .filter(|ch| *ch.service_uuid() == data_service)
        .cloned()
        .collect();

    if !preferred.is_empty() {
        return preferred;
    }

    chars
        .iter()
        .filter(notifiable)
        .filter(|ch| *ch.uuid() != battery_level)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoc_x_auxiliary_notification_is_subscribed_before_eeg() {
        assert_eq!(
            epoc_x_notification_rank(EMOTIV_EPOC_X_SERVICE, EMOTIV_EPOC_X_AUX_CHARACTERISTIC),
            Some(0)
        );
        assert_eq!(
            epoc_x_notification_rank(EMOTIV_EPOC_X_SERVICE, EMOTIV_EPOC_X_EEG_CHARACTERISTIC),
            Some(1)
        );
        assert_eq!(
            epoc_x_notification_rank(EMOTIV_EPOC_X_SERVICE, Uuid::nil()),
            None
        );
        assert_eq!(
            epoc_x_notification_rank(EMOTIV_DATA_SERVICE, EMOTIV_EPOC_X_AUX_CHARACTERISTIC),
            None
        );
        assert!(!should_decode_notification(
            HeadsetModel::EpocX,
            EMOTIV_EPOC_X_AUX_CHARACTERISTIC,
            false
        ));
        assert!(should_decode_notification(
            HeadsetModel::EpocX,
            EMOTIV_EPOC_X_AUX_CHARACTERISTIC,
            true
        ));
        assert!(should_decode_notification(
            HeadsetModel::EpocX,
            EMOTIV_EPOC_X_EEG_CHARACTERISTIC,
            false
        ));
        assert!(should_decode_notification(
            HeadsetModel::EpocPlus,
            EMOTIV_EPOC_X_AUX_CHARACTERISTIC,
            false
        ));
    }
}

fn should_decode_notification(model: HeadsetModel, uuid: Uuid, eeg_locked: bool) -> bool {
    model != HeadsetModel::EpocX || uuid != EMOTIV_EPOC_X_AUX_CHARACTERISTIC || eeg_locked
}

fn epoc_x_notification_rank(service: Uuid, characteristic: Uuid) -> Option<u8> {
    if service != EMOTIV_EPOC_X_SERVICE {
        return None;
    }
    if characteristic == EMOTIV_EPOC_X_AUX_CHARACTERISTIC {
        Some(0)
    } else if characteristic == EMOTIV_EPOC_X_EEG_CHARACTERISTIC {
        Some(1)
    } else {
        None
    }
}
