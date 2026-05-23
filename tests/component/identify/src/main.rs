//! M10 integration test: libp2p Identify over WasiTcpTransport.
//!
//! Reads `NATIVE_ADDR` and `NATIVE_PEER_ID` from the environment, dials the
//! native peer through the full Swarm upgrade chain (Noise XX + Yamux), and
//! asserts that `IdentifyEvent::Received` fires with the correct peer ID and
//! public key.

use std::pin::Pin;

use futures::StreamExt as _;
use libp2p_core::muxing::StreamMuxerBox;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade::Version;
use libp2p_core::Transport as _;
use libp2p_identity::{Keypair, PeerId};
use libp2p_swarm::{Config as SwarmConfig, Swarm, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;

struct WstdExecutor;

impl libp2p_swarm::Executor for WstdExecutor {
    fn exec(&self, future: Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
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
    eprintln!("M10 identify: local peer ID = {local_peer_id}");

    let noise = libp2p_noise::Config::new(&keypair).expect("noise config");
    let yamux = libp2p_yamux::Config::default();
    let transport: Boxed<(PeerId, StreamMuxerBox)> = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(noise)
        .multiplex(yamux)
        .boxed();

    let identify_cfg =
        libp2p_identify::Config::new("/ipfs/0.1.0".to_string(), keypair.public());
    let behaviour = libp2p_identify::Behaviour::new(identify_cfg);
    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    swarm.dial(native_addr.clone()).expect("dial");
    eprintln!("M10 identify: dialling {native_addr}");

    let mut got_identify = false;
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::Behaviour(libp2p_identify::Event::Received {
                peer_id,
                ref info,
                ..
            }) if peer_id == native_peer_id => {
                eprintln!(
                    "M10 identify: received info from {peer_id}; agent={}, protocols={:?}",
                    info.agent_version,
                    info.protocols,
                );
                assert_eq!(
                    info.public_key.to_peer_id(),
                    native_peer_id,
                    "public key mismatch"
                );
                got_identify = true;
                break;
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("dial failed: {error}");
            }
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M10 identify: connection closed");
                break;
            }
            ev => {
                eprintln!("M10 identify event: {ev:?}");
            }
        }
    }

    if !got_identify {
        panic!("M10 identify: connection closed before IdentifyEvent::Received");
    }
    eprintln!("M10 identify: PASS");
}
