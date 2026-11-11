# Raw BLE streaming (`raw` feature)

Connect to an Emotiv headset directly over Bluetooth LE and decrypt its EEG
locally, with no Cortex service, EMOTIV Launcher or API credentials. Runs
natively (macOS, Linux, Windows) and, compiled to `wasm32-unknown-unknown`, in
the browser.

> **Status.** Decryption, lock-on, reassembly and timestamps are covered by
> tests built from synthetic encrypted packets. Their behaviour against real
> headsets has not been validated for every model, and the motion layout is
> unverified (see [Motion](#motion-tentative)). Reports and packet captures
> from real hardware are welcome.

## Quick start

```toml
[dependencies]
emotiv = { version = "0.0.14", features = ["raw"] }
```

```rust
use emotiv::raw::{self, RawDevice};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let devices = raw::discover_devices().await?;
    let info = raw::select_best_device(&devices)
        .ok_or_else(|| anyhow::anyhow!("no headset found"))?
        .clone();

    let (mut rx, handle) = RawDevice::from_info(info).connect().await?;
    while let Some(packet) = rx.recv().await {
        println!(
            "#{:<5} {:.3}s  signal {}  battery {}%  {:?}",
            packet.counter, packet.timestamp, packet.signal_quality,
            packet.battery_percent, &packet.eeg_uv[..5],
        );
    }
    handle.disconnect().await
}
```

**Quit the EMOTIV Launcher first.** A headset accepts one connection at a time.

## Command line

```bash
cargo run --bin emotiv-raw --features raw -- --list               # discover
cargo run --bin emotiv-raw --features raw                          # best match
cargo run --bin emotiv-raw --features raw -- --connect <QUERY>     # name, address, id or serial
cargo run --bin emotiv-raw --features raw -- --serial <SERIAL>     # explicit decryption serial
cargo run --bin emotiv-raw --features raw -- --debug-view          # live receive/decode stats
cargo run --bin emotiv-raw --features raw -- --decode-audit        # decode sanity scoring
cargo run --bin emotiv-tui --features raw -- --raw                 # terminal UI
cargo run --example raw_stream --features raw
```

Environment:

| Variable | Effect |
|---|---|
| `EMOTIV_RAW_SERIAL` | Serial to derive the AES key from, tried before any inferred one |
| `EMOTIV_RAW_SCAN_SECS` | Discovery scan window in seconds (default 8, minimum 3) |
| `RUST_LOG` | e.g. `info` to see lock-on and reconnect messages |

On macOS, `build.rs` embeds `Info.plist` (`NSBluetoothAlwaysUsageDescription`)
into each binary when `raw` is enabled. Without it CoreBluetooth reports
*unauthorized* and never prompts.

## Finding a headset

| Call | Use |
|---|---|
| `raw::discover_devices()` | Scan and return every Emotiv-like device |
| `raw::select_best_device(&devices)` | Pick the auto-connect candidate |
| `raw::find_device(&devices, query)` | Pick by name, address, BLE id, MAC or serial |
| `raw::request_device()` | Open the platform's chooser; **the browser's way in** |
| `DeviceInfo::ble(id, name)` | Describe a known headset without scanning |

Discovery scans with Web Bluetooth filters: Emotiv name prefixes, the vendor
services `00001100`–`00001103-d102-11e1-9b23-00025b00a5a5`, and company id
`0x0422`. It then re-checks each sighting case-insensitively. Model and serial
are inferred from the advertised name (and the address where there is one).

`ble_id` is the platform identifier. On macOS/iOS it is a per-host UUID and
`ble_mac` is `None`, because Apple never exposes the address; elsewhere the id
*is* the address. A headset that is linked to the OS but not advertising is not
discovered.

## Connecting

`RawDevice::connect()` returns at once with a data receiver and a
`RawDeviceHandle`; the connection runs as a background task:

1. **Resolve** the `DeviceInfo` to a granted device: one this process already
   holds, else adopt by id, else scan and fuzzy-match.
2. **Session**: connect (10 s deadline), discover the granted services (15 s),
   subscribe to the data service's notifications, and write the start command
   to the control service. After 5 s of silence it resubscribes and re-sends
   the start command.
3. **Reconnect**: if the link drops after streaming started, retry under the
   `ReconnectPolicy`, keeping the decryption key. The default is 10 attempts,
   1 s doubling to 30 s. Use `RawDevice::with_reconnect(..)` to change it, or
   `ReconnectPolicy::disabled()` to end the stream on the first drop.

The handle reports `state()` (`Connecting`, `Streaming`, `Reconnecting`,
`Disconnected`) and `debug_stats()` (received, decoded, failed, motion packets,
reconnects, the locked key). `disconnect()` stops the task wherever it is and
returns once the link is closed. Dropping the handle leaves the stream running;
dropping the receiver ends it.

## Decryption

### Keys

Each packet is AES-128-ECB encrypted under a key derived from the headset's
**serial**. The serial is not readable over BLE: the Serial Number String
characteristic is on the Web Bluetooth blocklist. So it is inferred:

1. `EMOTIV_RAW_SERIAL`, if set;
2. `DeviceInfo::serial`: from discovery, `with_serial`, or `--serial`;
3. the address, BLE id, MAC and name.

Each is normalized to 12 alphanumerics, and both the first and the last 12
characters of an explicit serial are tried. Every (serial, model) pair becomes
a candidate key, with the inferred model first. There are three key schedules:

| Schedule | Models | Key |
|---|---|---|
| EPOC X | EPOC X | `serial[0..12]` + `"EX10"` |
| v2 | EPOC+, EPOC Flex, Insight 2 | `serial[0,3,7,1,5,11,4,9,2,6,10,8]` + `"1159"` |
| v1 | EPOC, Insight, MN8, Xtrodes | `serial[1], 0x00, serial[2,3,4,8,9,7,10,11,0,6,5]`, `0x54, 0x10, 0x42` |

### Lock-on

Decoding alone can't tell a right key from a wrong one: for 16-bit layouts any
32 bytes "decode" into a full set of channels. The packet counter can. Under
the right key it advances by a constant step; under a wrong key it is noise. A
candidate is **locked** once its counter shows the same non-zero step over four
consecutive packets. The log says:

```
Decryption locked: E50202E9ABCD/EPOC X on 0000…-d102-… (counter step 1)
```

If no candidate confirms within 64 packets, the best decodable one is locked
anyway and flagged **`:unconfirmed`**. The samples are then probably noise, so
set the serial. A confirmed lock that breaks its counter step 64 times in a row
is dropped and the search starts again. A dropped packet breaks the step once;
a wrong key breaks it every time.

### Packet layout (after decryption)

| Bytes | Content |
|---|---|
| 0–1 | Counter, big-endian `u16` |
| 2 … N−2 | EEG samples: 16-bit BE (EPOC X, EPOC+), 24-bit BE (EPOC Flex), packed 14-bit (others) |
| N−1 | Battery: the raw byte maps to 3.0–4.2 V, and 3.2–4.2 V maps to 0–100 % |

Packets are 32 bytes (EPOC family) or 16 bytes (Insight, MN8, Xtrodes). Values
are scaled to µV from the model's physical range. Contact quality is read from
2-bit fields at byte 20 onward, a simplified extraction.

Notifications are reassembled per characteristic. At the default BLE MTU a
32-byte packet arrives as 20 + 12 bytes.

### Timestamps

`DecryptedData::timestamp` is rebuilt from the counter and the model's sampling
rate, anchored to the first packet's arrival. Bluetooth delivery jitter doesn't
reach it, dropped packets leave a gap of the right size, and late packets are
back-dated. When the counter can't explain the elapsed time (more than 0.25 s
ahead or 2 s behind the arrival time), it re-anchors. `receive_time` is the
arrival time.

### Replaying and testing

`raw::DecodePipeline` is the whole path from notification to `DecryptedData`
with no Bluetooth in it. `push_at(uuid, payload, receive_time)` replays a
recording through exactly the code a live connection uses.

## Motion (tentative)

Once the EEG characteristic is locked, the data service's other notifications
are treated as motion: decrypted with the same key and attached to the **next**
EEG packet.

- `DecryptedData::motion`: `[counter, accel_x, accel_y, accel_z, gyro_x,
  gyro_y, gyro_z, mag_x, mag_y, mag_z]` as raw counts, read as a big-endian
  `u16` followed by nine big-endian `i16`.
- `DecryptedData::motion_raw`: the decrypted bytes, unmodified.

**The layout is a guess.** Nothing in this crate has been checked against a
real motion packet. A few seconds of `motion_raw` from a headset held still,
then moved, would be enough to confirm or correct it.

## In the browser

With `raw`, `cargo build --target wasm32-unknown-unknown` builds the
library. Only `types`, `protocol` and `raw` are compiled there. Two
differences:

- **Use `raw::request_device()` from a click handler.** Browsers open their
  device picker for it, and generally don't allow the scanning
  `discover_devices()` relies on.
- **Call `raw::set_clock(..)`** with a function returning `Date.now()`.
  `SystemTime::now()` panics on that target; without a clock, timestamps are
  `0.0`.

Browsers expose an opaque per-origin id rather than the address, so there are
fewer serial candidates than on Linux; the serial field in the example page is
there for that. [`examples/web`](examples/web) is a complete page: Chrome or
Edge, served from `localhost` or HTTPS.

## Dependencies

BLE goes through [`webbluetooth`](https://crates.io/crates/webbluetooth) 0.0.4,
the stack `muse-rs` uses. If you depend on `muse-rs` or `webbluetooth`
directly as well, keep every `webbluetooth` requirement on the same 0.0.x.
Cargo treats 0.0.x releases as incompatible, and two copies means two
`Bluetooth::shared()` sessions.

## Limitations

- **USB:** not implemented; `discover_devices()` returns BLE devices only.
- **Validation:** decoding has not been validated against real hardware for
  every model; contact-quality extraction is simplified.
- **Motion:** layout unverified (above).
- **Discovery:** headsets that are linked but not advertising are not found.

## License

MIT, as the rest of the crate.
