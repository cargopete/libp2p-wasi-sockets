//! M2 integration test for `WasiTcpTransport`.
//!
//! Exercises `Transport::listen_on` + `Transport::dial` end-to-end:
//!
//!   1. Bind on an ephemeral port via `listen_on`.
//!   2. Drive `Transport::poll` until `NewAddress` to learn the actual port.
//!   3. Call `Transport::dial` with that address.
//!   4. Drive accept and dial concurrently using `futures::future::join` until
//!      both sides hold a connected `WasiTcpStream`.
//!   5. Exchange bytes and assert the round-trip is correct.
//!
//! Any assertion failure or unexpected error causes a non-zero exit code,
//! which the integration harness treats as a test failure.

use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;

use futures::future;
use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p_core::transport::{DialOpts, ListenerId, PortUse, TransportEvent};
use libp2p_core::{Endpoint, Transport};
use libp2p_wasi_sockets::WasiTcpTransport;

const MSG: &[u8] = b"hello from WasiTcpTransport M2";

#[wstd::main]
async fn main() {
    let mut transport = WasiTcpTransport::default();
    let listener_id = ListenerId::next();

    // ── Phase 1: bind on an ephemeral port ────────────────────────────────────
    transport
        .listen_on(listener_id, "/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen_on");

    // ── Phase 2: drive poll() until NewAddress ────────────────────────────────
    let listen_addr = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::NewAddress { listen_addr, .. }) => Poll::Ready(listen_addr),
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    })
    .await;

    eprintln!("M2: bound to {listen_addr}");

    // ── Phase 3: kick off the dial ────────────────────────────────────────────
    let dial_fut = transport
        .dial(
            listen_addr,
            DialOpts {
                role: Endpoint::Dialer,
                port_use: PortUse::New,
            },
        )
        .expect("dial");

    // ── Phase 4: drive accept and dial concurrently ───────────────────────────
    //
    // `accept_fut` borrows `transport` mutably via the poll_fn closure.
    // `dial_fut` owns its state entirely (extracted via transport.dial()), so
    // there is no conflict.  Both share the same waker; when the loopback
    // connect completes, both sides become ready in the same reactor turn.
    let accept_fut = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::Incoming { upgrade, .. }) => Poll::Ready(upgrade),
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    });

    let (upgrade, dial_result) = future::join(accept_fut, dial_fut).await;

    let mut server = upgrade.await.expect("server upgrade");
    let mut client = dial_result.expect("dial future");

    // ── Phase 5: byte exchange ─────────────────────────────────────────────────
    //
    // Server reads MSG then echoes it back.  Run the server in a spawned task
    // so the client write and the server read can proceed concurrently.
    let server_task = wstd::runtime::spawn(async move {
        let mut buf = vec![0u8; MSG.len()];
        server
            .read_exact(&mut buf)
            .await
            .expect("server read_exact");
        assert_eq!(buf.as_slice(), MSG, "server: received bytes do not match MSG");
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
    assert_eq!(echo.as_slice(), MSG, "client: echo bytes do not match MSG");

    server_task.await;

    eprintln!("M2 transport: PASS");
}
