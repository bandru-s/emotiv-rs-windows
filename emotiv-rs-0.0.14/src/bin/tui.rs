//! Real-time EEG chart viewer for Emotiv headsets.
//!
//! # Usage
//!
//! ```bash
//! # Real headset (requires EMOTIV Launcher + API credentials)
//! cargo run --bin emotiv-tui
//!
//! # Direct raw BLE mode (no Cortex API)
//! cargo run --bin emotiv-tui --features raw -- --raw
//! cargo run --bin emotiv-tui --features raw -- --raw --raw-index 0
//! cargo run --bin emotiv-tui --features raw -- --raw --raw-device EPOC_1234
//!
//! # Simulated signal (no hardware or credentials needed)
//! cargo run --bin emotiv-tui --features simulate -- --simulate
//! ```
//!
//! # API credentials
//!
//! Create a Cortex App at <https://www.emotiv.com/my-account/cortex-apps/>
//! to get a Client ID and Client Secret, then export them:
//!
//! ```bash
//! export EMOTIV_CLIENT_ID="your_client_id"
//! export EMOTIV_CLIENT_SECRET="your_client_secret"
//! ```
//!
//! On the first run the EMOTIV Launcher will ask you to approve the app.
//!
//! # Keys (all modes)
//!
//! | Key | Action |
//! |-----|--------|
//! | `Tab` | Cycle views (EEG → Metrics → BandPower [→ SimControl]) |
//! | `+`/`=` | Zoom out (increase µV scale) |
//! | `-` | Zoom in |
//! | `a` | Auto-scale Y axis |
//! | `v` | Toggle smooth overlay |
//! | `p` / `r` | Pause / resume |
//! | `c` | Clear buffers |
//! | `q` / `Esc` | Quit |
//!
//! # Keys (simulate mode — `--features simulate`)
//!
//! | Key | Action |
//! |-----|--------|
//! | `1`–`5` | Brain state: 1=Relaxed 2=Focused 3=Excited 4=Drowsy 5=Meditative |
//! | `b` | Eye blink artifact (200 ms frontal spike) |
//! | `j` | Jaw clench artifact (500 ms temporal EMG burst) |
//! | `m` | Cycle mental command (neutral → push → pull → lift → drop) |
//! | `f` | Cycle facial expression (neutral → smile → surprise → frown → clench) |
//! | `n`/`N` | Noise level ↓/↑ |
//! | `g`/`G` | Signal gain ↓/↑ |

// Native by nature: it draws to a terminal. On wasm32 this binary compiles to an empty
// `main`, so `cargo build --target wasm32-unknown-unknown` builds the whole
// crate instead of failing here.
#[cfg(not(target_arch = "wasm32"))]
#[path = "tui/native.rs"]
mod native;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    native::main()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
