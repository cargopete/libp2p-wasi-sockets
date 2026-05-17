//! M5 interop test: WasiTcpTransport dials a native rust-libp2p peer.
//!
//! Reads two env vars set by the Wasmtime harness:
//!   NATIVE_ADDR    — multiaddr of the native listener (e.g. /ip4/127.0.0.1/tcp/50000)
//!   NATIVE_PEER_ID — base58 peer ID of the native peer
//!
//! Dial flow (client / WASM side):
//!   1. WasiTcpTransport + Noise XX + Yamux → (peer_id, muxer)
//!   2. Assert peer_id == NATIVE_PEER_ID
//!   3. poll_outbound → cs (yamux substream; SYN is lazy in yamux 0.13)
//!   4. poll_write(cs, MSG) → queues DATA+SYN into stream channel
//!   5. poll_inbound(client_muxer) → Active::poll: channel → TCP
//!   6. poll_inbound(client_muxer) in a loop until echo arrives in cs buffer
//!   7. Assert echo == MSG

use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;

use futures::io::{AsyncRead, AsyncWrite};
use libp2p_core::muxing::StreamMuxer;
use libp2p_core::transport::{DialOpts, PortUse};
use libp2p_core::upgrade::Version;
use libp2p_core::{Endpoint, PeerId, Transport};
use libp2p_identity::Keypair;
use libp2p_wasi_sockets::WasiTcpTransport;

const MSG: &[u8] = b"hello from WasiTcpTransport M5 interop";

#[wstd::main]
async fn main() {
    let native_addr: libp2p_core::Multiaddr = std::env::var("NATIVE_ADDR")
        .expect("NATIVE_ADDR env var missing")
        .parse()
        .expect("NATIVE_ADDR is not a valid multiaddr");

    let native_peer_id: PeerId = std::env::var("NATIVE_PEER_ID")
        .expect("NATIVE_PEER_ID env var missing")
        .parse()
        .expect("NATIVE_PEER_ID is not a valid peer ID");

    eprintln!("M5: dialling native peer at {native_addr}");

    // ── Dial with Noise + Yamux upgrade ──────────────────────────────────────
    let client_key = Keypair::generate_ed25519();

    let mut client_transport = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(libp2p_noise::Config::new(&client_key).expect("noise config"))
        .multiplex(libp2p_yamux::Config::default());

    // Trigger the dial before we start polling for transport events.
    let dial_fut = client_transport
        .dial(
            native_addr,
            DialOpts {
                role: Endpoint::Dialer,
                port_use: PortUse::New,
            },
        )
        .expect("dial");

    // The Noise XX handshake runs over the raw TCP connection; since we're the
    // dialer we drive it by awaiting the dial future directly.  The transport
    // itself doesn't need to be polled for a listener event here.
    let (remote_peer_id, mut client_muxer) = dial_fut.await.expect("handshake");

    assert_eq!(
        remote_peer_id, native_peer_id,
        "M5: peer ID mismatch: got {remote_peer_id}, expected {native_peer_id}"
    );
    eprintln!("M5: Noise handshake OK; native peer ID verified");

    // ── Substream echo — combined poll_fn (same pattern as M4 client side) ──
    //
    // yamux 0.13 is lazy: no SYN until the first poll_write.  We must write
    // MSG before driving the connection, so that Active::poll finds a frame in
    // the stream's mpsc channel and can flush it to TCP.
    let echo = {
        let mut cs_opt: Option<libp2p_yamux::Stream> = None;
        let mut client_written: usize = 0;
        let mut client_flushed = false;
        let mut echo_buf = vec![0u8; MSG.len()];
        let mut echo_pos: usize = 0;

        poll_fn(|cx| {
            // Step 1: open outbound yamux stream
            if cs_opt.is_none() {
                match Pin::new(&mut client_muxer).poll_outbound(cx) {
                    Poll::Ready(Ok(s)) => cs_opt = Some(s),
                    Poll::Ready(Err(e)) => panic!("poll_outbound: {e}"),
                    Poll::Pending => return Poll::Pending,
                }
            }

            // Step 2: write MSG into stream channel (DATA+SYN on first write)
            if client_written < MSG.len() {
                let cs = cs_opt.as_mut().unwrap();
                match Pin::new(cs).poll_write(cx, &MSG[client_written..]) {
                    Poll::Ready(Ok(n)) => client_written += n,
                    Poll::Ready(Err(e)) => panic!("write: {e}"),
                    Poll::Pending => {}
                }
            }
            if client_written == MSG.len() && !client_flushed {
                let cs = cs_opt.as_mut().unwrap();
                match Pin::new(cs).poll_flush(cx) {
                    Poll::Ready(Ok(())) => client_flushed = true,
                    Poll::Ready(Err(e)) => panic!("flush: {e}"),
                    Poll::Pending => {}
                }
            }

            // Step 3: drive client connection — stream channel → TCP,
            //         then TCP → cs receive buffer (for the native echo reply)
            let _ = Pin::new(&mut client_muxer).poll_inbound(cx);

            // Step 4: read echo from cs receive buffer
            if echo_pos < MSG.len() {
                let cs = cs_opt.as_mut().unwrap();
                while echo_pos < MSG.len() {
                    match Pin::new(&mut *cs).poll_read(cx, &mut echo_buf[echo_pos..]) {
                        Poll::Ready(Ok(0)) => panic!("M5: unexpected EOF reading echo"),
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

    assert_eq!(echo.as_slice(), MSG, "M5: echo mismatch");
    eprintln!("M5 interop: PASS");
}
