# Web interface

The `raw` feature, running in a browser over the browser's own Web Bluetooth.
Connect a headset from a button, watch the channels scroll — no Cortex service,
no EMOTIV Launcher.

Nothing is re-implemented here. Device matching, packet reassembly, serial
candidate search and AES decryption are the `emotiv::raw` code `emotiv-raw`
runs, compiled to `wasm32-unknown-unknown`. This example is a canvas and some
glue. The layout is borrowed from `muse-rs`'s web example.

## Running it

```sh
examples/web/build.sh
python3 -m http.server --directory examples/web/dist 8000
```

Then open <http://localhost:8000/> and press **Connect a headset**.

Requirements that are not this example's choice to make:

- **Chrome or Edge.** Safari and Firefox do not implement Web Bluetooth. The
  page says so rather than failing quietly.
- **`localhost` or HTTPS.** Web Bluetooth needs a secure context, so a
  `file://` URL will not do — hence the server above.
- **Nothing else connected to the headset.** Quit the EMOTIV Launcher (or
  anything running Cortex) first; a headset takes one connection at a time.

You will also need the `wasm32-unknown-unknown` target:

```sh
rustup target add wasm32-unknown-unknown
```

## What `build.sh` produces

`dist/` is generated and git-ignored:

| File | Where it comes from |
|---|---|
| `web.wasm` | `cargo build --example web --features raw --target wasm32-unknown-unknown` |
| `index.html`, `app.js`, `render.js` | copied from this directory |
| `webbluetooth.js` | copied out of the resolved `webbluetooth-wasm` crate |

The shim is deliberately **not** vendored. It is the other half of the module's
ABI, so it is taken from whichever `webbluetooth-wasm` the lock file resolved to
and cannot drift out of step with the `.wasm` beside it.

## Two threads

| | Main thread (`app.js`) | Worker (`render.js`) |
|---|---|---|
| Web Bluetooth + the wasm module | ✓ | |
| Ring buffers, autoscaling, every canvas call | | ✓ |
| Per-second DOM writes (status, battery, rate) | ✓ | |

`navigator.bluetooth` exists on `Window` and not in workers, and
`requestDevice` needs a user gesture, so the radio stays on the main thread. The
drawing — fourteen traces redrawn every frame — moves to a worker through
`transferControlToOffscreen()`. Samples cross once per animation frame, batched.

## The interface

There is no `wasm-bindgen`. `webbluetooth-wasm` does not use one, and this
example crosses the boundary the same way, so the whole interface fits in a
list.

**The module exports** (`examples/web/lib.rs`)

- `emotiv_connect()` / `emotiv_disconnect()` — the two buttons.
- `emotiv_poll()` — the page's once-a-second request for decode statistics.
- `wbt_alloc`, `wbt_free`, `wbt_settle`, `wbt_event`, `memory` — from
  `webbluetooth-wasm`; how the page answers a request.

**The module imports**

- `emotiv.*` — this example's own:
  - `emotiv_row(index, hz, name)` — a channel exists, with its name and rate
  - `emotiv_frame(ptr, len)` — one decrypted packet, `len` f64 µV values
  - `emotiv_quality(battery, signal)` — battery % and 0–4 signal quality,
    per packet; the page shows the latest once a second
  - `emotiv_stats(received, decoded, failed, key)` — the answer to `emotiv_poll`
  - `emotiv_serial(buffer, capacity)` — the page writes the optional serial
    field into the module's buffer and returns its length
  - `emotiv_status`, `emotiv_active`, `emotiv_log`
  - `emotiv_now()` — `Date.now()`, see below
- `webbluetooth.wbt_call` — every Bluetooth operation, through one import.

Three details that are easy to get wrong:

**Connecting has to come from a click.** `raw::request_device()` opens the
browser's chooser, which browsers only allow during a user gesture.
`emotiv_connect` spawns the work and returns immediately, so the chooser opens
while the gesture is still live. (`raw::discover_devices()` scans, which
browsers do not generally allow — `request_device` is the browser's way in.)

**The page supplies the clock.** `SystemTime::now()` panics on
`wasm32-unknown-unknown`, and every `DecryptedData` carries a timestamp. The
module calls `raw::set_clock(...)` with a function that asks the page for
`Date.now()`. Without it, timestamps are `0.0` rather than a panic.

**Decryption has to find the serial.** The AES key comes from the headset's
serial, which `raw` infers from the advertised name and the device id. A
browser hands out an opaque per-origin id rather than the Bluetooth address, so
there are fewer candidates than on Linux — the same situation as macOS. The
**Decode** row shows it: notifications counted under *rx* with nothing under
*ok* means no candidate key opens them. If that persists, type the headset's
serial into the field (it is remembered across reloads) and reconnect. The
**Key** row shows the serial/model candidate once decryption locks; a
`:unconfirmed` suffix means no candidate's packet counter checked out and the
traces are probably noise. If the link drops, the library reconnects on its
own and the status line says so.

## Borrowing this for your own page

1. **A browser has no tokio runtime.** The library routes its own spawning and
   timers (`src/raw/platform.rs`), but to spawn *your* top-level task use
   `webbluetooth_wasm::exec::spawn`, as `emotiv_connect` does.
2. **Install a `log` drain and a clock** before connecting: `PageLog` and
   `raw::set_clock` in `lib.rs`.
