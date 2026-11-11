//! # emotiv
//!
//! Async Rust client for streaming EEG and BCI data from
//! [Emotiv](https://www.emotiv.com/) headsets, two ways:
//!
//! - **Cortex API** — through the EMOTIV Launcher's
//!   [Cortex service](https://emotiv.gitbook.io/cortex-api/) (JSON-RPC 2.0
//!   over WebSocket). Every stream Cortex offers: EEG, motion, performance
//!   metrics, band power, mental commands, facial expressions, records,
//!   markers, training.
//! - **Raw BLE** *(feature = `raw`)* — straight to the headset over Bluetooth
//!   LE via [`webbluetooth`](https://crates.io/crates/webbluetooth), with the
//!   packet decryption done here. No Launcher, no credentials. Runs natively
//!   and, compiled to `wasm32-unknown-unknown`, in the browser.
//!
//! ## Supported hardware
//!
//! | Model | EEG ch | Cortex | Raw BLE |
//! |---|---|---|---|
//! | EPOC X | 14 | ✓ | ✓ |
//! | EPOC+ | 14 | ✓ | ✓ |
//! | EPOC Flex | 32 | ✓ | ✓ |
//! | Insight / Insight 2 | 5 | ✓ | ✓ |
//! | MN8 | 2 | ✓ | ✓ |
//!
//! Raw BLE decoding is implemented from the protocol and covered by tests with
//! synthetic packets; it has not been validated on every model.
//!
//! ## Cortex quick start
//!
//! You need a **Client ID** and **Client Secret**: install and log into the
//! [EMOTIV Launcher](https://www.emotiv.com/products/emotiv-launcher), create
//! a Cortex App at <https://www.emotiv.com/my-account/cortex-apps/>, and pass
//! the credentials via [`CortexClientConfig`](client::CortexClientConfig) or
//! the `EMOTIV_CLIENT_ID` / `EMOTIV_CLIENT_SECRET` environment variables. The
//! Launcher asks you to approve the app on first connection.
//!
//! ```no_run
//! use emotiv::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = CortexClientConfig {
//!         client_id: std::env::var("EMOTIV_CLIENT_ID")?,
//!         client_secret: std::env::var("EMOTIV_CLIENT_SECRET")?,
//!         ..Default::default()
//!     };
//!     let (mut rx, handle) = CortexClient::new(config).connect().await?;
//!
//!     while let Some(event) = rx.recv().await {
//!         match event {
//!             CortexEvent::SessionCreated(_) => {
//!                 handle.subscribe(&["eeg", "mot", "met", "pow"]).await?;
//!             }
//!             CortexEvent::Eeg(data) => println!("EEG: {:?}", &data.samples[..5]),
//!             CortexEvent::Disconnected => break,
//!             _ => {}
//!         }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! For long-running use, [`ResilientClient`](reconnect::ResilientClient)
//! adds reconnection and health checks on top.
//!
//! ## Raw BLE quick start (feature = `raw`)
//!
//! Quit the EMOTIV Launcher first — a headset takes one connection at a time.
//!
//! ```toml
//! [dependencies]
//! emotiv = { version = "0.0.14", features = ["raw"] }
//! ```
//!
//! ```ignore
//! use emotiv::raw::{self, RawDevice};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let devices = raw::discover_devices().await?;
//!     let info = raw::select_best_device(&devices).expect("no headset found").clone();
//!     let (mut rx, handle) = RawDevice::from_info(info).connect().await?;
//!
//!     while let Some(packet) = rx.recv().await {
//!         println!("{:.3}  {:?}", packet.timestamp, &packet.eeg_uv[..5]);
//!     }
//!     handle.disconnect().await
//! }
//! ```
//!
//! The AES key comes from the headset's serial, which is inferred from what it
//! advertises; if decoding never locks, set `EMOTIV_RAW_SERIAL` or use
//! `DeviceInfo::with_serial`. See the `raw` module and `RAW_FEATURE.md`.
//!
//! ## Simulation (feature = `simulate`)
//!
//! [`simulator::spawn_simulator`] produces synthetic EEG, motion, metrics and
//! band power through the same [`CortexEvent`](types::CortexEvent) channel —
//! no hardware or credentials needed.
//!
//! ## Module overview
//!
//! | Module | Purpose |
//! |---|---|
//! | [`prelude`] | One-line glob import of the most commonly needed Cortex types |
//! | [`client`] | Cortex WebSocket connection, auth flow, and the [`CortexHandle`](client::CortexHandle) command API |
//! | [`reconnect`] | [`ResilientClient`](reconnect::ResilientClient): auto-reconnect and health checks |
//! | [`types`] | Cortex event and data types ([`CortexEvent`](types::CortexEvent), [`EegData`](types::EegData), …) |
//! | [`protocol`] | JSON-RPC request builders and Cortex API constants |
//! | [`config`], [`error`], [`retry`], [`health`] | Configuration, errors, retry and health-check helpers |
//! | [`simulator`] | Signal simulator *(feature = `simulate`)* |
//! | `raw` | Direct BLE: discovery, connection, decryption *(feature = `raw`)* |
//!
//! ## Browser (`wasm32-unknown-unknown`)
//!
//! With `raw`, the crate builds for the browser. Only [`types`], [`protocol`]
//! and `raw` are compiled there — the Cortex client is native by nature — and
//! BLE goes through the page's own Web Bluetooth. `examples/web` is a complete
//! page.

// The crate docs above link to the Cortex modules, which do not exist on wasm.
// docs.rs builds for Linux, where every link resolves.
#![cfg_attr(target_arch = "wasm32", allow(rustdoc::broken_intra_doc_links))]

// The Cortex client is a WebSocket to the Cortex service on the local machine,
// over a tokio runtime — native by nature. In a browser only the data types and
// the `raw` BLE path are built.
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod error;
#[cfg(not(target_arch = "wasm32"))]
pub mod health;
pub mod protocol;
#[cfg(not(target_arch = "wasm32"))]
pub mod reconnect;
#[cfg(not(target_arch = "wasm32"))]
pub mod retry;
#[cfg(all(feature = "simulate", not(target_arch = "wasm32")))]
pub mod simulator;
pub mod types;
#[cfg(feature = "raw")]
pub mod raw;

/// Convenience re-exports for downstream crates.
///
/// ```
/// use emotiv::prelude::*;
/// ```
///
/// This brings in [`CortexClient`](client::CortexClient),
/// [`CortexClientConfig`](client::CortexClientConfig),
/// [`CortexHandle`](client::CortexHandle), all event/data types, and the
/// stream-name constants (`STREAM_EEG`, `STREAM_MOT`, …).
#[cfg(not(target_arch = "wasm32"))]
pub mod prelude {
    pub use crate::client::{CortexClient, CortexClientConfig, CortexHandle};
    pub use crate::config::CortexConfig;
    pub use crate::error::{CortexError, CortexResult};
    pub use crate::reconnect::{ConnectionEvent, ResilientClient};
    pub use crate::types::*;
    pub use crate::protocol::{
        EEG_FREQUENCY,
        STREAM_EEG, STREAM_MOT, STREAM_DEV, STREAM_MET, STREAM_POW,
        STREAM_COM, STREAM_FAC, STREAM_SYS,
    };
}
