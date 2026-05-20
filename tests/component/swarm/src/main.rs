//! M8 integration test: libp2p Swarm over WasiTcpTransport.
//!
//! Reads `NATIVE_ADDR` and `NATIVE_PEER_ID` from the environment, dials the
//! native peer through the full Swarm upgrade chain (Noise XX + Yamux), and
//! asserts that `ConnectionEstablished` fires for the expected peer.
//!
//! `libp2p_ping` is intentionally omitted: it uses `futures_timer` which
//! requires a background thread unavailable in single-threaded wasm32-wasip2.

use std::pin::Pin;

use futures::StreamExt as _;
use libp2p_core::muxing::StreamMuxerBox;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade::Version;
use libp2p_core::Transport as _;
use libp2p_identity::{Keypair, PeerId};
use libp2p_swarm::{dummy, Config as SwarmConfig, Swarm, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;

struct WstdExecutor;

impl libp2p_swarm::Executor for WstdExecutor {
    fn exec(&self, future: Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        // SAFETY: wasm32-wasip2 is single-threaded; futures are never moved
        // across threads even though we assert Send here.
        // `detach()` runs the task to completion without keeping the handle;
        // dropping an async_task::Task would cancel the future.
        wstd::runtime::spawn(future).detach();
    }
}

#[wstd::main]
async fn main() {
    let native_addr: libp2p_core::Multiaddr = std::env::var("NATIVE_ADDR")
        .expect("NATIVE_ADDR not set")
        .parse()
        .expect("invalid NATIVE_ADDR");

    let native_peer_id: PeerId = std::env::var("NATIVE_PEER_ID")
        .expect("NATIVE_PEER_ID not set")
        .parse()
        .expect("invalid NATIVE_PEER_ID");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M8 swarm: local peer ID = {local_peer_id}");

    // Build a Boxed<(PeerId, StreamMuxerBox)> transport via the upgrade builder.
    let noise = libp2p_noise::Config::new(&keypair).expect("noise config");
    let yamux = libp2p_yamux::Config::default();
    let transport: Boxed<(PeerId, StreamMuxerBox)> = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(noise)
        .multiplex(yamux)
        .boxed();

    // dummy::Behaviour has no protocol overhead and no timer dependencies —
    // it is enough to prove the full Swarm upgrade chain works on wasip2.
    let behaviour = dummy::Behaviour;
    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    swarm.dial(native_addr.clone()).expect("dial");
    eprintln!("M8 swarm: dialling {native_addr}");

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == native_peer_id => {
                eprintln!("M8 swarm: connected to {native_peer_id}");
                break;
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("dial failed: {error}");
            }
            ev => {
                eprintln!("M8 swarm event: {ev:?}");
            }
        }
    }

    eprintln!("M8 swarm connect: PASS");
}
