//! TCP echo server using [`WasiTcpTransport`].
//!
//! Listens on `/ip4/0.0.0.0/tcp/4001`, accepts connections, and echoes every
//! byte back to the sender.  This example has no authentication or
//! multiplexing — it demonstrates the raw `Transport` API only.
//!
//! # Build and run
//!
//! ```bash
//! cargo build --example echo --target wasm32-wasip2
//!
//! wasmtime run -S inherit-network \
//!     target/wasm32-wasip2/debug/examples/echo.wasm
//! ```
//!
//! Then in another terminal:
//!
//! ```bash
//! echo "hello wasi" | nc localhost 4001
//! # → hello wasi
//! ```

// ── wasm32-wasip2 implementation ─────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::pin::Pin;
    use std::task::Poll;

    use futures::future::poll_fn;
    use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use libp2p_core::transport::{ListenerId, TransportEvent};
    use libp2p_core::Transport;
    use libp2p_wasi_sockets::WasiTcpTransport;

    pub async fn run() {
        let mut transport = WasiTcpTransport::default();

        transport
            .listen_on(ListenerId::next(), "/ip4/0.0.0.0/tcp/4001".parse().unwrap())
            .expect("listen_on");

        // Drive the transport until the OS assigns the address.
        let addr = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
            Poll::Ready(TransportEvent::NewAddress { listen_addr, .. }) => Poll::Ready(listen_addr),
            Poll::Ready(_) | Poll::Pending => Poll::Pending,
        })
        .await;

        eprintln!("echo: listening on {addr}");

        // Accept connections forever.
        loop {
            let upgrade = poll_fn(|cx| match Pin::new(&mut transport).poll(cx) {
                Poll::Ready(TransportEvent::Incoming { upgrade, .. }) => Poll::Ready(upgrade),
                Poll::Ready(_) | Poll::Pending => Poll::Pending,
            })
            .await;

            let mut stream = upgrade.await.expect("accept");
            eprintln!("echo: accepted connection");

            // Spawn so we can accept the next connection concurrently.
            // The Task handle is intentionally dropped — it runs to completion
            // on the wstd reactor regardless.
            let _task = wstd::runtime::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => stream.write_all(&buf[..n]).await.expect("write"),
                        Err(e) => {
                            eprintln!("echo: read error: {e}");
                            break;
                        }
                    }
                }
                eprintln!("echo: connection closed");
            });
        }
    }
}

// ── entry points ──────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
#[wstd::main]
async fn main() {
    imp::run().await;
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("This example must be compiled for wasm32-wasip2.");
    eprintln!("Run: cargo build --example echo --target wasm32-wasip2");
    std::process::exit(1);
}
