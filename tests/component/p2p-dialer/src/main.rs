//! M12 integration test: WASM-to-WASM direct connection — dialer side.
//!
//! Reads `DIAL_ADDR` from the environment, dials the given multiaddr (the
//! `p2p-listener` WASM component), and asserts `ConnectionEstablished` from
//! any peer.  No native peer is involved in the connection.

use std::pin::Pin;

use futures::StreamExt as _;
use libp2p_core::muxing::StreamMuxerBox;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade::Version;
use libp2p_core::Transport as _;
use libp2p_identity::Keypair;
use libp2p_swarm::{dummy, Config as SwarmConfig, Swarm, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;

struct WstdExecutor;

impl libp2p_swarm::Executor for WstdExecutor {
    fn exec(&self, future: Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        wstd::runtime::spawn(future).detach();
    }
}

#[wstd::main]
async fn main() {
    let dial_addr: libp2p_core::Multiaddr = std::env::var("DIAL_ADDR")
        .expect("DIAL_ADDR not set")
        .parse()
        .expect("invalid DIAL_ADDR");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M12 p2p-dialer: local peer ID = {local_peer_id}");

    let noise = libp2p_noise::Config::new(&keypair).expect("noise config");
    let yamux = libp2p_yamux::Config::default();
    let transport: Boxed<(libp2p_identity::PeerId, StreamMuxerBox)> =
        WasiTcpTransport::default()
            .upgrade(Version::V1)
            .authenticate(noise)
            .multiplex(yamux)
            .boxed();

    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, dummy::Behaviour, local_peer_id, config);

    swarm.dial(dial_addr.clone()).expect("dial");
    eprintln!("M12 p2p-dialer: dialling {dial_addr}");

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M12 p2p-dialer: connected to {peer_id}");
                break;
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("dial failed: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M12 p2p-dialer: connection closed");
                break;
            }
            ev => {
                eprintln!("M12 p2p-dialer event: {ev:?}");
            }
        }
    }

    eprintln!("M12 p2p-dialer: PASS");
}
