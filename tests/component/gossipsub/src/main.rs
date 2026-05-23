//! M13 integration test: libp2p gossipsub over WasiTcpTransport.
//!
//! Two WASM components exchange a gossipsub message over a direct TCP
//! connection.  The same binary is used for both roles, selected via `MODE`:
//!
//! - `MODE=listen`  — binds on `LISTEN_PORT`, subscribes to `/test/1.0.0`,
//!                    waits for b"hello M13" from the publisher.
//! - `MODE=publish` — dials `DIAL_ADDR`, subscribes to `/test/1.0.0`, waits
//!                    for the listener's `Subscribed` event, then publishes
//!                    b"hello M13" and waits for the connection to close.
//!
//! `flood_publish = true` and a 100 ms heartbeat ensure the message is
//! delivered in a 2-peer scenario without waiting for full mesh formation.

use std::pin::Pin;
use std::time::Duration;

use futures::StreamExt as _;
use libp2p_core::muxing::StreamMuxerBox;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade::Version;
use libp2p_core::Transport as _;
use libp2p_gossipsub::{IdentTopic, MessageAuthenticity};
use libp2p_identity::{Keypair, PeerId};
use libp2p_swarm::{Config as SwarmConfig, Swarm, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;

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

fn build_gossipsub(keypair: &Keypair) -> libp2p_gossipsub::Behaviour {
    let config = libp2p_gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_millis(100))
        .flood_publish(true)
        .build()
        .expect("gossipsub config");
    libp2p_gossipsub::Behaviour::new(MessageAuthenticity::Signed(keypair.clone()), config)
        .expect("gossipsub behaviour")
}

#[wstd::main]
async fn main() {
    match std::env::var("MODE").as_deref().unwrap_or("listen") {
        "publish" => run_publisher().await,
        _ => run_listener().await,
    }
}

async fn run_listener() {
    let listen_port: u16 = std::env::var("LISTEN_PORT")
        .expect("LISTEN_PORT not set")
        .parse()
        .expect("invalid LISTEN_PORT");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M13 gossipsub-listener: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);
    let mut behaviour = build_gossipsub(&keypair);
    let topic = IdentTopic::new("/test/1.0.0");
    behaviour.subscribe(&topic).expect("subscribe");

    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    let listen_addr: libp2p_core::Multiaddr =
        format!("/ip4/0.0.0.0/tcp/{listen_port}").parse().unwrap();
    swarm.listen_on(listen_addr).expect("listen_on");

    let mut got_message = false;
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                eprintln!("M13 gossipsub-listener: listening on {address}");
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M13 gossipsub-listener: connected to {peer_id}");
            }
            SwarmEvent::Behaviour(libp2p_gossipsub::Event::Message { message, .. })
                if message.topic == topic.hash() =>
            {
                eprintln!(
                    "M13 gossipsub-listener: received {:?}",
                    String::from_utf8_lossy(&message.data)
                );
                assert_eq!(&message.data[..], b"hello M13", "unexpected message");
                got_message = true;
                break;
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                panic!("incoming connection error: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M13 gossipsub-listener: connection closed");
                break;
            }
            ev => {
                eprintln!("M13 gossipsub-listener event: {ev:?}");
            }
        }
    }

    if !got_message {
        panic!("M13 gossipsub-listener: connection closed before message received");
    }
    eprintln!("M13 gossipsub-listener: PASS");
}

async fn run_publisher() {
    let dial_addr: libp2p_core::Multiaddr = std::env::var("DIAL_ADDR")
        .expect("DIAL_ADDR not set")
        .parse()
        .expect("invalid DIAL_ADDR");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M13 gossipsub-publisher: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);
    let mut behaviour = build_gossipsub(&keypair);
    let topic = IdentTopic::new("/test/1.0.0");
    behaviour.subscribe(&topic).expect("subscribe");

    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    swarm.dial(dial_addr.clone()).expect("dial");
    eprintln!("M13 gossipsub-publisher: dialling {dial_addr}");

    let mut published = false;
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M13 gossipsub-publisher: connected to {peer_id}");
            }
            SwarmEvent::Behaviour(libp2p_gossipsub::Event::Subscribed { peer_id, topic: t })
                if t == topic.hash() && !published =>
            {
                eprintln!("M13 gossipsub-publisher: peer {peer_id} subscribed to {t}");
                swarm
                    .behaviour_mut()
                    .publish(topic.hash(), b"hello M13")
                    .expect("publish");
                eprintln!("M13 gossipsub-publisher: published message");
                published = true;
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("dial failed: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } if published => {
                eprintln!("M13 gossipsub-publisher: connection closed after publish");
                break;
            }
            SwarmEvent::ConnectionClosed { .. } => {
                panic!("M13 gossipsub-publisher: connection closed before publishing");
            }
            ev => {
                eprintln!("M13 gossipsub-publisher event: {ev:?}");
            }
        }
    }

    eprintln!("M13 gossipsub-publisher: PASS");
}
