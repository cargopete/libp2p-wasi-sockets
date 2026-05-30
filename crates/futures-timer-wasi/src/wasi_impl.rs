use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use wstd::runtime::{AsyncPollable, WaitFor};

/// A future that resolves after a given duration, driven by
/// `wasi:clocks/monotonic-clock` via the wstd reactor.
///
/// Drop-in replacement for `futures_timer::Delay` on wasm32-wasip2.
pub struct Delay {
    wait_for: WaitFor,
}

// wasm32-wasip2 is single-threaded; asserting Send is safe.
unsafe impl Send for Delay {}
unsafe impl Sync for Delay {}

impl Delay {
    /// Create a new delay that fires after `dur`.
    ///
    /// Must be called from within a `wstd::runtime::block_on` context (i.e.
    /// inside `#[wstd::main]` or any `spawn`'d task).
    pub fn new(dur: Duration) -> Self {
        let ns = dur.as_nanos().min(u64::MAX as u128) as u64;
        let raw = wasip2::clocks::monotonic_clock::subscribe_duration(ns);
        let pollable = AsyncPollable::new(raw);
        Delay {
            wait_for: pollable.wait_for(),
        }
    }

    /// Reset this delay to fire after `dur` from now.
    pub fn reset(&mut self, dur: Duration) {
        let ns = dur.as_nanos().min(u64::MAX as u128) as u64;
        let raw = wasip2::clocks::monotonic_clock::subscribe_duration(ns);
        let pollable = AsyncPollable::new(raw);
        self.wait_for = pollable.wait_for();
    }
}

impl Future for Delay {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // WaitFor is Unpin (plain struct, no PhantomPinned).
        Pin::new(&mut self.get_mut().wait_for).poll(cx)
    }
}

impl fmt::Debug for Delay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delay").finish()
    }
}
