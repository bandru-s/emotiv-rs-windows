//! Raw BLE/USB device streaming CLI.
//!
//! Direct connection to Emotiv headsets via BLE or USB, bypassing the Cortex API.
//!
//! # Usage
//!
//! ```bash
//! # Discover devices
//! cargo run --bin emotiv-raw --features raw -- --list
//!
//! # Connect to a specific device (name, address, BLE id, MAC or serial)
//! cargo run --bin emotiv-raw --features raw -- --connect "<QUERY>"
//!
//! # Connect to first discovered device
//! cargo run --bin emotiv-raw --features raw
//!
//! # Decode audit mode (live decode sanity scoring)
//! cargo run --bin emotiv-raw --features raw -- --decode-audit
//!
//! # Give the serial explicitly if decryption never locks
//! # (equivalent to EMOTIV_RAW_SERIAL=<SERIAL>)
//! cargo run --bin emotiv-raw --features raw -- --serial "<SERIAL>"
//! ```

// Native by nature: it reads stdin and drives a tokio runtime. On wasm32 this binary compiles to an empty
// `main`, so `cargo build --target wasm32-unknown-unknown` builds the whole
// crate instead of failing here.
#[cfg(not(target_arch = "wasm32"))]
#[path = "raw/native.rs"]
mod native;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    native::main()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
