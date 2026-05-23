//! M14 integration test: libp2p Kademlia DHT over WasiTcpTransport.
//!
//! Two WASM components form a 2-node Kademlia DHT over direct TCP.
//! The same binary runs as either a provider or a seeker, selected via `MODE`:
//!
//! - `MODE=provide` — binds on `LISTEN_PORT`, pre-stores record
//!                    `("kad-key", b"hello M14")` in its local MemoryStore,
//!                    serves the seeker's GET_VALUE query automatically, then
//!                    exits after the seeker disconnects.
//!
//! - `MODE=seek`    — dials `PROVIDER_ADDR`, calls `get_record("kad-key")`
//!                    once the connection is established, and asserts that the
//!                    returned value equals b"hello M14".

use std::pin::Pin;

use futures::StreamExt as _;
use libp2p_core::muxing::StreamMuxerBox;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade::Version;
use libp2p_core::Transport as _;
use libp2p_identity::{Keypair, PeerId};
use libp2p_kad::{store::{MemoryStore, RecordStore as _}, GetRecordOk, QueryResult, Record, RecordKey};
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

#[wstd::main]
async fn main() {
    match std::env::var("MODE").as_deref().unwrap_or("provide") {
        "seek" => run_seeker().await,
        _ => run_provider().await,
    }
}

async fn run_provider() {
    let listen_port: u16 = std::env::var("LISTEN_PORT")
        .expect("LISTEN_PORT not set")
        .parse()
        .expect("invalid LISTEN_PORT");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M14 provider: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);

    let store = MemoryStore::new(local_peer_id);
    let mut kad = libp2p_kad::Behaviour::new(local_peer_id, store);

    // Pre-store the record that the seeker will query for.
    let key = RecordKey::new(&b"kad-key");
    let record = Record {
        key: key.clone(),
        value: b"hello M14".to_vec(),
        publisher: Some(local_peer_id),
        expires: None,
    };
    kad.store_mut().put(record).expect("store record");
    eprintln!("M14 provider: stored record for key {key:?}");

    // Provider must be in Server mode so it accepts inbound Kademlia streams.
    // In Mode::Client (the default) listen_protocol() returns DeniedUpgrade,
    // which rejects inbound requests and causes the seeker's query to time out.
    kad.set_mode(Some(libp2p_kad::Mode::Server));

    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, kad, local_peer_id, config);

    let listen_addr: libp2p_core::Multiaddr =
        format!("/ip4/0.0.0.0/tcp/{listen_port}").parse().unwrap();
    swarm.listen_on(listen_addr).expect("listen_on");

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                eprintln!("M14 provider: listening on {address}");
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M14 provider: seeker {peer_id} connected");
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                eprintln!("M14 provider: seeker {peer_id} disconnected");
                break;
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                panic!("incoming connection error: {error}");
            }
            ev => {
                eprintln!("M14 provider event: {ev:?}");
            }
        }
    }

    eprintln!("M14 provider: PASS");
}

async fn run_seeker() {
    let provider_addr: libp2p_core::Multiaddr = std::env::var("PROVIDER_ADDR")
        .expect("PROVIDER_ADDR not set")
        .parse()
        .expect("invalid PROVIDER_ADDR");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M14 seeker: local peer ID = {local_peer_id}");

    let transport = build_transport(&keypair);

    let store = MemoryStore::new(local_peer_id);
    let kad = libp2p_kad::Behaviour::new(local_peer_id, store);

    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, kad, local_peer_id, config);

    swarm.dial(provider_addr.clone()).expect("dial");
    eprintln!("M14 seeker: dialling {provider_addr}");

    let key = RecordKey::new(&b"kad-key");
    let mut queried = false;
    let mut got_record = false;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                eprintln!("M14 seeker: connected to {peer_id}");
                // add_address registers the peer; in libp2p-kad 0.48 it checks
                // connected_peers (already populated before ConnectionEstablished
                // fires) and inserts the peer as NodeStatus::Connected, so
                // get_record can find it immediately.
                let addr = endpoint.get_remote_address().clone();
                swarm.behaviour_mut().add_address(&peer_id, addr);
                if !queried {
                    eprintln!("M14 seeker: issuing get_record");
                    swarm.behaviour_mut().get_record(key.clone());
                    queried = true;
                }
            }
            SwarmEvent::Behaviour(libp2p_kad::Event::RoutingUpdated { peer, .. })
                if !queried =>
            {
                eprintln!("M14 seeker: routing table updated for {peer}, issuing get_record");
                swarm.behaviour_mut().get_record(key.clone());
                queried = true;
            }
            SwarmEvent::Behaviour(libp2p_kad::Event::OutboundQueryProgressed {
                result: QueryResult::GetRecord(Ok(GetRecordOk::FoundRecord(peer_record))),
                ..
            }) => {
                eprintln!(
                    "M14 seeker: got record, value={:?}",
                    String::from_utf8_lossy(&peer_record.record.value)
                );
                assert_eq!(
                    peer_record.record.value,
                    b"hello M14",
                    "unexpected record value"
                );
                got_record = true;
                break;
            }
            SwarmEvent::Behaviour(libp2p_kad::Event::OutboundQueryProgressed {
                result: QueryResult::GetRecord(Err(e)),
                ..
            }) => {
                panic!("M14 seeker: get_record failed: {e:?}");
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("dial failed: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M14 seeker: connection closed");
                break;
            }
            ev => {
                eprintln!("M14 seeker event: {ev:?}");
            }
        }
    }

    if !got_record {
        panic!("M14 seeker: connection closed before record was retrieved");
    }
    eprintln!("M14 seeker: PASS");
}
