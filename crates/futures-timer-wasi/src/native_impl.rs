use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

struct Inner {
    done: bool,
    waker: Option<Waker>,
}

/// A future that resolves after a given duration.
///
/// On non-WASI targets this spawns a background `thread::sleep` thread, which
/// is equivalent to the upstream `futures-timer` behaviour.
pub struct Delay {
    inner: Arc<Mutex<Inner>>,
}

impl Delay {
    /// Create a new delay that fires after `dur`.
    pub fn new(dur: Duration) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            done: false,
            waker: None,
        }));
        let inner2 = inner.clone();
        std::thread::spawn(move || {
            std::thread::sleep(dur);
            let mut g = inner2.lock().unwrap();
            g.done = true;
            if let Some(w) = g.waker.take() {
                w.wake();
            }
        });
        Delay { inner }
    }

    /// Reset this delay to fire after `dur` from now.
    pub fn reset(&mut self, dur: Duration) {
        *self = Self::new(dur);
    }
}

impl Future for Delay {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut g = self.inner.lock().unwrap();
        if g.done {
            Poll::Ready(())
        } else {
            g.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl fmt::Debug for Delay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delay").finish()
    }
}
