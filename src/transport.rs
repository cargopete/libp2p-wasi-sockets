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
use crate::multiaddr::{
    multiaddr_to_dial_target, multiaddr_to_socketaddr, socketaddr_to_multiaddr, DialTarget,
};
use crate::stream::WasiTcpStream;

#[cfg(target_arch = "wasm32")]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

#[cfg(target_arch = "wasm32")]
use wasip2::sockets::{
    instance_network::instance_network,
    network::{ErrorCode, IpAddressFamily, IpSocketAddress, Ipv4SocketAddress, Ipv6SocketAddress},
    tcp::TcpSocket,
    tcp_create_socket::create_tcp_socket,
};
#[cfg(target_arch = "wasm32")]
use wstd::runtime::AsyncPollable;

/// Configuration for [`WasiTcpTransport`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Disable Nagle's algorithm.
    ///
    /// **Note**: TCP_NODELAY is not part of the WASI 0.2 sockets spec; this
    /// field is accepted for API compatibility but has no effect at runtime.
    pub nodelay: bool,
    /// TCP keep-alive idle time.  `None` disables keep-alive.
    ///
    /// Applied via `wasi:sockets/tcp.set-keep-alive-enabled` +
    /// `set-keep-alive-idle-time`.  Accepted sockets inherit keep-alive
    /// settings from the listener socket per the WASI spec.
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

/// Non-Send box future used internally during accept/bind sequencing.
#[cfg(target_arch = "wasm32")]
type WasmBoxFut<T> = Pin<Box<dyn std::future::Future<Output = T>>>;

/// Thin `Send` wrapper for a `WasmBoxFut`.
///
/// wasm32-wasip2 is single-threaded — WASI resource handles (plain integers) are
/// trivially safe to "send" across the non-existent thread boundary.  The `Send`
/// bound on `Transport::Dial` is needed for `Transport::boxed()` / the libp2p
/// Swarm, so we must assert it here.
#[cfg(target_arch = "wasm32")]
pub struct SendDialFut<T>(WasmBoxFut<T>);

#[cfg(target_arch = "wasm32")]
// SAFETY: wasm32-wasip2 is single-threaded; no thread ever borrows the future
// concurrently.
unsafe impl<T> Send for SendDialFut<T> {}

#[cfg(target_arch = "wasm32")]
impl<T> std::future::Future for SendDialFut<T> {
    type Output = T;
    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<T> {
        // SAFETY: we do not move `self.0` out of its pin.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}

/// A bound, listening raw WASI TCP socket.
#[cfg(target_arch = "wasm32")]
struct RawListener {
    socket: TcpSocket,
    /// Resolved local address, including the ephemeral port if 0 was requested.
    local_addr: SocketAddr,
}

#[cfg(target_arch = "wasm32")]
unsafe impl Send for RawListener {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for RawListener {}

/// State machine for a single listener identified by its [`ListenerId`].
#[cfg(target_arch = "wasm32")]
struct ListenerState {
    bind_addr: SocketAddr,
    /// The bound listener, available once the bind+listen future resolves.
    listener: Option<Arc<RawListener>>,
    /// In-flight bind+listen future.
    bind_future: Option<WasmBoxFut<std::io::Result<RawListener>>>,
    /// In-flight accept future.
    accept_future: Option<WasmBoxFut<std::io::Result<(WasiTcpStream, Option<SocketAddr>)>>>,
    /// Whether we have emitted a `NewAddress` event for this listener.
    /// Set back to `false` after emitting `AddressExpired` so the next `poll`
    /// knows to emit `ListenerClosed` and remove the entry.
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
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
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
    /// A boxed future that is `Send` via an `unsafe` wrapper (wasm32 is
    /// single-threaded, so the assertion is always safe in practice).  The
    /// `Send` bound is required by `Transport::boxed()` used by the libp2p Swarm.
    #[cfg(target_arch = "wasm32")]
    type Dial = SendDialFut<Result<Self::Output, Self::Error>>;
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
            let listen_backlog = self.config.listen_backlog;
            let keep_alive = self.config.keep_alive;
            let family = if sock_addr.is_ipv4() {
                IpAddressFamily::Ipv4
            } else {
                IpAddressFamily::Ipv6
            };
            let wasi_bind_addr = sockaddr_to_wasi(sock_addr);

            let bind_fut: WasmBoxFut<std::io::Result<RawListener>> = Box::pin(async move {
                let socket = create_tcp_socket(family).map_err(wasi_io_err)?;
                socket.set_listen_backlog_size(listen_backlog as u64).ok();
                // Set keep-alive on the listener; accepted sockets inherit it per WASI spec.
                if let Some(ka) = keep_alive {
                    let _ = socket.set_keep_alive_enabled(true);
                    let _ = socket.set_keep_alive_idle_time(ka.as_nanos() as u64);
                }
                let network = instance_network();
                socket.start_bind(&network, wasi_bind_addr).map_err(|e| {
                    if matches!(e, ErrorCode::AccessDenied) {
                        warn!(
                            "Network capability denied — pass `-S inherit-network` \
                             to wasmtime to grant the component network access."
                        );
                        std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "network access denied",
                        )
                    } else {
                        wasi_io_err(e)
                    }
                })?;
                AsyncPollable::new(socket.subscribe()).wait_for().await;
                socket.finish_bind().map_err(wasi_io_err)?;
                socket.start_listen().map_err(wasi_io_err)?;
                AsyncPollable::new(socket.subscribe()).wait_for().await;
                socket.finish_listen().map_err(wasi_io_err)?;
                let local_addr = socket
                    .local_address()
                    .map(wasi_sockaddr_to_std)
                    .map_err(wasi_io_err)?;
                Ok(RawListener { socket, local_addr })
            });

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
        #[cfg(target_arch = "wasm32")]
        {
            let keep_alive = self.config.keep_alive;
            let target = multiaddr_to_dial_target(&addr).map_err(TransportError::Other)?;
            let dial_fut: WasmBoxFut<Result<WasiTcpStream, Error>> = Box::pin(async move {
                // Resolve DNS if needed.
                let sock_addr = match target {
                    DialTarget::Addr(sa) => sa,
                    DialTarget::Dns { host, port, family } => {
                        crate::dns::resolve_host(&host, port, family).await?
                    }
                };

                if sock_addr.is_ipv6() {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "IPv6 TCP is not yet supported (wstd limitation)",
                    )));
                }

                let socket =
                    create_tcp_socket(IpAddressFamily::Ipv4).map_err(|e| match e {
                        ErrorCode::AccessDenied => {
                            warn!(
                                "Network capability denied — pass `-S inherit-network` \
                                 to wasmtime to grant the component network access."
                            );
                            Error::AccessDenied
                        }
                        _ => Error::Io(wasi_io_err(e)),
                    })?;

                if let Some(ka) = keep_alive {
                    let _ = socket.set_keep_alive_enabled(true);
                    let _ = socket.set_keep_alive_idle_time(ka.as_nanos() as u64);
                }

                let network = instance_network();
                let wasi_remote = sockaddr_to_wasi(sock_addr);
                socket.start_connect(&network, wasi_remote).map_err(|e| match e {
                    ErrorCode::AccessDenied => {
                        warn!(
                            "Network capability denied — pass `-S inherit-network` \
                             to wasmtime to grant the component network access."
                        );
                        Error::AccessDenied
                    }
                    _ => Error::Io(wasi_io_err(e)),
                })?;

                AsyncPollable::new(socket.subscribe()).wait_for().await;

                let (input, output) =
                    socket.finish_connect().map_err(|e| match e {
                        ErrorCode::AccessDenied => {
                            warn!(
                                "Network capability denied — pass `-S inherit-network` \
                                 to wasmtime to grant the component network access."
                            );
                            Error::AccessDenied
                        }
                        _ => Error::Io(wasi_io_err(e)),
                    })?;

                Ok(WasiTcpStream::from_raw(socket, input, output, Some(sock_addr)))
            });
            return Ok(SendDialFut(dial_fut));
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
                            .map(|l| socketaddr_to_multiaddr(l.local_addr))
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

