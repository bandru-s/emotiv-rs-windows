//! The public face of a raw BLE connection: [`DeviceInfo`], [`RawDevice`],
//! [`RawDeviceHandle`] and the policies and statistics around them.
//!
//! The work happens elsewhere: [`crate::raw::discovery`] finds and identifies
//! headsets, `stream` (internal) runs the background connection — one
//! *session* per BLE link, reconnecting under a [`ReconnectPolicy`] — and
//! [`crate::raw::pipeline`] turns notifications into [`DecryptedData`].

use crate::raw::discovery::{
    discover_ble_devices, discover_usb_devices, infer_model_from_name, infer_serial, is_zero_mac,
    mac_from_id, normalize_id,
};
use crate::raw::stream;
use crate::raw::types::{DecryptedData, DeviceState, HeadsetModel};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};

/// Device transport type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportType {
    Ble,
    Usb,
    UsbSerial,
}

impl std::fmt::Display for TransportType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ble => write!(f, "BLE"),
            Self::Usb => write!(f, "USB HID"),
            Self::UsbSerial => write!(f, "USB Serial"),
        }
    }
}

/// Information about a discovered device.
///
/// Discovery fills one in; to connect to a known headset without discovery,
/// build one with [`DeviceInfo::ble`]:
///
/// ```
/// # use emotiv::raw::{DeviceInfo, HeadsetModel};
/// let info = DeviceInfo::ble("AA:BB:CC:DD:EE:FF", "EPOC X")
///     .with_serial("E50202E9ABCD1234");
/// assert_eq!(info.model, HeadsetModel::EpocX);
/// assert_eq!(info.ble_mac.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
/// ```
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub address: String,
    /// Platform BLE identifier.
    /// • macOS / iOS — a per-host UUID the system assigns; Apple never
    ///   exposes the hardware address
    /// • elsewhere — the Bluetooth address (`AA:BB:CC:DD:EE:FF`)
    pub ble_id: String,
    /// The Bluetooth address, where the platform exposes one (not on Apple).
    pub ble_mac: Option<String>,
    pub serial: String,
    pub model: HeadsetModel,
    pub transport: TransportType,
    pub battery_percent: u8,
    pub is_connected: bool,
}

impl Default for DeviceInfo {
    /// An unnamed EPOC X over BLE with nothing known about it — a base for
    /// struct-update syntax.
    fn default() -> Self {
        Self {
            name: "(unknown)".into(),
            address: String::new(),
            ble_id: String::new(),
            ble_mac: None,
            serial: String::new(),
            model: HeadsetModel::EpocX,
            transport: TransportType::Ble,
            battery_percent: 0,
            is_connected: false,
        }
    }
}

impl DeviceInfo {
    /// A BLE headset known by its platform id and advertised name, with the
    /// address, model and serial inferred exactly as discovery infers them.
    pub fn ble(id: impl Into<String>, name: impl Into<String>) -> Self {
        let ble_id = id.into();
        let name = name.into();
        let name = if name.trim().is_empty() {
            "(unknown)".to_string()
        } else {
            name
        };
        let ble_mac = mac_from_id(&ble_id);
        let serial = infer_serial(&name, ble_mac.as_deref().unwrap_or(&ble_id));
        Self {
            model: infer_model_from_name(&name).unwrap_or(HeadsetModel::EpocX),
            address: ble_id.clone(),
            ble_id,
            ble_mac,
            serial,
            name,
            ..Self::default()
        }
    }

    /// Use this serial for the AES key instead of the inferred one.
    pub fn with_serial(mut self, serial: impl Into<String>) -> Self {
        self.serial = serial.into();
        self
    }

    /// Use this model instead of the one inferred from the name.
    pub fn with_model(mut self, model: HeadsetModel) -> Self {
        self.model = model;
        self
    }
}

// ── Choosing a device ─────────────────────────────────────────────────────────

impl DeviceInfo {
    /// Whether `query` identifies this device: equal (ignoring case) to its
    /// name, address, BLE id, MAC or serial — or, comparing alphanumerics only
    /// and ignoring case, contained in one of them or containing it. So
    /// `"aabbcc"`, `"AA:BB:CC:DD:EE:FF"` and a serial suffix all work.
    pub fn matches(&self, query: &str) -> bool {
        let query_norm = normalize_id(query);
        [
            Some(self.name.as_str()),
            Some(self.address.as_str()),
            Some(self.ble_id.as_str()),
            self.ble_mac.as_deref(),
            Some(self.serial.as_str()),
        ]
        .into_iter()
        .flatten()
        .any(|field| {
            if field.eq_ignore_ascii_case(query) {
                return true;
            }
            let field_norm = normalize_id(field);
            !field_norm.is_empty()
                && !query_norm.is_empty()
                && (field_norm.contains(&query_norm) || query_norm.contains(&field_norm))
        })
    }

