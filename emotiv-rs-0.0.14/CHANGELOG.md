# Changelog

## 0.0.14 — 2026-10-19

The `raw` feature moves from btleplug to [`webbluetooth`](https://crates.io/crates/webbluetooth), the Web Bluetooth stack `muse-rs` uses, and with that runs **in the browser** as well as natively. Decryption gets a rebuilt pipeline that confirms it has the right key instead of assuming it, and connections reconnect on their own. The Cortex client is unchanged.

### Breaking changes
- **MSRV 1.75 → 1.94**, which webbluetooth requires.
- **`DeviceState::Reconnecting`** is a new variant; exhaustive matches need an arm for it.
- **New fields:** `DecryptedData::motion_raw` and `StreamDebugStats::{motion_packets, reconnects}`. Code that builds these structs by listing every field needs the new ones (`DecryptedData` still deserializes without `motion_raw`).
- **Reconnect is on by default.** After a link drop the data receiver stays open while the task retries; use `ReconnectPolicy::disabled()` for the old behaviour.
- **`raw::connect_device` returns the `RawDeviceHandle`** instead of a `RawDevice` clone that couldn't control the stream. It now matches the query against name, address, BLE id, MAC or serial.
- **`raw` no longer pulls in `btleplug`, `hidapi`, `ndarray` or `num`**, and the crate no longer depends on `chrono` or enables `uuid/v4`. None were used, but crates relying on them through feature unification will need to depend on them directly.
- **`tokio` features are split by target**: only `sync` everywhere, `full` on native targets. Native builds get the same set as before.
- **`StreamDebugStats::decrypt_failures` counts every notification that produced no EEG**, not every 50th.

### Added
- **Browser support.** With `raw`, `cargo build --target wasm32-unknown-unknown` builds the library. Only `types`, `protocol` and `raw` are compiled for wasm; the Cortex client is native-only, and the binaries compile to an empty `main` there (their bodies moved to `src/bin/{cli,tui,raw}/native.rs`). Spawning, timers and the clock go through an internal `raw::platform`: tokio and `SystemTime` natively, `webbluetooth-wasm`'s executor and `setTimeout` in the browser.
- **`examples/web`**: a page that streams decrypted EEG over the browser's Web Bluetooth, with no Cortex service. It has connect/disconnect, an optional serial field, and a live received/decoded/failed readout with the locked key. Drawing runs in a worker on an `OffscreenCanvas`. `examples/web/build.sh` builds it.
- **`raw::request_device()`** opens the platform's device chooser (the browser's picker, or the first match natively) and returns an already-granted `DeviceInfo`.
- **`raw::set_clock()`** (wasm only) supplies the clock for timestamps, since `SystemTime::now()` panics on that target.
- **`raw::pipeline::DecodePipeline`** (public): reassembly, key lock-on, motion routing, timestamps and stats, with no Bluetooth in it. `push_at` replays recorded notifications through the same code a live connection uses.
- **Automatic reconnect** with exponential backoff, keeping the locked key: `RawDevice::with_reconnect(ReconnectPolicy { .. })`. The default is 10 attempts, 1 s doubling to 30 s.
- **Motion (tentative layout).** Once the EEG characteristic is locked, the data service's other notifications are decrypted with the same key and attached to the next EEG packet. `DecryptedData::motion` holds `[counter, accel xyz, gyro xyz, mag xyz]` raw counts, and `motion_raw` holds the plaintext. **The layout is unverified against hardware**; the raw bytes are kept so it can be corrected from a capture.
- **Serial override:** the `EMOTIV_RAW_SERIAL` environment variable, `emotiv-raw --serial`, and `DeviceInfo::with_serial`. Both the first and the last 12 characters of a serial are tried.
- **Choosing a device:** `DeviceInfo::ble(id, name)`, `with_serial`, `with_model` and `Default`; `DeviceInfo::matches(query)` and `looks_like_emotiv()`; `raw::select_best_device` and `raw::find_device`. These replace the copies `emotiv-raw` and `emotiv-tui` each carried.
- **`HeadsetModel` implements `FromStr`**, plus `HeadsetModel::from_name`. The inherent `from_str` is deprecated.
- **macOS Bluetooth permission.** With `raw`, `build.rs` embeds `Info.plist` into the binaries; without it, CoreBluetooth silently reports *unauthorized*.

### Changed
- **`DecryptedData::timestamp` is rebuilt from the packet counter** and sampling rate, so Bluetooth delivery jitter no longer shows up. It re-anchors to the receive time when the counter can't explain a gap. `receive_time` is now the arrival time (it was always `0.0`).
- **`RawDeviceHandle::disconnect()`** stops the task wherever it is (scanning, connecting, streaming, or waiting to reconnect) and returns once the link is actually closed. Before, its command channel was dropped on creation, so a disconnect only took effect at the next packet or the 5 s silence timeout.
- **`raw::device` is split by responsibility:** `device` (public types), `discovery` (scan, resolve, identity inference) and an internal `stream` (the connection task). The 0.0.13 paths still work.
- Discovery uses Web Bluetooth scan filters (Emotiv name prefixes, vendor services `0x1100`–`0x1103`, company id `0x0422`). Connecting first uses a device the session already holds, then adopts by id, then scans and fuzzy-matches.

