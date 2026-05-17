//! M3 integration test for multi-listener support and lifecycle events.
//!
//! Scenario:
//!   1. Bind two listeners on ephemeral ports via `listen_on`.
//!   2. Drive `Transport::poll` until both `NewAddress` events are received.
//!   3. Dial listener 1; drive accept + dial concurrently; exchange bytes.
//!   4. Remove listener 2 via `remove_listener`.
//!   5. Drive `Transport::poll` until `AddressExpired` for listener 2.
//!   6. Drive `Transport::poll` until `ListenerClosed` for listener 2.
//!
//! Any assertion failure or unexpected error causes a non-zero exit code,
//! which the integration harness treats as a test failure.

use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;

use futures::future;
use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p_core::transport::{DialOpts, ListenerId, PortUse, TransportEvent};
use libp2p_core::{Endpoint, Transport};
use libp2p_wasi_sockets::WasiTcpTransport;

const MSG: &[u8] = b"hello from WasiTcpTransport M3 multi-listener";

#[wstd::main]
async fn main() {
    let mut transport = WasiTcpTransport::default();
    let id1 = ListenerId::next();
    let id2 = ListenerId::next();

    // ── Phase 1: bind two listeners ───────────────────────────────────────────
    transport
        .listen_on(id1, "/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen_on id1");
    transport
        .listen_on(id2, "/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen_on id2");

    // ── Phase 2: collect both NewAddress events ───────────────────────────────
    let mut addrs: HashMap<ListenerId, libp2p_core::Multiaddr> = HashMap::new();
    while addrs.len() < 2 {
        let (lid, addr) = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
            Poll::Ready(TransportEvent::NewAddress { listener_id, listen_addr }) => {
                Poll::Ready((listener_id, listen_addr))
            }
            Poll::Ready(_) | Poll::Pending => Poll::Pending,
        })
        .await;
        eprintln!("M3: listener {lid} bound to {addr}");
        addrs.insert(lid, addr);
    }

    // ── Phase 3: dial listener 1, exchange bytes ──────────────────────────────
    let addr1 = addrs[&id1].clone();

    let dial_fut = transport
        .dial(
            addr1,
            DialOpts {
                role: Endpoint::Dialer,
                port_use: PortUse::New,
            },
        )
        .expect("dial id1");

    let accept_fut = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::Incoming { listener_id, upgrade, .. })
            if listener_id == id1 =>
        {
            Poll::Ready(upgrade)
        }
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    });

    let (upgrade, dial_result) = future::join(accept_fut, dial_fut).await;

    let mut server = upgrade.await.expect("server upgrade");
    let mut client = dial_result.expect("dial");

    let server_task = wstd::runtime::spawn(async move {
        let mut buf = vec![0u8; MSG.len()];
        server
            .read_exact(&mut buf)
            .await
            .expect("server read_exact");
        assert_eq!(buf.as_slice(), MSG, "server: wrong bytes");
        server.write_all(&buf).await.expect("server write_all");
        server.flush().await.expect("server flush");
    });

    client.write_all(MSG).await.expect("client write_all");
    client.flush().await.expect("client flush");

    let mut echo = vec![0u8; MSG.len()];
    client
        .read_exact(&mut echo)
        .await
        .expect("client read_exact");
    assert_eq!(echo.as_slice(), MSG, "client: wrong echo");

    server_task.await;
    eprintln!("M3: byte exchange on listener 1 OK");

    // ── Phase 4: remove listener 2 ────────────────────────────────────────────
    assert!(
        transport.remove_listener(id2),
        "remove_listener(id2) should return true"
    );

    // ── Phase 5: expect AddressExpired for id2 ────────────────────────────────
    poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::AddressExpired { listener_id, listen_addr })
            if listener_id == id2 =>
        {
            eprintln!("M3: AddressExpired for listener 2 ({listen_addr}) OK");
            Poll::Ready(())
        }
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    })
    .await;

    // ── Phase 6: expect ListenerClosed for id2 ────────────────────────────────
    poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::ListenerClosed { listener_id, reason })
            if listener_id == id2 =>
        {
            assert!(reason.is_ok(), "ListenerClosed reason should be Ok(())");
            eprintln!("M3: ListenerClosed for listener 2 OK");
            Poll::Ready(())
        }
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    })
    .await;

    eprintln!("M3 transport: PASS");
}
