//! Spawning, timers and the wall clock, chosen per target.
//!
//! The raw client asks a runtime for four things: run a task in the
//! background, sleep, put a deadline on a future, and tell the time. Natively
//! these are tokio and `SystemTime`.
//!
//! In a browser none of them are. Tokio's timer is not implemented for
//! `wasm32-unknown-unknown` — tokio's own suite marks building a runtime with
//! `time` enabled `#[should_panic]` there — and its multi-threaded scheduler has
//! no threads to run on. `SystemTime::now()` panics outright on that target.
//!
//! What a page does have is an event loop, and `webbluetooth-wasm` already
//! speaks to it: `exec::spawn` polls a task whenever something wakes it, and
//! the shim's `SET_TIMEOUT` operation is `setTimeout` reached over the same
//! request/settle path as every BLE call. The shim has no clock operation, so
//! the page supplies one through [`set_clock`].
//!
//! Both implementations present the same functions, so the call sites in
//! [`crate::raw::device`] read identically on either target.

use std::future::Future;
use std::time::Duration;

/// A deadline passed before the future produced a value.
///
/// Its own type rather than `tokio::time::error::Elapsed`, so a wasm build need
/// not enable tokio's `time` feature — the feature whose mere presence panics
/// there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline elapsed")
    }
}

impl std::error::Error for Elapsed {}

// ── Native ────────────────────────────────────────────────────────────────────

/// Run `task` in the background, discarding its handle.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn(task: impl Future<Output = ()> + Send + 'static) {
    tokio::spawn(task);
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| Elapsed)
}

/// Seconds since the Unix epoch.
#[cfg(not(target_arch = "wasm32"))]
pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ── Browser ───────────────────────────────────────────────────────────────────

/// Run `task` on the page's event loop.
///
/// No `Send` bound, and none to add: a page has one thread, and the Web
/// Bluetooth objects these tasks hold are not `Send` to begin with.
#[cfg(target_arch = "wasm32")]
pub fn spawn(task: impl Future<Output = ()> + 'static) {
    webbluetooth_wasm::exec::spawn(task);
}

#[cfg(target_arch = "wasm32")]
pub async fn sleep(duration: Duration) {
    use webbluetooth_wasm::codec::Writer;
    use webbluetooth_wasm::host::{call, Op};

    // The shim reads one `u32` of milliseconds, then settles the call from a
    // `setTimeout`. Saturating rather than truncating: a wrapped delay is a
    // sleep that returns immediately.
    let ms = u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
    let mut message = Writer::new();
    message.u32(ms);

    // An error here means the page never called back. There is no recovery
    // finer than carrying on: the caller asked to wait, not to hear about the
    // host.
    let _ = call(Op::SetTimeout, &message.finish()).await;
}

#[cfg(target_arch = "wasm32")]
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    use webbluetooth::future::{select, Either};

    // `select` drops the loser, which for the timer side means `Pending`'s
    // `Drop` deregisters the token. A `setTimeout` that has already been
    // scheduled still fires, and settles a token nobody is waiting on.
    let future = std::pin::pin!(future);
    let deadline = std::pin::pin!(sleep(duration));

    match select(future, deadline).await {
        Either::Left((value, _)) => Ok(value),
        Either::Right(((), _)) => Err(Elapsed),
    }
}

#[cfg(target_arch = "wasm32")]
static CLOCK: std::sync::OnceLock<fn() -> f64> = std::sync::OnceLock::new();

/// Install the wall clock the browser build stamps packets with.
///
/// `clock` returns **milliseconds** since the Unix epoch — what `Date.now()`
/// gives a page. Call once, before connecting; later calls are ignored.
/// Without it every [`DecryptedData::timestamp`](crate::raw::DecryptedData)
/// is `0.0`, rather than the panic `SystemTime::now()` would be.
#[cfg(target_arch = "wasm32")]
pub fn set_clock(clock: fn() -> f64) {
    let _ = CLOCK.set(clock);
}

/// Seconds since the Unix epoch, from the clock given to [`set_clock`].
#[cfg(target_arch = "wasm32")]
pub fn now_secs() -> f64 {
    CLOCK.get().map_or(0.0, |clock| clock() / 1000.0)
}
