//! M4 integration test: Noise XX + Yamux upgrade on top of WasiTcpTransport.
//!
//! Key insight on yamux 0.13: opening an outbound stream via `poll_outbound` is
//! lazy — no SYN frame is sent until the first write to the stream.  The server
//! therefore cannot accept an inbound stream until the client writes data.  We
//! handle this by writing to the client stream before driving the connections.

use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;

use futures::future;
use futures::io::{AsyncRead, AsyncWrite};
use libp2p_core::muxing::StreamMuxer;
use libp2p_core::transport::{DialOpts, ListenerId, PortUse, TransportEvent};
use libp2p_core::upgrade::Version;
use libp2p_core::{Endpoint, Transport};
use libp2p_identity::Keypair;
use libp2p_wasi_sockets::WasiTcpTransport;

const MSG: &[u8] = b"hello from WasiTcpTransport M4 Noise+Yamux";

#[wstd::main]
async fn main() {
    // ── Phase 1: keypairs ─────────────────────────────────────────────────────
    let server_key = Keypair::generate_ed25519();
    let client_key = Keypair::generate_ed25519();
    let server_peer_id = server_key.public().to_peer_id();
    let client_peer_id = client_key.public().to_peer_id();

    // ── Phase 2: upgraded transports ─────────────────────────────────────────
    let mut server_transport = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(libp2p_noise::Config::new(&server_key).expect("server noise config"))
        .multiplex(libp2p_yamux::Config::default());

    let mut client_transport = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(libp2p_noise::Config::new(&client_key).expect("client noise config"))
        .multiplex(libp2p_yamux::Config::default());

    // ── Phase 3: listen ───────────────────────────────────────────────────────
    let listener_id = ListenerId::next();
    server_transport
        .listen_on(listener_id, "/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen_on");

    // ── Phase 4: wait for NewAddress ──────────────────────────────────────────
    let listen_addr = poll_fn(|cx| match Pin::new(&mut server_transport).poll(cx) {
        Poll::Ready(TransportEvent::NewAddress { listen_addr, .. }) => Poll::Ready(listen_addr),
        Poll::Ready(_) | Poll::Pending => Poll::Pending,
    })
    .await;
    eprintln!("M4: bound to {listen_addr}");

    // ── Phase 5: dial ─────────────────────────────────────────────────────────
    let dial_fut = client_transport
        .dial(
            listen_addr,
            DialOpts {
                role: Endpoint::Dialer,
                port_use: PortUse::New,
            },
        )
        .expect("dial");

    // ── Phase 6: Noise XX + Yamux handshake ───────────────────────────────────
    let server_fut = async {
        let upgrade = poll_fn(|cx| match Pin::new(&mut server_transport).poll(cx) {
            Poll::Ready(TransportEvent::Incoming { upgrade, .. }) => Poll::Ready(upgrade),
            Poll::Ready(_) | Poll::Pending => Poll::Pending,
        })
        .await;
        upgrade.await
    };

    let (server_result, client_result) = future::join(server_fut, dial_fut).await;

    let (remote_from_server, mut server_muxer) = server_result.expect("server handshake");
    let (remote_from_client, mut client_muxer) = client_result.expect("client handshake");

    assert_eq!(remote_from_server, client_peer_id, "server: wrong remote peer ID");
    assert_eq!(remote_from_client, server_peer_id, "client: wrong remote peer ID");
    eprintln!("M4: Noise handshake complete; peer IDs verified");

    // ── Phases 7-9: stream acquisition + data exchange ────────────────────────
    //
    // All done in one poll_fn because of the lazy-SYN behaviour of yamux 0.13:
    //
    //   1. poll_outbound → client stream cs (no SYN sent yet)
    //   2. poll_write(cs, MSG) → queues DATA+SYN frame in stream channel
    //   3. poll_inbound(client_muxer) → Active::poll drains stream channel → TCP
    //   4. poll_inbound(server_muxer) → Active::poll reads TCP → DATA+SYN →
    //      creates inbound stream ss, buffers MSG bytes inside it
    //   5. read from ss → MSG already in receive buffer
    //   6. write echo to ss → queues DATA+ACK frame
    //   7. poll_inbound(server_muxer) → flushes echo → TCP
    //      poll_inbound(client_muxer) → reads echo → routes to cs buffer
    //   8. read from cs → echo bytes

    let client_echo = {
        let mut cs_opt: Option<libp2p_yamux::Stream> = None;
        let mut ss_opt: Option<libp2p_yamux::Stream> = None;

        // Phase 8 state
        let mut client_written: usize = 0;
        let mut client_flushed = false;
        let mut server_pos: usize = 0;
        let mut server_buf = vec![0u8; MSG.len()];

        // Phase 9 state
        let mut server_written: usize = 0;
        let mut server_flushed = false;
        let mut client_pos: usize = 0;
        let mut client_echo = vec![0u8; MSG.len()];

        poll_fn(|cx| {
            // ── Step 1: get client outbound stream ────────────────────────────
            if cs_opt.is_none() {
                match Pin::new(&mut client_muxer).poll_outbound(cx) {
                    Poll::Ready(Ok(s)) => cs_opt = Some(s),
                    Poll::Ready(Err(e)) => panic!("poll_outbound: {e}"),
                    Poll::Pending => return Poll::Pending,
                }
            }

            // ── Step 2: client writes MSG (queues DATA+SYN into stream chan) ──
            if client_written < MSG.len() {
                let cs = cs_opt.as_mut().unwrap();
                match Pin::new(cs).poll_write(cx, &MSG[client_written..]) {
                    Poll::Ready(Ok(n)) => client_written += n,
                    Poll::Ready(Err(e)) => panic!("client write: {e}"),
                    Poll::Pending => {} // will retry next poll
                }
            }
            if client_written == MSG.len() && !client_flushed {
                let cs = cs_opt.as_mut().unwrap();
                match Pin::new(cs).poll_flush(cx) {
                    Poll::Ready(Ok(())) => client_flushed = true,
                    Poll::Ready(Err(e)) => panic!("client flush: {e}"),
                    Poll::Pending => {}
                }
            }

            // ── Step 3: drive client connection (stream chan → TCP) ───────────
            // poll_inbound calls poll_next_inbound which drives Active::poll:
            // it drains the stream channel and writes yamux frames to TCP.
            let _ = Pin::new(&mut client_muxer).poll_inbound(cx);

            // ── Step 4: drive server connection and get inbound stream ────────
            // Active::poll on server reads from TCP; the DATA+SYN frame creates
            // the inbound stream and puts the MSG bytes into its receive buffer.
            if ss_opt.is_none() {
                match Pin::new(&mut server_muxer).poll_inbound(cx) {
                    Poll::Ready(Ok(s)) => ss_opt = Some(s),
                    Poll::Ready(Err(e)) => panic!("server poll_inbound: {e}"),
                    Poll::Pending => {
                        if client_written == 0 {
                            // Nothing written to cs yet; SYN can't reach server.
                            return Poll::Pending;
                        }
                        return Poll::Pending;
                    }
                }
            } else {
                // Keep driving server muxer for subsequent data frames.
                let _ = Pin::new(&mut server_muxer).poll_inbound(cx);
            }

            // ── Step 5: server reads MSG from ss ──────────────────────────────
            if server_pos < MSG.len() {
                let ss = ss_opt.as_mut().unwrap();
                while server_pos < MSG.len() {
                    match Pin::new(&mut *ss).poll_read(cx, &mut server_buf[server_pos..]) {
                        Poll::Ready(Ok(0)) => panic!("server: unexpected EOF"),
                        Poll::Ready(Ok(n)) => server_pos += n,
                        Poll::Ready(Err(e)) => panic!("server read: {e}"),
                        Poll::Pending => break,
                    }
                }
            }
            if server_pos < MSG.len() {
                return Poll::Pending;
            }

            // ── Step 6: server writes echo (queues DATA+ACK into stream chan) ─
            if server_written < server_buf.len() {
                let ss = ss_opt.as_mut().unwrap();
                match Pin::new(&mut *ss).poll_write(cx, &server_buf[server_written..]) {
                    Poll::Ready(Ok(n)) => server_written += n,
                    Poll::Ready(Err(e)) => panic!("server echo write: {e}"),
                    Poll::Pending => {}
                }
            }
            if server_written == server_buf.len() && !server_flushed {
                let ss = ss_opt.as_mut().unwrap();
                match Pin::new(&mut *ss).poll_flush(cx) {
                    Poll::Ready(Ok(())) => server_flushed = true,
                    Poll::Ready(Err(e)) => panic!("server echo flush: {e}"),
                    Poll::Pending => {}
                }
            }

            // ── Step 7: drive server (echo → TCP), then client (TCP → cs) ────
            let _ = Pin::new(&mut server_muxer).poll_inbound(cx);
            let _ = Pin::new(&mut client_muxer).poll_inbound(cx);

            // ── Step 8: client reads echo from cs ─────────────────────────────
            if client_pos < MSG.len() {
                let cs = cs_opt.as_mut().unwrap();
                while client_pos < MSG.len() {
                    match Pin::new(&mut *cs).poll_read(cx, &mut client_echo[client_pos..]) {
                        Poll::Ready(Ok(0)) => panic!("client: unexpected EOF"),
                        Poll::Ready(Ok(n)) => client_pos += n,
                        Poll::Ready(Err(e)) => panic!("client echo read: {e}"),
                        Poll::Pending => break,
                    }
                }
            }

            if client_pos == MSG.len() {
                Poll::Ready(client_echo.clone())
            } else {
                Poll::Pending
            }
        })
        .await
    };

    assert_eq!(client_echo.as_slice(), MSG, "client: wrong echo");
    eprintln!("M4 transport: PASS");
}
