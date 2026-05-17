use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use libp2p_core::multiaddr::Multiaddr;
use libp2p_core::transport::{DialOpts, ListenerId, TransportError, TransportEvent};
use libp2p_core::Transport;
use tracing::warn;

use crate::error::Error;
use crate::multiaddr::{multiaddr_to_socketaddr, socketaddr_to_multiaddr};
use crate::stream::WasiTcpStream;

/// Configuration for [`WasiTcpTransport`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Disable Nagle's algorithm. Defaults to `true`.
    pub nodelay: bool,
    /// TCP keep-alive interval. `None` disables keep-alive.
    pub keep_alive: Option<Duration>,
    /// Listen backlog passed to `wasi:sockets/tcp.set-listen-backlog-size`. Defaults to 128.
    pub listen_backlog: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nodelay: true,
            keep_alive: None,
            listen_backlog: 128,
        }
    }
}

// ── wasm32-wasip2 implementation ─────────────────────────────────────────────

/// Non-Send box future — sufficient for a single-threaded wasm32 runtime.
#[cfg(target_arch = "wasm32")]
type WasmBoxFut<T> = Pin<Box<dyn std::future::Future<Output = T>>>;

/// State machine for a single listener identified by its [`ListenerId`].
#[cfg(target_arch = "wasm32")]
struct ListenerState {
    bind_addr: std::net::SocketAddr,
    /// The bound listener, available once the bind future resolves.
    listener: Option<Arc<wstd::net::TcpListener>>,
    /// In-flight bind future.
    bind_future: Option<WasmBoxFut<std::io::Result<wstd::net::TcpListener>>>,
    /// In-flight accept future.
    accept_future: Option<WasmBoxFut<std::io::Result<wstd::net::TcpStream>>>,
    /// Whether we have emitted a `NewAddress` event for this listener.
    /// Also used as a sentinel: set back to `false` after emitting `AddressExpired`
    /// so the next `poll` knows to emit `ListenerClosed`.
    announced: bool,
    /// Set by `remove_listener`; causes `poll` to emit `AddressExpired` (if
    /// `announced`) followed by `ListenerClosed`, then drop the entry.
    closing: bool,
}

/// A libp2p transport backed by `wasi:sockets/tcp`.
///
/// # Host requirements
///
/// The WASI host must grant network access to the component.  Under Wasmtime,
/// pass `-S inherit-network` (or `--wasi inherit-network`).  Without it, all
/// dials fail with [`Error::AccessDenied`] and listeners cannot be bound.
pub struct WasiTcpTransport {
    #[allow(dead_code)] // applied in M1 when nodelay/keep_alive socket options are set
    config: Config,
    #[cfg(target_arch = "wasm32")]
    listeners: HashMap<ListenerId, ListenerState>,
    #[cfg(not(target_arch = "wasm32"))]
    _phantom: std::marker::PhantomData<()>,
}

// SAFETY: wasm32-wasip2 is single-threaded; WASI resource handles are safe to
// "send" across the non-existent thread boundary.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for WasiTcpTransport {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for WasiTcpTransport {}

impl WasiTcpTransport {
    /// Create a transport with the given [`Config`].
    pub fn new(config: Config) -> Self {
        Self {
            config,
            #[cfg(target_arch = "wasm32")]
            listeners: HashMap::new(),
            #[cfg(not(target_arch = "wasm32"))]
            _phantom: std::marker::PhantomData,
        }
    }
}

impl Default for WasiTcpTransport {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Transport for WasiTcpTransport {
    type Output = WasiTcpStream;
    type Error = Error;
    /// The upgrade is immediate: the accepted stream is already a connected
    /// byte-stream; no further handshake is required at the transport layer.
    type ListenerUpgrade = futures::future::Ready<Result<Self::Output, Self::Error>>;
    /// A boxed, non-Send future — adequate for a single-threaded wasm32 executor.
    #[cfg(target_arch = "wasm32")]
    type Dial = WasmBoxFut<Result<Self::Output, Self::Error>>;
    #[cfg(not(target_arch = "wasm32"))]
    type Dial = futures::future::Pending<Result<Self::Output, Self::Error>>;

    fn listen_on(
        &mut self,
        id: ListenerId,
        addr: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        let sock_addr = multiaddr_to_socketaddr(&addr).map_err(TransportError::Other)?;

        #[cfg(target_arch = "wasm32")]
        {
            let addr_str = sock_addr.to_string();
            let bind_fut: WasmBoxFut<std::io::Result<wstd::net::TcpListener>> =
                Box::pin(async move { wstd::net::TcpListener::bind(&addr_str).await });

            self.listeners.insert(
                id,
                ListenerState {
                    bind_addr: sock_addr,
                    listener: None,
                    bind_future: Some(bind_fut),
                    accept_future: None,
                    announced: false,
                    closing: false,
                },
            );
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (id, sock_addr);
        }

        Ok(())
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(state) = self.listeners.get_mut(&id) {
                state.closing = true;
                true
            } else {
                false
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = id;
            false
        }
    }