    /// Whether this looks like an Emotiv headset rather than something else
    /// that happened to match discovery: an Emotiv-like name, or an
    /// already-connected device that is not generically named.
    pub fn looks_like_emotiv(&self) -> bool {
        if has_emotiv_name(&self.name) {
            return true;
        }
        let name = self.name.to_ascii_lowercase();
        if name == "(unknown)" || name.starts_with("gb-") || name.starts_with("ble") {
            return false;
        }
        self.is_connected
    }

    /// How good an auto-connect candidate this is; higher is better.
    fn score(&self) -> i32 {
        let mut score = 0;
        if self.name != "(unknown)" {
            score += 20;
        }
        if has_emotiv_name(&self.name) {
            score += 50;
        }
        if self.ble_mac.as_deref().is_some_and(|m| !is_zero_mac(m)) {
            score += 15;
        }
        if !normalize_id(&self.ble_id).is_empty() {
            score += 10;
        }
        if self.is_connected {
            score += 10;
        }
        score
    }
}

fn has_emotiv_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["emotiv", "epoc", "insight", "flex", "mn8", "xtrodes"]
        .iter()
        .any(|word| name.contains(word))
}

/// The device to auto-connect to: the best-scoring one that
/// [looks like an Emotiv headset](DeviceInfo::looks_like_emotiv), or the
/// best-scoring device overall if none does. Earlier devices win ties.
pub fn select_best_device(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    let likely = devices.iter().filter(|d| d.looks_like_emotiv());
    best_scoring(likely).or_else(|| best_scoring(devices.iter()))
}

/// Highest [`DeviceInfo::score`], earliest on a tie.
fn best_scoring<'a>(
    devices: impl DoubleEndedIterator<Item = &'a DeviceInfo>,
) -> Option<&'a DeviceInfo> {
    // `max_by_key` keeps the last maximum, so search from the back.
    devices.rev().max_by_key(|d| d.score())
}

/// The first device that [matches](DeviceInfo::matches) `query`.
pub fn find_device<'a>(devices: &'a [DeviceInfo], query: &str) -> Option<&'a DeviceInfo> {
    devices.iter().find(|d| d.matches(query))
}

/// Runtime debug statistics for raw BLE stream processing.
#[derive(Debug, Clone, Default)]
pub struct StreamDebugStats {
    pub received_notifications: u64,
    pub decoded_packets: u64,
    /// Notifications that produced no EEG: searched without a key lock, or
    /// failed under the locked key.
    pub decrypt_failures: u64,
    pub timeout_count: u64,
    pub last_notify_uuid: Option<String>,
    pub last_payload_len: usize,
    /// The locked key as `serial/model`, with `:partial` / `:unconfirmed`.
    pub active_serial_candidate: Option<String>,
    pub subscribed_characteristics: Vec<String>,
    pub start_command_writes: u64,
    pub active_notify_uuid: Option<String>,
    /// Motion packets decrypted (see `DecryptedData::motion`).
    pub motion_packets: u64,
    /// Times the link dropped and a reconnect was attempted.
    pub reconnects: u64,
}

