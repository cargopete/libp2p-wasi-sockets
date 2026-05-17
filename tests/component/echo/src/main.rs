//! Echo integration test for `WasiTcpStream`.
//!
//! This binary is compiled for `wasm32-wasip2` and run under Wasmtime by
//! `tests/integration.rs`.  It exercises the full `AsyncRead` / `AsyncWrite`
//! bridge in a real loopback TCP round-trip:
//!
//!   listener task ←── writes ──── dialer task
//!                  ──── echo ───→
//!
//! The test passes if both sides agree on the exchanged bytes.  Any `assert!`
//! failure or unexpected error causes a non-zero exit code, which the
//! integration test harness treats as a test failure.

use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p_wasi_sockets::WasiTcpStream;
use wstd::iter::AsyncIterator as _;
use wstd::net::{TcpListener, TcpStream};

/// The message sent from dialer → listener and echoed back.
const MSG: &[u8] = b"hello from WasiTcpStream M1";

#[wstd::main]
async fn main() {
    // ── Phase 1: bind listener on an ephemeral port ───────────────────────
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("TcpListener::bind");

    let port = listener
        .local_addr()
        .expect("local_addr")
        .port();

    // ── Phase 2: spawn the listener task ─────────────────────────────────
    // wstd::runtime::spawn runs a task on the same single-threaded reactor.
    let listener_task = wstd::runtime::spawn(async move {
        let accepted = listener
            .incoming()
            .next()
            .await
            .expect("listener::next — no result")
            .expect("listener::next — accept error");

        let mut server = WasiTcpStream::new(accepted);

        // Read MSG from dialer.
        let mut buf = vec![0u8; MSG.len()];
        server
            .read_exact(&mut buf)
            .await
            .expect("server read_exact");
        assert_eq!(buf.as_slice(), MSG, "server: received bytes do not match MSG");

        // Echo MSG back.
        server
            .write_all(&buf)
            .await
            .expect("server write_all");
        server.flush().await.expect("server flush");
    });

    // ── Phase 3: dial and exchange bytes ──────────────────────────────────
    let raw = TcpStream::connect(format!("127.0.0.1:{port}").as_str())
        .await
        .expect("TcpStream::connect");

    let mut client = WasiTcpStream::new(raw);

    client
        .write_all(MSG)
        .await
        .expect("client write_all");
    client.flush().await.expect("client flush");

    let mut echo = vec![0u8; MSG.len()];
    client
        .read_exact(&mut echo)
        .await
        .expect("client read_exact");
    assert_eq!(echo.as_slice(), MSG, "client: echo bytes do not match MSG");

    // ── Phase 4: join listener ────────────────────────────────────────────
    listener_task.await;

    eprintln!("M1 echo: PASS");
}
