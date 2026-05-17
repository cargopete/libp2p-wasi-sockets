//! Dial a remote libp2p peer using Noise XX + Yamux over [`WasiTcpTransport`].
//!
//! Reads the target address and expected peer ID from environment variables,
//! performs the full Noise XX handshake and Yamux negotiation, opens an
//! outbound substream, writes a message, and reads the echo back.
//!
//! # Environment variables
//!
//! | Variable    | Example                              | Description              |
//! |-------------|--------------------------------------|--------------------------|
//! | `PEER_ADDR` | `/ip4/127.0.0.1/tcp/4001`            | Multiaddr of remote peer |
//! | `PEER_ID`   | `12D3KooW…`                          | Expected base58 peer ID  |
//!
//! # Build and run
//!
//! ```bash
//! cargo build --example noise_dial --target wasm32-wasip2
//!
//! PEER_ADDR=/ip4/127.0.0.1/tcp/4001 \
//! PEER_ID=12D3KooW… \
//! wasmtime run -S inherit-network \
//!     target/wasm32-wasip2/debug/examples/noise_dial.wasm
//! ```
//!
//! # Testing against a native rust-libp2p peer
//!
//! The `m5_interop` integration test in this repository spins up exactly such
//! a peer automatically.  You can also point this example at any other
//! Noise-XX + Yamux capable libp2p node (Kubo, go-libp2p, etc.).

// ── wasm32-wasip2 implementation ─────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::pin::Pin;
    use std::task::Poll;

    use futures::future::poll_fn;
    use futures::io::{AsyncRead, AsyncWrite};
    use libp2p_core::muxing::StreamMuxer;
    use libp2p_core::transport::{DialOpts, PortUse};
    use libp2p_core::upgrade::Version;
    use libp2p_core::{Endpoint, PeerId, Transport};
    use libp2p_identity::Keypair;
    use libp2p_wasi_sockets::WasiTcpTransport;

    const MSG: &[u8] = b"hello from libp2p-wasi-sockets";

    pub async fn run() {
        let peer_addr: libp2p_core::Multiaddr = std::env::var("PEER_ADDR")
            .expect("PEER_ADDR env var missing")
            .parse()
            .expect("PEER_ADDR is not a valid multiaddr");

        let peer_id: PeerId = std::env::var("PEER_ID")
            .expect("PEER_ID env var missing")
            .parse()
            .expect("PEER_ID is not a valid peer ID");

        eprintln!("noise_dial: dialling {peer_addr}");

        // ── Dial with Noise XX + Yamux ────────────────────────────────────────
        let local_key = Keypair::generate_ed25519();

        let mut transport = WasiTcpTransport::default()
            .upgrade(Version::V1)
            .authenticate(libp2p_noise::Config::new(&local_key).expect("noise config"))
            .multiplex(libp2p_yamux::Config::default());

        let dial_fut = transport
            .dial(
                peer_addr,
                DialOpts {
                    role: Endpoint::Dialer,
                    port_use: PortUse::New,
                },
            )
            .expect("dial");

        let (remote_peer_id, mut muxer) = dial_fut.await.expect("handshake");

        assert_eq!(
            remote_peer_id, peer_id,
            "peer ID mismatch: got {remote_peer_id}, expected {peer_id}"
        );
        eprintln!("noise_dial: connected to {remote_peer_id}");

        // ── Open a Yamux substream and do one echo round-trip ─────────────────
        //
        // yamux 0.13 is lazy: the SYN is bundled with the first DATA frame.
        // Write MSG before calling poll_inbound so Active::poll finds a frame
        // in the stream channel and can flush it to TCP.
        let echo = {
            let mut stream_opt: Option<libp2p_yamux::Stream> = None;
            let mut written: usize = 0;
            let mut flushed = false;
            let mut echo_buf = vec![0u8; MSG.len()];
            let mut echo_pos: usize = 0;

            poll_fn(|cx| {
                // Step 1: open outbound yamux stream.
                if stream_opt.is_none() {
                    match Pin::new(&mut muxer).poll_outbound(cx) {
                        Poll::Ready(Ok(s)) => stream_opt = Some(s),
                        Poll::Ready(Err(e)) => panic!("poll_outbound: {e}"),
                        Poll::Pending => return Poll::Pending,
                    }
                }

                // Step 2: write MSG into stream channel (queues DATA+SYN).
                if written < MSG.len() {
                    let s = stream_opt.as_mut().unwrap();
                    match Pin::new(s).poll_write(cx, &MSG[written..]) {
                        Poll::Ready(Ok(n)) => written += n,
                        Poll::Ready(Err(e)) => panic!("write: {e}"),
                        Poll::Pending => {}
                    }
                }
                if written == MSG.len() && !flushed {
                    let s = stream_opt.as_mut().unwrap();
                    match Pin::new(s).poll_flush(cx) {
                        Poll::Ready(Ok(())) => flushed = true,
                        Poll::Ready(Err(e)) => panic!("flush: {e}"),
                        Poll::Pending => {}
                    }
                }

                // Step 3: drive connection — channel → TCP, TCP → stream buffer.
                let _ = Pin::new(&mut muxer).poll_inbound(cx);

                // Step 4: read echo.
                if echo_pos < MSG.len() {
                    let s = stream_opt.as_mut().unwrap();
                    while echo_pos < MSG.len() {
                        match Pin::new(&mut *s).poll_read(cx, &mut echo_buf[echo_pos..]) {
                            Poll::Ready(Ok(0)) => panic!("unexpected EOF reading echo"),
                            Poll::Ready(Ok(n)) => echo_pos += n,
                            Poll::Ready(Err(e)) => panic!("echo read: {e}"),
                            Poll::Pending => break,
                        }
                    }
                }

                if echo_pos == MSG.len() {
                    Poll::Ready(echo_buf.clone())
                } else {
                    Poll::Pending
                }
            })
            .await
        };

        assert_eq!(echo.as_slice(), MSG, "echo mismatch");
        eprintln!("noise_dial: echo OK — round-trip complete");
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
    eprintln!("Run: cargo build --example noise_dial --target wasm32-wasip2");
    std::process::exit(1);
}