    fn dial(
        &mut self,
        addr: Multiaddr,
        _opts: DialOpts,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        let sock_addr = multiaddr_to_socketaddr(&addr).map_err(TransportError::Other)?;
        let _ = &sock_addr; // used below only on wasm32

        #[cfg(target_arch = "wasm32")]
        {
            let dial_fut: WasmBoxFut<Result<WasiTcpStream, Error>> =
                Box::pin(async move {
                    wstd::net::TcpStream::connect(sock_addr)
                        .await
                        .map(WasiTcpStream::new)
                        .map_err(|e| {
                            if e.kind() == std::io::ErrorKind::PermissionDenied {
                                warn!(
                                    "Network capability denied — pass `-S inherit-network` \
                                     to wasmtime to grant the component network access."
                                );
                                Error::AccessDenied
                            } else {
                                Error::Io(e)
                            }
                        })
                });
            return Ok(dial_fut);
        }

        #[cfg(not(target_arch = "wasm32"))]
        Err(TransportError::Other(Error::UnsupportedMultiaddr(addr)))
    }

    fn poll(
        self: Pin<&mut Self>,
        #[allow(unused_variables)] cx: &mut Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        #[cfg(target_arch = "wasm32")]
        {
            let this = self.get_mut();
            let ids: Vec<ListenerId> = this.listeners.keys().cloned().collect();

            for id in ids {
                let state = this.listeners.get_mut(&id).unwrap();

                // ── Phase 0: handle closing listeners ─────────────────────────
                //
                // Sequence: AddressExpired (if previously announced) → ListenerClosed.
                // We re-use `announced` as the "AddressExpired not yet sent" flag:
                // after emitting AddressExpired we set it to false so the next poll
                // emits ListenerClosed and removes the entry.
                if state.closing {
                    state.bind_future = None;
                    state.accept_future = None;
                    if state.announced {
                        let addr = state
                            .listener
                            .as_ref()
                            .and_then(|l| l.local_addr().ok())
                            .map(socketaddr_to_multiaddr)
                            .unwrap_or_else(|| socketaddr_to_multiaddr(state.bind_addr));
                        state.announced = false;
                        return Poll::Ready(TransportEvent::AddressExpired {
                            listener_id: id,
                            listen_addr: addr,
                        });
                    }
                    // AddressExpired already sent (or was never announced).
                    let _ = state; // end the mutable borrow before remove
                    this.listeners.remove(&id);
                    return Poll::Ready(TransportEvent::ListenerClosed {
                        listener_id: id,
                        reason: Ok(()),
                    });
                }

                // ── Phase 1: drive the bind future ────────────────────────────
                if let Some(ref mut bind_fut) = state.bind_future {
                    match bind_fut.as_mut().poll(cx) {
                        Poll::Pending => continue,
                        Poll::Ready(Err(e)) => {
                            state.bind_future = None;
                            let err = if e.kind() == std::io::ErrorKind::PermissionDenied {
                                Error::AccessDenied
                            } else {
                                Error::Io(e)
                            };
                            return Poll::Ready(TransportEvent::ListenerError {
                                listener_id: id,
                                error: err,
                            });
                        }
                        Poll::Ready(Ok(listener)) => {
                            let local_addr = listener
                                .local_addr()
                                .map(socketaddr_to_multiaddr)
                                .unwrap_or_else(|_| socketaddr_to_multiaddr(state.bind_addr));
                            state.listener = Some(Arc::new(listener));
                            state.bind_future = None;
                            state.announced = true;
                            return Poll::Ready(TransportEvent::NewAddress {
                                listener_id: id,
                                listen_addr: local_addr,
                            });
                        }
                    }
                }

                // ── Phase 2: accept loop ───────────────────────────────────────
                let Some(listener_arc) = state.listener.as_ref().map(Arc::clone) else {
                    continue;
                };

                if state.accept_future.is_none() {
                    let listener = Arc::clone(&listener_arc);
                    state.accept_future = Some(Box::pin(async move {
                        use wstd::iter::AsyncIterator as _;
                        listener
                            .incoming()
                            .next()
                            .await
                            .unwrap_or_else(|| {
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::BrokenPipe,
                                    "listener closed",
                                ))
                            })
                    }));
                }

                if let Some(ref mut accept_fut) = state.accept_future {
                    match accept_fut.as_mut().poll(cx) {
                        Poll::Pending => {}
                        Poll::Ready(Err(e)) => {
                            state.accept_future = None;
                            return Poll::Ready(TransportEvent::ListenerError {
                                listener_id: id,
                                error: Error::Io(e),
                            });
                        }
                        Poll::Ready(Ok(tcp_stream)) => {
                            state.accept_future = None;
                            let local_addr = listener_arc
                                .local_addr()
                                .map(socketaddr_to_multiaddr)
                                .unwrap_or_else(|_| socketaddr_to_multiaddr(state.bind_addr));
                            // send_back_addr: wstd's TcpStream::peer_addr() returns a debug
                            // string (not a SocketAddr).  For v0.1.0 we use the listen addr as a
                            // placeholder.  Tracking issue: add proper peer-addr extraction.
                            let send_back_addr = local_addr.clone();
                            let wasi_stream = WasiTcpStream::new(tcp_stream);
                            return Poll::Ready(TransportEvent::Incoming {
                                listener_id: id,
                                upgrade: futures::future::ready(Ok(wasi_stream)),
                                local_addr,
                                send_back_addr,
                            });
                        }
                    }
                }
            }

            Poll::Pending
        }

        #[cfg(not(target_arch = "wasm32"))]
        Poll::Pending
    }
}
