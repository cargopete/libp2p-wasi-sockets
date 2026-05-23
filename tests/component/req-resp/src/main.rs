//! M17 integration test: libp2p request-response over WasiTcpTransport.
//!
//! Two WASM components form a request-response pair over direct TCP.
//! The same binary runs as either a server or client, selected via `MODE`:
//!
//! - `MODE=server` — binds on `LISTEN_PORT`, waits for a request, sends
//!                   back b"pong", then exits.
//!
//! - `MODE=client` — dials `SERVER_ADDR`, sends request b"ping", asserts
//!                   the response equals b"pong".

use std::io;
use std::pin::Pin;

use async_trait::async_trait;
use futures::prelude::*;
use libp2p_core::muxing::StreamMuxerBox;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade::Version;
use libp2p_core::Transport as _;
use libp2p_identity::{Keypair, PeerId};
use libp2p_request_response::{Behaviour, Config, Event, Message, ProtocolSupport};
use libp2p_swarm::{Config as SwarmConfig, Swarm, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;

// ── Codec ─────────────────────────────────────────────────────────────────────

/// Simple length-prefixed codec: 4-byte big-endian length followed by payload.
#[derive(Clone)]
struct SimpleCodec;

const PROTOCOL: &str = "/ping-pong/1.0.0";

#[async_trait]
impl libp2p_request_response::Codec for SimpleCodec {
    type Protocol = &'static str;
    type Request = Vec<u8>;
    type Response = Vec<u8>;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, res).await
    }
}

async fn read_length_prefixed<T: AsyncRead + Unpin>(io: &mut T) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_length_prefixed<T: AsyncWrite + Unpin>(io: &mut T, data: Vec<u8>) -> io::Result<()> {
    io.write_all(&(data.len() as u32).to_be_bytes()).await?;
    io.write_all(&data).await?;
    io.flush().await
}

// ── Transport ─────────────────────────────────────────────────────────────────

struct WstdExecutor;

impl libp2p_swarm::Executor for WstdExecutor {
    fn exec(&self, future: Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        wstd::runtime::spawn(future).detach();
    }
}

fn build_transport(keypair: &Keypair) -> Boxed<(PeerId, StreamMuxerBox)> {
    let noise = libp2p_noise::Config::new(keypair).expect("noise config");
    let yamux = libp2p_yamux::Config::default();
    WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(noise)
        .multiplex(yamux)
        .boxed()
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[wstd::main]
async fn main() {
    match std::env::var("MODE").as_deref().unwrap_or("server") {
        "client" => run_client().await,
        _ => run_server().await,
    }
}

// ── Server ────────────────────────────────────────────────────────────────────

async fn run_server() {
    let listen_port: u16 = std::env::var("LISTEN_PORT")
        .expect("LISTEN_PORT not set")
        .parse()
        .expect("invalid LISTEN_PORT");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M17 server: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);
    let behaviour = Behaviour::with_codec(
        SimpleCodec,
        [(PROTOCOL, ProtocolSupport::Full)],
        Config::default(),
    );
    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    let listen_addr: libp2p_core::Multiaddr =
        format!("/ip4/0.0.0.0/tcp/{listen_port}").parse().unwrap();
    swarm.listen_on(listen_addr).expect("listen_on");

    let mut responded = false;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                eprintln!("M17 server: listening on {address}");
            }
            SwarmEvent::Behaviour(Event::Message {
                message:
                    Message::Request {
                        request,
                        channel,
                        ..
                    },
                ..
            }) => {
                eprintln!(
                    "M17 server: got request {:?}",
                    String::from_utf8_lossy(&request)
                );
                swarm
                    .behaviour_mut()
                    .send_response(channel, b"pong".to_vec())
                    .expect("send_response");
                responded = true;
            }
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M17 server: client disconnected");
                break;
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                panic!("incoming connection error: {error}");
            }
            _ => {}
        }
    }

    assert!(responded, "M17 server: never responded to a request");
    eprintln!("M17 server: PASS");
}

// ── Client ────────────────────────────────────────────────────────────────────

async fn run_client() {
    let server_addr: libp2p_core::Multiaddr = std::env::var("SERVER_ADDR")
        .expect("SERVER_ADDR not set")
        .parse()
        .expect("invalid SERVER_ADDR");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M17 client: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);
    let behaviour = Behaviour::with_codec(
        SimpleCodec,
        [(PROTOCOL, ProtocolSupport::Full)],
        Config::default(),
    );
    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    eprintln!("M17 client: dialling {server_addr}");
    swarm.dial(server_addr).expect("dial");

    let mut query_id = None;
    let mut got_response = false;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M17 client: connected to {peer_id}");
                let id = swarm
                    .behaviour_mut()
                    .send_request(&peer_id, b"ping".to_vec());
                query_id = Some(id);
                eprintln!("M17 client: sent request (id={id:?})");
            }
            SwarmEvent::Behaviour(Event::Message {
                message: Message::Response { response, .. },
                ..
            }) => {
                eprintln!(
                    "M17 client: got response {:?}",
                    String::from_utf8_lossy(&response)
                );
                assert_eq!(response, b"pong", "unexpected response");
                got_response = true;
                break;
            }
            SwarmEvent::Behaviour(Event::OutboundFailure { error, .. }) => {
                panic!("M17 client: outbound failure: {error}");
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("M17 client: dial failed: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } if !got_response => {
                panic!("M17 client: connection closed before response received");
            }
            ev => {
                eprintln!("M17 client event: {ev:?}");
            }
        }
    }

    let _ = query_id;
    eprintln!("M17 client: PASS");
}
