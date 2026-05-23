//! M18 integration test: libp2p-rendezvous over WasiTcpTransport.
//!
//! A native Tokio rendezvous server acts as the meeting point.
//! The same binary runs as either registrant or discoverer:
//!
//! - `MODE=register` — listens on `LISTEN_PORT`, dials `SERVER_ADDR`,
//!                     registers under namespace "wasm-peers", exits on
//!                     `client::Event::Registered`.
//!
//! - `MODE=discover` — dials `SERVER_ADDR`, discovers "wasm-peers",
//!                     asserts at least one registration returned, exits.

use std::pin::Pin;

use futures::prelude::*;
use libp2p_core::muxing::StreamMuxerBox;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade::Version;
use libp2p_core::Transport as _;
use libp2p_identity::{Keypair, PeerId};
use libp2p_rendezvous::client::{self, Behaviour as RendezvousBehaviour};
use libp2p_rendezvous::Namespace;
use libp2p_swarm::{Config as SwarmConfig, Swarm, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;

const NAMESPACE: &str = "wasm-peers";

// ── Executor ──────────────────────────────────────────────────────────────────

struct WstdExecutor;

impl libp2p_swarm::Executor for WstdExecutor {
    fn exec(&self, future: Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        wstd::runtime::spawn(future).detach();
    }
}

// ── Transport ─────────────────────────────────────────────────────────────────

fn build_transport(keypair: &Keypair) -> Boxed<(PeerId, StreamMuxerBox)> {
    WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(libp2p_noise::Config::new(keypair).expect("noise config"))
        .multiplex(libp2p_yamux::Config::default())
        .boxed()
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[wstd::main]
async fn main() {
    match std::env::var("MODE").as_deref().unwrap_or("register") {
        "discover" => run_discoverer().await,
        _ => run_registrant().await,
    }
}

// ── Registrant ────────────────────────────────────────────────────────────────

async fn run_registrant() {
    let listen_port: u16 = std::env::var("LISTEN_PORT")
        .expect("LISTEN_PORT not set")
        .parse()
        .expect("invalid LISTEN_PORT");

    let server_addr: libp2p_core::Multiaddr = std::env::var("SERVER_ADDR")
        .expect("SERVER_ADDR not set")
        .parse()
        .expect("invalid SERVER_ADDR");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M18 registrant: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);
    let behaviour = RendezvousBehaviour::new(keypair.clone());
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, SwarmConfig::with_executor(WstdExecutor));

    // Listen so the Swarm has a real address to include in the PeerRecord.
    // Use 127.0.0.1 (not 0.0.0.0) so the listen address is routable.
    let listen_addr: libp2p_core::Multiaddr =
        format!("/ip4/127.0.0.1/tcp/{listen_port}").parse().unwrap();
    swarm.listen_on(listen_addr).expect("listen_on");

    // Wait for the listener to be ready, then promote the address to an
    // external address so the rendezvous client can include it in PeerRecord.
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                eprintln!("M18 registrant: listening on {address}");
                swarm.add_external_address(address);
                break;
            }
            _ => {}
        }
    }

    eprintln!("M18 registrant: dialling {server_addr}");
    swarm.dial(server_addr).expect("dial");

    let ns = Namespace::new(NAMESPACE.to_owned()).expect("valid namespace");

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M18 registrant: connected to {peer_id}");
                swarm
                    .behaviour_mut()
                    .register(ns.clone(), peer_id, None)
                    .expect("register");
                eprintln!("M18 registrant: REGISTER sent");
            }
            SwarmEvent::Behaviour(client::Event::Registered { namespace, ttl, .. }) => {
                eprintln!("M18 registrant: registered in {namespace:?} (ttl={ttl}s)");
                break;
            }
            SwarmEvent::Behaviour(client::Event::RegisterFailed { namespace, error, .. }) => {
                panic!("M18 registrant: register failed in {namespace:?}: {error:?}");
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("M18 registrant: dial failed: {error}");
            }
            ev => {
                eprintln!("M18 registrant event: {ev:?}");
            }
        }
    }

    eprintln!("M18 registrant: PASS");
}

// ── Discoverer ────────────────────────────────────────────────────────────────

async fn run_discoverer() {
    let server_addr: libp2p_core::Multiaddr = std::env::var("SERVER_ADDR")
        .expect("SERVER_ADDR not set")
        .parse()
        .expect("invalid SERVER_ADDR");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M18 discoverer: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);
    let behaviour = RendezvousBehaviour::new(keypair.clone());
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, SwarmConfig::with_executor(WstdExecutor));

    eprintln!("M18 discoverer: dialling {server_addr}");
    swarm.dial(server_addr).expect("dial");

    let ns = Namespace::new(NAMESPACE.to_owned()).expect("valid namespace");

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M18 discoverer: connected to {peer_id}");
                swarm
                    .behaviour_mut()
                    .discover(Some(ns.clone()), None, None, peer_id);
                eprintln!("M18 discoverer: DISCOVER sent");
            }
            SwarmEvent::Behaviour(client::Event::Discovered { registrations, .. }) => {
                eprintln!(
                    "M18 discoverer: found {} registration(s)",
                    registrations.len()
                );
                for reg in &registrations {
                    eprintln!("  - peer: {}", reg.record.peer_id());
                }
                assert!(
                    !registrations.is_empty(),
                    "M18 discoverer: expected at least one registration"
                );
                break;
            }
            SwarmEvent::Behaviour(client::Event::DiscoverFailed { error, .. }) => {
                panic!("M18 discoverer: discover failed: {error:?}");
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("M18 discoverer: dial failed: {error}");
            }
            ev => {
                eprintln!("M18 discoverer event: {ev:?}");
            }
        }
    }

    eprintln!("M18 discoverer: PASS");
}
