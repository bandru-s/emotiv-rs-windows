//! A web interface for an Emotiv headset over the `raw` feature: the same BLE
//! path and decryption `emotiv-raw` uses, in a browser, over the browser's own
//! Web Bluetooth. No Cortex service, no EMOTIV Launcher.
//!
//! Build it with `examples/web/build.sh`, which also lays out a directory you
//! can serve. `examples/web/README.md` has the details.
//!
//! # Shape of it
//!
//! There is no `wasm-bindgen` here, because `webbluetooth-wasm` does not use
//! one: the boundary is plain numbers across a hand-written shim, and this
//! example crosses it the same way.
//!
//! - Out of the page, into this module: `emotiv_connect` and
//!   `emotiv_disconnect` (the two buttons) and `emotiv_poll` (a once-a-second
//!   request for decode statistics).
//! - Out of this module, into the page: the `emotiv` import namespace below —
//!   status text, EEG frames, battery, the clock. `app.js` supplies them.
//! - Everything Bluetooth: `webbluetooth`'s own `webbluetooth` namespace, which
//!   the shim fills in. This file never touches it.
//!
//! # Native builds are empty
//!
//! The target is `wasm32-unknown-unknown`. `cargo build --examples` on a native
//! target still builds this file, so the browser half is behind a `cfg` and
//! what remains natively is an empty library.

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use emotiv::raw::device::RawDeviceHandle;
    use emotiv::raw::{self, DeviceState, RawDevice};

    // What the page provides. (A plain comment, not a doc comment: rustdoc
    // does not document extern blocks.)
    //
    // `#[link(wasm_import_module)]` names the namespace these arrive in, which
    // is what `app.js` passes to `instantiate` as its extra imports. Without
    // it they would land in `env`, where the page supplies nothing, and
    // instantiation would fail on a missing import.
    #[link(wasm_import_module = "emotiv")]
    unsafe extern "C" {
        fn emotiv_status(ptr: *const u8, len: usize);
        fn emotiv_log(level: u32, ptr: *const u8, len: usize);
        fn emotiv_row(index: u32, hz: f64, ptr: *const u8, len: usize);
        fn emotiv_frame(ptr: *const f64, len: usize);
        fn emotiv_quality(battery: f64, signal: u32);
        fn emotiv_active(active: u32);
        fn emotiv_now() -> f64;
        fn emotiv_serial(ptr: *mut u8, capacity: usize) -> usize;
        fn emotiv_stats(received: f64, decoded: f64, failures: f64, ptr: *const u8, len: usize);
    }

    thread_local! {
        // The live connection, for the two exports that act on it. A page has
        // one thread, so a thread-local is simply "the" handle.
        static HANDLE: RefCell<Option<Rc<RawDeviceHandle>>> = const { RefCell::new(None) };
        // Whether the status line is currently saying "reconnecting", so it
        // can be put back once streaming resumes.
        static RECONNECTING: Cell<bool> = const { Cell::new(false) };
    }

    fn current_handle() -> Option<Rc<RawDeviceHandle>> {
        HANDLE.with(|h| h.borrow().clone())
    }

    /// The serial typed into the page, if any. The page writes UTF-8 into the
    /// buffer and returns how many bytes it wrote.
    fn page_serial() -> Option<String> {
        let mut buffer = [0u8; 64];
        let len = unsafe { emotiv_serial(buffer.as_mut_ptr(), buffer.len()) }.min(buffer.len());
        let serial = std::str::from_utf8(&buffer[..len]).ok()?.trim();
        (!serial.is_empty()).then(|| serial.to_owned())
    }

    /// Sends the library's `log` records to the page's console.
    ///
    /// Without this a browser build is silent: `env_logger` is native-only, so
    /// every `info!` in `raw::device` — including the line saying which serial
    /// candidate decryption locked onto — goes nowhere.
    struct PageLog;

    impl log::Log for PageLog {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            let line = format!("{}: {}", record.target(), record.args());
            // `Level` is 1..=5, error through trace; `app.js` maps it to the
            // matching console method.
            unsafe { emotiv_log(record.level() as u32, line.as_ptr(), line.len()) }
        }

        fn flush(&self) {}
    }

    static PAGE_LOG: PageLog = PageLog;

    /// `Date.now()`, by way of the page. `SystemTime::now()` panics on this
    /// target, so the library is handed a clock instead.
    fn page_clock() -> f64 {
        unsafe { emotiv_now() }
    }

    /// Tell the page what is happening, in words it can put on screen.
    fn status(message: &str) {
        // The page copies the bytes out before it returns and never keeps the
        // pointer.
        unsafe { emotiv_status(message.as_ptr(), message.len()) }
    }

    /// Connect to a headset and stream from it. The page's only entry point.
    ///
    /// Has to be reached from a click, not from page load: `requestDevice`
    /// opens the browser's own chooser, and browsers only allow that during a
    /// user gesture. Returning straight away matters for the same reason — the
    /// work goes onto the page's event loop rather than being awaited here, so
    /// the chooser opens inside the window the click grants.
    #[no_mangle]
    pub extern "C" fn emotiv_connect() {
        // Once per page. A second press is an `Err` rather than a panic, which
        // is why the results are discarded.
        let _ = log::set_logger(&PAGE_LOG);
        log::set_max_level(log::LevelFilter::Info);
        raw::set_clock(page_clock);

        // A browser has no tokio runtime to hand this to. `exec` is the
        // executor the BLE calls are already being polled by.
        webbluetooth_wasm::exec::spawn(async {
            // Bracketing the whole task, so the button comes back whether this
            // ended in a disconnection or an error.
            unsafe { emotiv_active(1) };
            if let Err(error) = stream().await {
                // `{:#}` rather than `{}`: the context chain is where the
                // useful half of a BLE failure is written down.
                status(&format!("{error:#}"));
            }
            unsafe { emotiv_active(0) };
        });
    }

    async fn stream() -> anyhow::Result<()> {
        status("Opening the browser's device chooser…");
        let mut info = raw::request_device().await?;
        // The AES key is derived from the serial, which is otherwise inferred
        // from the name and an id that, in a browser, is not the address.
        if let Some(serial) = page_serial() {
            log::info!("Using serial from the page: {serial}");
            info.serial = serial;
        }

        // Declared up front, from the model the name implies. The decryptor
        // may settle on a different model's layout; rows for any extra
        // channels are declared as they first appear.
        let model = info.model;
        let hz = f64::from(model.sampling_rate());
        let names = model.channels();
        let mut declared = 0usize;
        let mut declare_up_to = |count: usize| {
            while declared < count {
                let name = names
                    .get(declared)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| format!("ch{declared}"));
                unsafe { emotiv_row(declared as u32, hz, name.as_ptr(), name.len()) }
                declared += 1;
            }
        };
        declare_up_to(names.len());

        status(&format!("Connecting to {} ({})…", info.name, model));
        let (mut packets, handle) = RawDevice::from_info(info).connect().await?;
        HANDLE.with(|h| *h.borrow_mut() = Some(Rc::new(handle)));

        status("Connected. Waiting for decryption to lock…");
        let mut locked = false;

        // Ends when the headset disconnects: the sending half is dropped with
        // the streaming task, which closes the channel.
        while let Some(data) = packets.recv().await {
            if !locked {
                locked = true;
                status("Streaming.");
            }
            declare_up_to(data.eeg_uv.len());
            // One call per packet carrying every channel, rather than one per
            // sample: 14 channels at 256 Hz would be 3,500 crossings a second.
            unsafe { emotiv_frame(data.eeg_uv.as_ptr(), data.eeg_uv.len()) }
            unsafe {
                emotiv_quality(
                    f64::from(data.battery_percent),
                    u32::from(data.signal_quality),
                )
            }
        }

        HANDLE.with(|h| h.borrow_mut().take());
        status("Disconnected.");
        Ok(())
    }

    /// The Disconnect button. The stream notices at once and closes the link;
    /// `stream` then sees its channel end and reports it.
    #[no_mangle]
    pub extern "C" fn emotiv_disconnect() {
        if let Some(handle) = current_handle() {
            status("Disconnecting…");
            webbluetooth_wasm::exec::spawn(async move {
                let _ = handle.disconnect().await;
            });
        }
    }

    /// Report decode statistics to the page. Called by its once-a-second
    /// timer rather than pushed per packet: before decryption locks there are
    /// no packets to push from, and that is exactly when these numbers matter.
    #[no_mangle]
    pub extern "C" fn emotiv_poll() {
        if let Some(handle) = current_handle() {
            webbluetooth_wasm::exec::spawn(async move {
                // The library reconnects on its own when the link drops; the
                // page only has to say so.
                let reconnecting = handle.state().await == DeviceState::Reconnecting;
                if reconnecting != RECONNECTING.with(Cell::get) {
                    RECONNECTING.with(|r| r.set(reconnecting));
                    status(if reconnecting {
                        "Link lost — reconnecting…"
                    } else {
                        "Streaming."
                    });
                }
                let stats = handle.debug_stats().await;
                let key = stats.active_serial_candidate.unwrap_or_default();
                unsafe {
                    emotiv_stats(
                        stats.received_notifications as f64,
                        stats.decoded_packets as f64,
                        stats.decrypt_failures as f64,
                        key.as_ptr(),
                        key.len(),
                    )
                }
            });
        }
    }
}
