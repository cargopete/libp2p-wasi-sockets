use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::io::{AsyncRead, AsyncWrite};

#[cfg(target_arch = "wasm32")]
use std::net::SocketAddr;
#[cfg(target_arch = "wasm32")]
use wasip2::{
    io::streams::{InputStream, OutputStream},
    sockets::tcp::{ShutdownType, TcpSocket},
};
#[cfg(target_arch = "wasm32")]
use wstd::io::{AsyncInputStream, AsyncOutputStream};

/// A bidirectional TCP byte-stream wrapping raw WASI socket resources.
///
/// Bridges WASI's async I/O streams with the `futures::io` poll-based traits
/// required by libp2p's upgrade machinery (Noise, Yamux, …).
///
/// Obtain instances through [`WasiTcpTransport`](crate::WasiTcpTransport);
/// do not construct directly.
///
/// # Implementation notes
///
/// Each `poll_read` / `poll_write` call that finds no in-flight future creates
/// one, boxes it, and drives it on subsequent polls.  `AsyncInputStream::read`
/// and `AsyncOutputStream::write` take `&self`, so they can be shared across
/// futures without cloning the underlying WASI handle — they are held in Arcs.
///
/// `unsafe impl Send` is provided for the wasm32-wasip2 target, where the
/// runtime is single-threaded and WASI resource handles (integers) are trivially
/// safe to "send" across the non-existent thread boundary.
pub struct WasiTcpStream {
    #[cfg(target_arch = "wasm32")]
    input: Arc<AsyncInputStream>,
    #[cfg(target_arch = "wasm32")]
    output: Arc<AsyncOutputStream>,
    /// Remote peer address, available for inbound connections (accepted by a
    /// listener) and outbound connections (dialled directly).  Retained for
    /// diagnostics / future use; not currently surfaced through a public API.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    peer_addr: Option<SocketAddr>,
    #[cfg(target_arch = "wasm32")]
    read_state: ReadState,
    #[cfg(target_arch = "wasm32")]
    write_state: WriteState,
    /// Kept alive to hold the WASI TCP socket resource open.  Declared last
    /// so it drops after `input`/`output`, whose `InputStream`/`OutputStream`
    /// are children of this socket in the WASI resource model — dropping the
    /// parent while children exist produces a "resource has children" trap.
    #[cfg(target_arch = "wasm32")]
    _socket: Arc<TcpSocket>,
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
    /// Construct a `WasiTcpStream` from raw WASI socket resources.
    ///
    /// # Arguments
    ///
    /// * `socket` — The accepted or connected WASI TCP socket.  Kept alive to
    ///   ensure the connection remains open; shutdown on Drop.
    /// * `input` — The WASI input stream from `accept()` / `finish_connect()`.
    /// * `output` — The WASI output stream from the same.
    /// * `peer_addr` — Remote peer's `SocketAddr`, if known.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn from_raw(
        socket: TcpSocket,
        input: InputStream,
        output: OutputStream,
        peer_addr: Option<SocketAddr>,
    ) -> Self {
        Self {
            input: Arc::new(AsyncInputStream::new(input)),
            output: Arc::new(AsyncOutputStream::new(output)),
            peer_addr,
            read_state: ReadState::Idle,
            write_state: WriteState::Idle,
            _socket: Arc::new(socket),
        }
    }

    /// Returns the remote peer's address, if available.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    pub(crate) fn peer_addr(&self) -> Option<SocketAddr> {
        self.peer_addr
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for WasiTcpStream {
    fn drop(&mut self) {
        // Best-effort graceful shutdown — mirrors wstd's TcpStream::drop.
        let _ = self._socket.shutdown(ShutdownType::Both);
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
                        let stream = Arc::clone(&this.input);
                        let len = buf.len();
                        let fut: WasmBoxFut<_> = Box::pin(async move {
                            let mut tmp = vec![0u8; len];
                            let n = stream.read(&mut tmp).await?;
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
                        let stream = Arc::clone(&this.output);
                        let data = buf.to_vec();
                        let fut: WasmBoxFut<_> = Box::pin(async move { stream.write(&data).await });
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
                        let stream = Arc::clone(&this.output);
                        let fut: WasmBoxFut<_> = Box::pin(async move { stream.flush().await });
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

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Flush remaining data; the socket is shut down on Drop.
        self.as_mut().poll_flush(cx)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "WasiTcpStream is only functional on wasm32-wasip2",
    )
}
