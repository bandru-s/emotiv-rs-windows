use emotiv::raw::{
    self,
    device::RawDeviceHandle,
    types::{DecryptedData, DeviceState, HeadsetModel},
    DeviceInfo, RawDevice,
};
use serde_json::{json, Value};
use std::io::{self, Write};
use tokio::io::{AsyncBufReadExt, BufReader};

fn emit(event: Value) {
    println!("{event}");
    let _ = io::stdout().flush();
}

fn emit_error(message: impl std::fmt::Display) {
    emit(json!({ "type": "error", "message": message.to_string() }));
}

async fn next_packet(
    receiver: &mut Option<tokio::sync::mpsc::Receiver<DecryptedData>>,
) -> Option<DecryptedData> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn device_json(device: &DeviceInfo) -> Value {
    json!({
        "name": device.name,
        "ble_id": device.ble_id,
        "serial": device.serial,
        "model": device.model.name(),
        "battery_percent": device.battery_percent,
        "is_connected": device.is_connected,
        "channels": device.model.channels()
    })
}

fn emit_status(state: &str, message: &str) {
    emit(json!({ "type": "status", "state": state, "message": message }));
}

async fn disconnect(
    receiver: &mut Option<tokio::sync::mpsc::Receiver<DecryptedData>>,
    handle: &mut Option<RawDeviceHandle>,
) {
    *receiver = None;
    if let Some(handle) = handle.take() {
        if let Err(error) = handle.disconnect().await {
            emit_error(format!("A headset leválasztása nem sikerült: {error:#}"));
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut devices: Vec<DeviceInfo> = Vec::new();
    let mut current_device: Option<DeviceInfo> = None;
    let mut receiver = None;
    let mut handle = None;
    let mut last_device_state = None;
    let mut stream_announced = false;
    let mut state_poll = tokio::time::interval(std::time::Duration::from_millis(250));

    emit_status("ready", "Készen áll. Keresd meg a headsetet Bluetoothon.");

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        emit_error(format!("Nem olvasható a parancs: {error}"));
                        break;
                    }
                };
                let command: Value = match serde_json::from_str(&line) {
                    Ok(command) => command,
                    Err(error) => {
                        emit_error(format!("Érvénytelen parancs: {error}"));
                        continue;
                    }
                };

                match command["command"].as_str() {
                    Some("discover") => {
                        emit_status("scanning", "EMOTIV headsetek keresése Bluetoothon...");
                        match raw::discover_devices().await {
                            Ok(found) => {
                                devices = found
                                    .into_iter()
                                    .map(|device| {
                                        let name = device.name.to_ascii_uppercase();
                                        if name.contains("EPOCX") || name.contains("EPOC X") {
                                            device.with_model(HeadsetModel::EpocX)
                                        } else {
                                            device
                                        }
                                    })
                                    .collect();
                                emit(json!({
                                    "type": "devices",
                                    "devices": devices.iter().map(device_json).collect::<Vec<_>>()
                                }));
                                emit_status("ready", "A headsetkeresés befejeződött.");
                            }
                            Err(error) => {
                                emit_error(format!("A headsetek keresése nem sikerült: {error:#}"));
                            }
                        }
                    }
                    Some("connect") => {
                        if handle.is_some() {
                            emit_error("Már csatlakozik egy headset. Előbb válaszd le.");
                            continue;
                        }
                        let Some(ble_id) = command["device"].as_str() else {
                            emit_error("Hiányzik a kiválasztott headset azonosítója.");
                            continue;
                        };
                        let Some(device) = devices.iter().find(|device| device.ble_id == ble_id).cloned() else {
                            emit_error("A kiválasztott headset már nem található. Keress újra.");
                            continue;
                        };

                        emit_status("connecting", &format!("Csatlakozás: {}...", device.name));
                        match RawDevice::from_info(device.clone()).connect().await {
                            Ok((stream, device_handle)) => {
                                current_device = Some(device);
                                receiver = Some(stream);
                                handle = Some(device_handle);
                                last_device_state = None;
                                stream_announced = false;
                                emit_status("connecting", "A Bluetooth-kapcsolat létrehozása...");
                            }
                            Err(error) => {
                                emit_error(format!("A headsethez csatlakozás nem sikerült: {error:#}"));
                            }
                        }
                    }
                    Some("disconnect") => {
                        disconnect(&mut receiver, &mut handle).await;
                        current_device = None;
                        last_device_state = None;
                        stream_announced = false;
                        emit_status("disconnected", "A headset lecsatlakoztatva.");
                    }
                    _ => emit_error("Ismeretlen parancs."),
                }
            }
            packet = next_packet(&mut receiver) => {
                match packet {
                    Some(data) => {
                        emit(json!({
                            "type": "data",
                            "data": {
                                "counter": data.counter,
                                "eeg_uv": data.eeg_uv,
                                "contact_quality": data.contact_quality,
                                "signal_quality": data.signal_quality,
                                "battery_percent": data.battery_percent,
                                "timestamp": data.timestamp
                            }
                        }));
                    }
                    None if receiver.is_some() => {
                        receiver = None;
                        handle = None;
                        current_device = None;
                        last_device_state = None;
                        stream_announced = false;
                        emit_status("disconnected", "A headset-kapcsolat lezárult. Részletes hibaüzenetért nézd meg a diagnosztikát.");
                    }
                    None => {}
                }
            }
            _ = state_poll.tick() => {
                if let Some(device_handle) = &handle {
                    let state = device_handle.state().await;
                    if last_device_state != Some(state) {
                        last_device_state = Some(state);
                        match state {
                            DeviceState::Connecting => {
                                emit_status("connecting", "A headset Bluetooth-kapcsolata folyamatban...");
                            }
                            DeviceState::Connected => {
                                emit_status("connecting", "Bluetooth-kapcsolat létrejött; EEG-adatfolyam indítása...");
                            }
                            DeviceState::Streaming => {
                                if !stream_announced {
                                    if let Some(device) = &current_device {
                                        emit(json!({
                                            "type": "connected",
                                            "model": device.model.name(),
                                            "channels": device.model.channels(),
                                            "sampling_rate": device.model.sampling_rate()
                                        }));
                                    }
                                    stream_announced = true;
                                }
                                emit_status("streaming", "Csatlakoztatva, élő EEG-adatfolyam.");
                            }
                            DeviceState::Reconnecting => {
                                emit_status("reconnecting", "A Bluetooth-kapcsolat megszakadt; újracsatlakozás...");
                            }
                            DeviceState::Disconnecting => {
                                emit_status("connecting", "A headset leválasztása...");
                            }
                            DeviceState::Disconnected => {}
                        }
                    }
                }
            }
        }
    }

    disconnect(&mut receiver, &mut handle).await;
}
