//! Headless CLI: connect to an Emotiv headset and print all events to stdout.
//!
//! # Usage
//!
//! ```bash
//! # Real headset (requires EMOTIV Launcher + API credentials)
//! cargo run --bin emotiv-cli
//!
//! # Simulated signal (no hardware or credentials needed)
//! cargo run --bin emotiv-cli --features simulate -- --simulate
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
//! # Environment variables
//!
//! | Variable | Description |
//! |---|---|
//! | `EMOTIV_CLIENT_ID` | Cortex App client ID (required for real headset) |
//! | `EMOTIV_CLIENT_SECRET` | Cortex App client secret (required for real headset) |
//! | `EMOTIV_DEBUG` | Set to any value to log all WebSocket messages |
//! | `RUST_LOG` | Log verbosity, e.g. `RUST_LOG=debug` |

// Native by nature: it reads environment credentials and talks to the Cortex service over a tokio runtime. On wasm32 this binary compiles to an empty
// `main`, so `cargo build --target wasm32-unknown-unknown` builds the whole
// crate instead of failing here.
#[cfg(not(target_arch = "wasm32"))]
#[path = "bin/cli/native.rs"]
mod native;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    native::main()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
