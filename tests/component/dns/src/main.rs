//! M7 integration test: DNS multiaddr dial via `WasiTcpTransport`.
//!
//! Verifies end-to-end DNS resolution + TCP connect using `/dns4/localhost/tcp/<port>`:
//!
//!   1. Bind on an ephemeral port via `/ip4/127.0.0.1/tcp/0`.
//!   2. Drive `Transport::poll` until `NewAddress` to learn the bound port.
//!   3. Construct `/dns4/localhost/tcp/<port>` and call `Transport::dial`.
//!   4. Concurrently accept the incoming connection and complete the dial.
//!   5. Exchange bytes and assert the round-trip is correct.
//!
//! The DNS resolution of `localhost` → `127.0.0.1` exercises the
//! `wasi:sockets/ip-name-lookup` path in `WasiTcpTransport`.

use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;

use futures::future;
use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p_core::multiaddr::{Multiaddr, Protocol};
use libp2p_core::transport::{DialOpts, ListenerId, PortUse, TransportEvent};
use libp2p_core::{Endpoint, Transport};
use libp2p_wasi_sockets::WasiTcpTransport;

const MSG: &[u8] = b"hello from WasiTcpTransport M7 dns";

#[wstd::main]
async fn main() {
    let mut transport = WasiTcpTransport::default();
    let listener_id = ListenerId::next();

    // ── Phase 1: bind on an ephemeral port ────────────────────────────────────
    transport
        .listen_on(listener_id, "/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen_on");

    // ── Phase 2: drive poll() until NewAddress to get the bound port ──────────
    let listen_addr: Multiaddr = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::NewAddress { listen_addr, .. }) => Poll::Ready(listen_addr),
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    })
    .await;

    let port = listen_addr
        .iter()
        .find_map(|p| if let Protocol::Tcp(port) = p { Some(port) } else { None })
        .expect("tcp port in listen_addr");

    eprintln!("M7: bound to {listen_addr}, port={port}");

    // ── Phase 3: dial via DNS ─────────────────────────────────────────────────
    let dns_addr: Multiaddr = format!("/dns4/localhost/tcp/{port}").parse().unwrap();
    eprintln!("M7: dialling {dns_addr}");

    let dial_fut = transport
        .dial(
            dns_addr,
            DialOpts {
                role: Endpoint::Dialer,
                port_use: PortUse::New,
            },
        )
        .expect("dial");

    // ── Phase 4: accept + dial concurrently ───────────────────────────────────
    let accept_fut = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::Incoming { upgrade, .. }) => Poll::Ready(upgrade),
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    });

    let (upgrade, dial_result) = future::join(accept_fut, dial_fut).await;

    let mut server = upgrade.await.expect("server upgrade");
    let mut client = dial_result.expect("dial future");

    // ── Phase 5: byte exchange ─────────────────────────────────────────────────
    let server_task = wstd::runtime::spawn(async move {
        let mut buf = vec![0u8; MSG.len()];
        server.read_exact(&mut buf).await.expect("server read_exact");
        assert_eq!(buf.as_slice(), MSG, "server: received bytes do not match MSG");
        server.write_all(&buf).await.expect("server write_all");
        server.flush().await.expect("server flush");
    });

    client.write_all(MSG).await.expect("client write_all");
    client.flush().await.expect("client flush");

    let mut echo = vec![0u8; MSG.len()];
    client.read_exact(&mut echo).await.expect("client read_exact");
    assert_eq!(echo.as_slice(), MSG, "client: echo bytes do not match MSG");

    server_task.await;

    eprintln!("M7 dns: PASS");
}