                // ── Phase 1: drive the bind+listen future ──────────────────────
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
                        Poll::Ready(Ok(raw_listener)) => {
                            let local_addr = socketaddr_to_multiaddr(raw_listener.local_addr);
                            state.listener = Some(Arc::new(raw_listener));
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
                        loop {
                            match listener.socket.accept() {
                                Ok((accepted, input, output)) => {
                                    // Peer address from the accepted socket — this is the real
                                    // remote address, not the listen address placeholder used
                                    // in earlier versions.
                                    let peer_addr = accepted
                                        .remote_address()
                                        .map(wasi_sockaddr_to_std)
                                        .ok();
                                    let stream = WasiTcpStream::from_raw(
                                        accepted, input, output, peer_addr,
                                    );
                                    return Ok::<
                                        (WasiTcpStream, Option<SocketAddr>),
                                        std::io::Error,
                                    >((stream, peer_addr));
                                }
                                Err(ErrorCode::WouldBlock) => {
                                    AsyncPollable::new(listener.socket.subscribe())
                                        .wait_for()
                                        .await;
                                }
                                Err(e) => return Err(wasi_io_err(e)),
                            }
                        }
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
                        Poll::Ready(Ok((wasi_stream, peer_addr))) => {
                            state.accept_future = None;
                            let local_addr = socketaddr_to_multiaddr(listener_arc.local_addr);
                            let send_back_addr = peer_addr
                                .map(socketaddr_to_multiaddr)
                                .unwrap_or_else(|| local_addr.clone());
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

// ── wasm32-only helpers ────────────────────────────────────────────────────────

/// Convert a `std::net::SocketAddr` into a WASI `IpSocketAddress`.
#[cfg(target_arch = "wasm32")]
fn sockaddr_to_wasi(addr: SocketAddr) -> IpSocketAddress {
    match addr {
        SocketAddr::V4(a) => {
            let [b0, b1, b2, b3] = a.ip().octets();
            IpSocketAddress::Ipv4(Ipv4SocketAddress {
                port: a.port(),
                address: (b0, b1, b2, b3),
            })
        }
        SocketAddr::V6(a) => {
            let [s0, s1, s2, s3, s4, s5, s6, s7] = a.ip().segments();
            IpSocketAddress::Ipv6(Ipv6SocketAddress {
                port: a.port(),
                flow_info: a.flowinfo(),
                address: (s0, s1, s2, s3, s4, s5, s6, s7),
                scope_id: a.scope_id(),
            })
        }
    }
}

/// Convert a WASI `IpSocketAddress` into a `std::net::SocketAddr`.
#[cfg(target_arch = "wasm32")]
fn wasi_sockaddr_to_std(addr: IpSocketAddress) -> SocketAddr {
    match addr {
        IpSocketAddress::Ipv4(a) => {
            let (b0, b1, b2, b3) = a.address;
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(b0, b1, b2, b3)), a.port)
        }
        IpSocketAddress::Ipv6(a) => {
            let (s0, s1, s2, s3, s4, s5, s6, s7) = a.address;
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(s0, s1, s2, s3, s4, s5, s6, s7)),
                a.port,
            )
        }
    }
}

/// Wrap a WASI `ErrorCode` as a generic `std::io::Error`.
#[cfg(target_arch = "wasm32")]
fn wasi_io_err(e: ErrorCode) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, format!("{e:?}"))
}
