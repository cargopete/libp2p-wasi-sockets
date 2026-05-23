//! M12 integration test: WASM-to-WASM direct connection — listener side.
//!
//! Reads `LISTEN_PORT` from the environment, binds a WasiTcpTransport listener,
//! and breaks on the first `ConnectionEstablished` event from any peer.
//! No native peer is involved — the dialer is the `p2p-dialer` WASM component.

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
    let listen_port: u16 = std::env::var("LISTEN_PORT")
        .expect("LISTEN_PORT not set")
        .parse()
        .expect("invalid LISTEN_PORT");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M12 p2p-listener: local peer ID = {local_peer_id}");

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

    let listen_addr: libp2p_core::Multiaddr =
        format!("/ip4/0.0.0.0/tcp/{listen_port}").parse().unwrap();
    swarm.listen_on(listen_addr).expect("listen_on");

    let mut got_connection = false;
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                eprintln!("M12 p2p-listener: listening on {address}");
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                eprintln!("M12 p2p-listener: inbound connection from {peer_id}");
                got_connection = true;
                break;
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                panic!("incoming connection error: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M12 p2p-listener: connection closed");
                break;
            }
            ev => {
                eprintln!("M12 p2p-listener event: {ev:?}");
            }
        }
    }

    if !got_connection {
        panic!("M12 p2p-listener: connection closed before ConnectionEstablished");
    }
    eprintln!("M12 p2p-listener: PASS");
}