/// When and how often to reconnect after the BLE link drops.
///
/// Only a drop *after* streaming started is retried; failing to connect in the
/// first place is returned as an error, as before. Waits double from
/// `initial_delay` up to `max_delay`, and the count resets whenever a
/// reconnected session delivers data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Consecutive failed attempts before giving up. `0` disables reconnecting.
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for ReconnectPolicy {
    /// Ten attempts, 1 s doubling to 30 s — about four minutes of trying.
    fn default() -> Self {
        Self {
            max_attempts: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl ReconnectPolicy {
    /// Never reconnect: the stream ends when the link drops.
    pub fn disabled() -> Self {
        Self {
            max_attempts: 0,
            ..Self::default()
        }
    }

    /// The wait before attempt `attempt` (0-based).
    pub fn delay(&self, attempt: u32) -> Duration {
        self.initial_delay
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.max_delay)
    }
}

/// Handle to a connected device.
///
/// Dropping it leaves the stream running; the stream ends when the receiver
/// returned alongside it is dropped, or the device goes away for good.
pub struct RawDeviceHandle {
    state: watch::Receiver<DeviceState>,
    stop: Arc<Notify>,
    debug_stats: Arc<Mutex<StreamDebugStats>>,
}

impl RawDeviceHandle {
    pub(crate) fn new(
        state: watch::Receiver<DeviceState>,
        stop: Arc<Notify>,
        debug_stats: Arc<Mutex<StreamDebugStats>>,
    ) -> Self {
        Self {
            state,
            stop,
            debug_stats,
        }
    }

    /// Stop streaming and close the link. Returns once the device is
    /// disconnected — immediately if it already was.
    pub async fn disconnect(&self) -> Result<()> {
        self.stop.notify_one();
        let mut state = self.state.clone();
        // An `Err` means the task has already ended, which is disconnected too.
        let _ = state.wait_for(|s| *s == DeviceState::Disconnected).await;
        Ok(())
    }

    /// Check current connection state.
    pub async fn state(&self) -> DeviceState {
        *self.state.borrow()
    }

    /// Snapshot of live BLE receive/decode stats.
    pub async fn debug_stats(&self) -> StreamDebugStats {
        lock_stats(&self.debug_stats).clone()
    }
}

/// The stats lock is never held across an `await` and nothing panics while
/// holding it, but a poisoned lock is still better read than propagated.
pub(crate) fn lock_stats(
    stats: &Mutex<StreamDebugStats>,
) -> std::sync::MutexGuard<'_, StreamDebugStats> {
    stats.lock().unwrap_or_else(|e| e.into_inner())
}

/// Raw device for BLE/USB connection.
pub struct RawDevice {
    info: DeviceInfo,
    reconnect: ReconnectPolicy,
}

impl RawDevice {
    /// Create from device info, reconnecting under the default
    /// [`ReconnectPolicy`].
    pub fn from_info(info: DeviceInfo) -> Self {
        Self {
            info,
            reconnect: ReconnectPolicy::default(),
        }
    }

    /// Reconnect under `policy` instead of the default.
    pub fn with_reconnect(mut self, policy: ReconnectPolicy) -> Self {
        self.reconnect = policy;
        self
    }

    /// Discover all connected Emotiv devices.
    pub async fn discover() -> Result<Vec<DeviceInfo>> {
        let mut devices = Vec::new();
        match discover_ble_devices().await {
            Ok(found) => devices.extend(found),
            Err(e) => log::warn!("BLE discovery failed: {e:#}"),
        }
        devices.extend(discover_usb_devices().await.unwrap_or_default());
        Ok(devices)
    }

    /// Connect to this device and start streaming in the background.
    ///
    /// Returns at once; follow progress through [`RawDeviceHandle::state`].
    /// The receiver closes when the stream ends for good.
    pub async fn connect(&self) -> Result<(mpsc::Receiver<DecryptedData>, RawDeviceHandle)> {
        Ok(stream::start(self.info.clone(), self.reconnect.clone()))
    }

    /// Get device info.
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, id: &str) -> DeviceInfo {
        DeviceInfo::ble(id, name)
    }

    #[test]
    fn test_matches_any_identifier() {
        let info = named("EPOC X ABCDEF123456", "aa:bb:cc:dd:ee:ff");
        assert!(info.matches("AA:BB:CC:DD:EE:FF"));
        assert!(info.matches("aabbccddeeff"));
        assert!(info.matches("epoc x"));
        assert!(info.matches("ABCDEF123456"));
        assert!(!info.matches("Insight"));
        assert!(!info.matches(""));
    }

    #[test]
    fn test_select_best_device_prefers_emotiv_names() {
        let devices = vec![
            named("GB-1234", "11:11:11:11:11:11"),
            named("Insight 5AF2", "22:22:22:22:22:22"),
            named("EPOC X 9C1D", "33:33:33:33:33:33"),
        ];
        // Both Emotiv names score the same; the earlier one wins.
        assert_eq!(select_best_device(&devices).unwrap().name, "Insight 5AF2");
        assert_eq!(find_device(&devices, "333333").unwrap().name, "EPOC X 9C1D");
        assert!(select_best_device(&[]).is_none());

        // Nothing Emotiv-like: fall back to the best overall.
        let others = vec![
            named("", "44:44:44:44:44:44"),
            named("Thermometer", "55:55:55:55:55:55"),
        ];
        assert_eq!(select_best_device(&others).unwrap().name, "Thermometer");
    }

    #[test]
    fn test_reconnect_backoff_doubles_to_the_cap() {
        let policy = ReconnectPolicy::default();
        let secs: Vec<u64> = (0..7).map(|a| policy.delay(a).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(policy.delay(u32::MAX), Duration::from_secs(30));
        assert_eq!(ReconnectPolicy::disabled().max_attempts, 0);
    }

    #[test]
    fn test_device_info_ble_infers_like_discovery() {
        let info = DeviceInfo::ble("aa:bb:cc:dd:ee:ff", "Insight 2 ABCDEF123456");
        assert_eq!(info.model, HeadsetModel::Insight2);
        assert_eq!(info.serial, "ABCDEF123456");
        assert_eq!(info.ble_mac.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(info.address, info.ble_id);

        // Apple-style id: no address, serial from the id.
        let mac_less = DeviceInfo::ble("6F1C2B9E-3D4A-4C5B-9E8F-0A1B2C3D4E5F", "");
        assert_eq!(mac_less.name, "(unknown)");
        assert_eq!(mac_less.ble_mac, None);
        assert_eq!(
            mac_less.with_model(HeadsetModel::EpocPlus).model,
            HeadsetModel::EpocPlus
        );
    }
}
