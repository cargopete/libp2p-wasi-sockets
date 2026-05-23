//! Echo integration test for `WasiTcpStream`.
//!
//! This binary is compiled for `wasm32-wasip2` and run under Wasmtime by
//! `tests/integration.rs`.  It exercises the full `AsyncRead` / `AsyncWrite`
//! bridge in a real loopback TCP round-trip via `WasiTcpTransport`:
//!
//!   listener task ←── writes ──── dialer task
//!                  ──── echo ───→
//!
//! The test passes if both sides agree on the exchanged bytes.  Any `assert!`
//! failure or unexpected error causes a non-zero exit code, which the
//! integration test harness treats as a test failure.

use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;

use futures::future;
use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p_core::transport::{DialOpts, ListenerId, PortUse, TransportEvent};
use libp2p_core::{Endpoint, Transport};
use libp2p_wasi_sockets::WasiTcpTransport;

/// The message sent from dialer → listener and echoed back.
const MSG: &[u8] = b"hello from WasiTcpStream M1";

#[wstd::main]
async fn main() {
    let mut transport = WasiTcpTransport::default();
    let listener_id = ListenerId::next();

    // ── Phase 1: bind listener on an ephemeral port ───────────────────────
    transport
        .listen_on(listener_id, "/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen_on");

    // ── Phase 2: drive poll() until NewAddress ────────────────────────────
    let listen_addr = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::NewAddress { listen_addr, .. }) => Poll::Ready(listen_addr),
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    })
    .await;

    // ── Phase 3: kick off the dial ────────────────────────────────────────
    let dial_fut = transport
        .dial(
            listen_addr,
            DialOpts {
                role: Endpoint::Dialer,
                port_use: PortUse::New,
            },
        )
        .expect("dial");

    // ── Phase 4: drive accept and dial concurrently ───────────────────────
    let accept_fut = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
        Poll::Ready(TransportEvent::Incoming { upgrade, .. }) => Poll::Ready(upgrade),
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    });

    let (upgrade, dial_result) = future::join(accept_fut, dial_fut).await;

    let mut server = upgrade.await.expect("server upgrade");
    let mut client = dial_result.expect("dial future");

    // ── Phase 5: byte exchange ─────────────────────────────────────────────
    //
    // Server reads MSG then echoes it back.  Run in a spawned task so client
    // write and server read can proceed concurrently.
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

    eprintln!("M1 echo: PASS");
}
