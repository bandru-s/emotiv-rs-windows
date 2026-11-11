fn main() {
    // ── macOS: embed Info.plist so CoreBluetooth grants Bluetooth access ──────
    //
    // On macOS, CBCentralManager silently refuses to scan (state stays
    // "unauthorised") unless the running binary has an embedded Info.plist
    // containing NSBluetoothAlwaysUsageDescription.
    //
    // A bare `cargo run` binary has no app bundle, so the plist is linked into
    // the `__TEXT,__info_plist` section of the Mach-O via `-sectcreate`;
    // macOS reads that section exactly as it would a bundle's Info.plist.
    //
    // Only with `raw`: the Cortex client talks to a local WebSocket and never
    // touches Bluetooth.
    //
    // `CARGO_CFG_TARGET_OS` reflects the *target* (not the host), so
    // cross-compilation to macOS is handled correctly too.
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_RAW");

    let is_macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let raw = std::env::var_os("CARGO_FEATURE_RAW").is_some();
    if is_macos && raw {
        let dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set by Cargo");
        let plist = format!("{dir}/Info.plist");

        // Each `cargo:rustc-link-arg` appends one argument to the final linker
        // invocation:  ld … -sectcreate __TEXT __info_plist /path/Info.plist …
        println!("cargo:rustc-link-arg=-sectcreate");
        println!("cargo:rustc-link-arg=__TEXT");
        println!("cargo:rustc-link-arg=__info_plist");
        println!("cargo:rustc-link-arg={plist}");
    }
}
