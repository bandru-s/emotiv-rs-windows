//! Direct BLE/USB device connection and data streaming.
//!
//! This module provides raw hardware access to Emotiv headsets via BLE or USB,
//! bypassing the Cortex API and connecting directly to the device.
//!
//! # Features
//!
//! - **Device discovery**: Auto-detect connected Emotiv headsets
//! - **BLE support**: EPOC X, EPOC+, Insight 2
//! - **USB support**: EPOC+ and other USB-connected models
//! - **Decryption**: Full AES-ECB packet decryption (binary-compatible)
//! - **Streaming**: Async data stream via mpsc channel
//! - **Browser**: Builds for `wasm32-unknown-unknown`, over the page's own Web
//!   Bluetooth — use [`request_device`] (a user gesture opens the browser's
//!   picker) and `set_clock` there; see `examples/web`
//!
//! # Usage
//!
//! ```no_run
//! use emotiv::raw::{self, RawDevice};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Discover devices
//!     let devices = raw::discover_devices().await?;
//!     println!("Found {} devices", devices.len());
//!
//!     if let Some(info) = devices.into_iter().next() {
//!         // Connect and stream
//!         let (mut rx, _handle) = RawDevice::from_info(info).connect().await?;
//!         while let Some(data) = rx.recv().await {
//!             println!("EEG samples: {:?}", &data.eeg_uv[..5.min(data.eeg_uv.len())]);
//!         }
//!     }
//!     Ok(())
//! }
//! ```

pub mod decryption;
pub mod device;
pub mod discovery;
pub mod pipeline;
pub mod types;

// The background connection task. Internal: driven through `RawDevice`.
mod stream;

// Spawning, timers and the clock differ between a native runtime and a
// browser event loop. Internal, apart from the browser's clock hook.
mod platform;
#[cfg(target_arch = "wasm32")]
pub use platform::set_clock;

pub use decryption::Decryptor;
pub use device::{
    find_device, select_best_device, DeviceInfo, RawDevice, RawDeviceHandle, ReconnectPolicy,
    StreamDebugStats, TransportType,
};
pub use discovery::request_device;
pub use pipeline::{DecodePipeline, PipelineStats};
pub use types::*;

use anyhow::Result;

/// Discover all connected Emotiv devices (BLE and USB).
pub async fn discover_devices() -> Result<Vec<DeviceInfo>> {
    RawDevice::discover().await
}

/// Discover devices, then connect to the first one that
/// [matches](DeviceInfo::matches) `query` — an address, BLE id, serial or name.
pub async fn connect_device(
    query: &str,
) -> Result<(tokio::sync::mpsc::Receiver<DecryptedData>, RawDeviceHandle)> {
    let devices = discover_devices().await?;
    let info =
        find_device(&devices, query).ok_or_else(|| anyhow::anyhow!("Device not found: {query}"))?;
    RawDevice::from_info(info.clone()).connect().await
}