### Fixed
- **Decryption could lock onto a wrong key.** For 16-bit layouts any 32 bytes "decode" into a full set of channels, so the first candidate (first serial guess, inferred model) always won, right or wrong. A candidate now locks only when its decrypted packet counter advances by the same step over four consecutive packets. If nothing confirms within 64 packets, it falls back to the old choice, flagged `:unconfirmed` with a warning. A confirmed lock is dropped and searched again after 64 consecutive counter breaks.
- **Split packets were never reassembled.** Notifications shorter than 20 bytes were dropped before reassembly, so a 32-byte packet arriving as 20 + 12 at the default BLE MTU lost its tail.
- `examples/raw_stream.rs` didn't compile (nested `#[tokio::main]`, invalid format string).
- `emotiv-tui` didn't compile with `--features simulate` (typo `phui` → `phi`).
- The `raw` module doctest referenced a nonexistent `DeviceType`, and `test_key_derivation` asserted the wrong key byte.
- Clippy is clean across all targets and features.

## 0.0.13 — 2026-04-18

### Changed
- Dropped the unused `rand` dependency.

## 0.0.12 — 2026-04-15

### Changed
- Faster builds: `rustls` without default features (`ring`, `logging`, `std`, `tls12` only).

## 0.0.11 — 2026-03-28

### Bugfixes
- **Disconnects from the Cortex service propagate.** `CORTEX_STOP_ALL_STREAMS` (code 0), `CORTEX_CLOSE_SESSION` (code 1), `HEADSET_DISCONNECTED` (102) and `HEADSET_CONNECTION_FAILED` (103) now clear the session, emit `CortexEvent::Disconnected`, and end the WebSocket loop instead of leaving it lingering.

## 0.0.10 — 2026-03-28

### Bugfixes
- `Error` and `Disconnected` events are sent on every failure path: WebSocket connection failure, write failures (auth, responses, commands), and command-channel close.

## 0.0.9 — 2026-03-19

### Bugfixes
- Warnings 102 (`HEADSET_DISCONNECTED`) and 103 (`HEADSET_CONNECTION_FAILED`) emit `CortexEvent::Disconnected` immediately, without waiting for the data watchdog timeout.

## 0.0.8 — 2026-03-19

### Bugfixes
- Failed stream subscriptions (e.g. EEG without a license) are logged and emitted as `CortexEvent::Error` instead of being silently ignored.

## 0.0.7 — 2026-03-19

### Changed
- With `auto_create_session: false`, querying headsets only emits `HeadsetsQueried`; it no longer connects a headset or creates a session.

## 0.0.6 — 2026-03-19

### Added
- `CortexEvent::HeadsetsQueried(Vec<HeadsetInfo>)` and `CortexHandle::query_headsets()`, to enumerate headsets without triggering auto-connect.

## 0.0.5 — 2026-03-19

### Bugfixes
- **Preserve EEG array positions for non-numeric values**: The Cortex API includes string values in the EEG array (e.g. marker data `"0:0:0"`). Previously `filter_map(as_f64)` dropped these, shifting all subsequent array indices and breaking the DataLabels-to-electrode mapping. Now uses `map(unwrap_or(NAN))` to preserve positions, so electrode indices from DataLabels always point to the correct data.

## 0.0.4 — 2026-03-19

### Bugfixes
- **Request ID in error events**: Error events now include the JSON-RPC request ID that triggered the error, making it easier to diagnose which API call failed.
- **Visible WS logging**: `debug_mode` now uses `eprintln!` instead of `log::debug!` so WS message traces are always captured in stderr (and log files), regardless of the log framework configuration.

## 0.0.3 — 2026-03-19

### Bugfixes
- **Prevent re-auth warnings from killing active sessions**: The Cortex service can send `ACCESS_RIGHT_GRANTED` (code 9), `HEADSET_CONNECTED` (code 104), and `HEADSET_SCANNING_FINISHED` (code 142) warnings at any time — including while a session is active. Previously each handler unconditionally triggered `authorize()`, `query_headsets()`, or `refresh_headset_list()`, which could invalidate the current cortex token or create a duplicate session. Now each handler checks the current state first and skips the action if a token or session is already established.

## 0.0.1 — 2026-03-17

Initial release.

### Features
- Full Emotiv Cortex API client over WebSocket (JSON-RPC 2.0)
- Authentication flow: hasAccessRight → requestAccess → authorize
- Headset management: query, connect, disconnect, refresh
- Session lifecycle: create, close
- Data streaming: EEG, motion, device info, performance metrics, band power, mental commands, facial expressions, system events
- Subscribe / unsubscribe to any combination of streams
- Recording: create, stop, export (CSV / EDF)
- Markers: inject, update
- Profile management: query, create, load, unload, save
- BCI training: mental command and facial expression training via sys stream
- Advanced BCI: active actions, sensitivity get/set, brain map, training threshold
- Record management: query records, request download
- Headset clock sync
- Signal simulator for all data types (no hardware needed)
- CLI binary (`emotiv-cli`) with `--simulate` mode
- TUI binary (`emotiv-tui`) with EEG waveforms, metrics bars, band power view
- 8 example scripts mirroring the official cortex-example Python examples
- 69 unit tests, 17 integration tests, 2 doc-tests
