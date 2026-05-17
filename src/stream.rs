use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::io::{AsyncRead, AsyncWrite};

/// A bidirectional TCP byte-stream wrapping `wstd::net::TcpStream`.
///
/// Bridges wstd's async-fn-based traits with the `futures::io` poll-based traits
/// required by libp2p's upgrade machinery (Noise, Yamux, …).
///
/// # Implementation notes
///
/// The stream is held in an `Arc` so that independent read and write futures can
/// each hold a clone without cloning the underlying WASI socket handle.
/// Each `poll_read` / `poll_write` call that finds no in-flight future creates
/// one, boxes it as a non-Send `Pin<Box<dyn Future>>`, and drives it on
/// subsequent polls.
///
/// `unsafe impl Send` is provided for the wasm32-wasip2 target, where the
/// runtime is single-threaded and WASI resources (integer resource handles)
/// are trivially safe to "send" across the (non-existent) thread boundary.
pub struct WasiTcpStream {
    #[cfg(target_arch = "wasm32")]
    inner: Arc<wstd::net::TcpStream>,
    #[cfg(target_arch = "wasm32")]
    read_state: ReadState,
    #[cfg(target_arch = "wasm32")]
    write_state: WriteState,
    #[cfg(not(target_arch = "wasm32"))]
    _phantom: std::marker::PhantomData<()>,
}

// SAFETY: wasm32-wasip2 is single-threaded; WASI resource handles are integers
// that are safe to transfer across the non-existent thread boundary.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for WasiTcpStream {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for WasiTcpStream {}

/// Heap-allocated, pinned, non-Send future — the only flavour we need on a
/// single-threaded wasm32 target.
#[cfg(target_arch = "wasm32")]
type WasmBoxFut<T> = Pin<Box<dyn std::future::Future<Output = T>>>;

#[cfg(target_arch = "wasm32")]
enum ReadState {
    Idle,
    Pending(WasmBoxFut<io::Result<(Vec<u8>, usize)>>),
}

#[cfg(target_arch = "wasm32")]
enum WriteState {
    Idle,
    Writing(WasmBoxFut<io::Result<usize>>),
    Flushing(WasmBoxFut<io::Result<()>>),
}

impl WasiTcpStream {
    /// Wrap a connected `wstd::net::TcpStream` in the libp2p-compatible stream shim.
    ///
    /// Consumers typically obtain streams via [`WasiTcpTransport`](crate::WasiTcpTransport), but
    /// you can also wrap a stream you constructed directly with `wstd::net::TcpStream::connect`.
    #[cfg(target_arch = "wasm32")]
    pub fn new(stream: wstd::net::TcpStream) -> Self {
        Self {
            inner: Arc::new(stream),
            read_state: ReadState::Idle,
            write_state: WriteState::Idle,
        }
    }
}

impl AsyncRead for WasiTcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        #[cfg(target_arch = "wasm32")]
        {
            let this = self.get_mut();
            loop {
                match &mut this.read_state {
                    ReadState::Idle => {
                        let stream = Arc::clone(&this.inner);
                        let len = buf.len();
                        // The async block captures `stream` (Arc, 'static) and
                        // `len` (usize, Copy).  The borrow of `&*stream` inside
                        // the block is self-referential within the state machine,
                        // which async/await handles correctly via Pin.
                        let fut: WasmBoxFut<_> = Box::pin(async move {
                            use wstd::io::AsyncRead as _;
                            let mut tmp = vec![0u8; len];
                            let mut s = &*stream;
                            let n = s.read(&mut tmp).await?;
                            Ok((tmp, n))
                        });
                        this.read_state = ReadState::Pending(fut);
                    }
                    ReadState::Pending(fut) => match fut.as_mut().poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok((tmp, n))) => {
                            let to_copy = n.min(buf.len());
                            buf[..to_copy].copy_from_slice(&tmp[..to_copy]);
                            this.read_state = ReadState::Idle;
                            return Poll::Ready(Ok(to_copy));
                        }
                        Poll::Ready(Err(e)) => {
                            this.read_state = ReadState::Idle;
                            return Poll::Ready(Err(e));
                        }
                    },
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (cx, buf);
            Poll::Ready(Err(unsupported()))
        }
    }
}

impl AsyncWrite for WasiTcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        #[cfg(target_arch = "wasm32")]
        {
            let this = self.get_mut();
            loop {
                match &mut this.write_state {
                    WriteState::Idle => {
                        let stream = Arc::clone(&this.inner);
                        let data = buf.to_vec();
                        let fut: WasmBoxFut<_> = Box::pin(async move {
                            use wstd::io::AsyncWrite as _;
                            let mut s = &*stream;
                            s.write(&data).await
                        });
                        this.write_state = WriteState::Writing(fut);
                    }
                    WriteState::Writing(fut) => match fut.as_mut().poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(result) => {
                            this.write_state = WriteState::Idle;
                            return Poll::Ready(result);
                        }
                    },
                    // A flush is in flight; writes must wait.
                    WriteState::Flushing(_) => return Poll::Pending,
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (cx, buf);
            Poll::Ready(Err(unsupported()))
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        #[allow(unused_variables)] cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        #[cfg(target_arch = "wasm32")]
        {
            let this = self.get_mut();
            loop {
                match &mut this.write_state {
                    WriteState::Idle => {
                        let stream = Arc::clone(&this.inner);
                        let fut: WasmBoxFut<_> = Box::pin(async move {
                            use wstd::io::AsyncWrite as _;
                            let mut s = &*stream;
                            s.flush().await
                        });
                        this.write_state = WriteState::Flushing(fut);
                    }
                    // A write is in flight; wait for it before flushing.
                    WriteState::Writing(_) => return Poll::Pending,
                    WriteState::Flushing(fut) => match fut.as_mut().poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(result) => {
                            this.write_state = WriteState::Idle;
                            return Poll::Ready(result);
                        }
                    },
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        // Flush remaining data; the socket is shut down on Drop (wstd's TcpStream Drop impl
        // calls socket.shutdown(Both)).
        self.as_mut().poll_flush(cx)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "WasiTcpStream is only functional on wasm32-wasip2")
}
