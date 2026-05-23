//! M11 integration test: inbound libp2p Swarm connection over WasiTcpTransport.
//!
//! Reads `LISTEN_PORT` and `NATIVE_PEER_ID` from the environment, binds a
//! WasiTcpTransport listener on that port, and asserts that a native peer
//! connects and `ConnectionEstablished` fires for the expected peer ID.
//!
//! This exercises the inbound (server) path of WasiTcpTransport and proves
//! that Wasm components can act as libp2p listeners, not just dialers.

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
        wstd::runtime::spawn(future).detach();
    }
}

#[wstd::main]
async fn main() {
    let listen_port: u16 = std::env::var("LISTEN_PORT")
        .expect("LISTEN_PORT not set")
        .parse()
        .expect("invalid LISTEN_PORT");

    let native_peer_id: PeerId = std::env::var("NATIVE_PEER_ID")
        .expect("NATIVE_PEER_ID not set")
        .parse()
        .expect("invalid NATIVE_PEER_ID");

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    eprintln!("M11 listener: local peer ID = {local_peer_id}");

    let noise = libp2p_noise::Config::new(&keypair).expect("noise config");
    let yamux = libp2p_yamux::Config::default();
    let transport: Boxed<(PeerId, StreamMuxerBox)> = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(noise)
        .multiplex(yamux)
        .boxed();

    let behaviour = dummy::Behaviour;
    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    let listen_addr: libp2p_core::Multiaddr =
        format!("/ip4/0.0.0.0/tcp/{listen_port}").parse().unwrap();
    swarm.listen_on(listen_addr).expect("listen_on");

    let mut got_connection = false;
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                eprintln!("M11 listener: listening on {address}");
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. }
                if peer_id == native_peer_id =>
            {
                eprintln!("M11 listener: inbound connection from {peer_id}");
                got_connection = true;
                break;
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                panic!("incoming connection error: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M11 listener: connection closed");
                break;
            }
            ev => {
                eprintln!("M11 listener event: {ev:?}");
            }
        }
    }

    if !got_connection {
        panic!("M11 listener: connection closed before ConnectionEstablished");
    }
    eprintln!("M11 listener: PASS");
}
