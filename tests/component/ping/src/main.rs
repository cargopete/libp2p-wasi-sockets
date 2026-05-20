//! M9 integration test: libp2p Ping over WasiTcpTransport.
//!
//! Reads `NATIVE_ADDR` and `NATIVE_PEER_ID` from the environment, dials the
//! native peer through the full Swarm upgrade chain (Noise XX + Yamux), and
//! asserts that a successful ping RTT is observed.
//!
//! `futures_timer` is patched via `[patch.crates-io]` in Cargo.toml to use
//! `wasi:clocks/monotonic-clock` instead of a background thread, enabling
//! `libp2p_ping::Behaviour` to work on wasm32-wasip2.

use std::pin::Pin;
use std::time::Duration;

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
    eprintln!("M9 ping: local peer ID = {local_peer_id}");

    let noise = libp2p_noise::Config::new(&keypair).expect("noise config");
    let yamux = libp2p_yamux::Config::default();
    let transport: Boxed<(PeerId, StreamMuxerBox)> = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(noise)
        .multiplex(yamux)
        .boxed();

    let ping_cfg = libp2p_ping::Config::new()
        .with_interval(Duration::from_millis(500))
        .with_timeout(Duration::from_secs(5));
    let behaviour = libp2p_ping::Behaviour::new(ping_cfg);
    let config = SwarmConfig::with_executor(WstdExecutor);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, config);

    swarm.dial(native_addr.clone()).expect("dial");
    eprintln!("M9 ping: dialling {native_addr}");

    let mut got_ping = false;
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::Behaviour(libp2p_ping::Event {
                peer,
                result: Ok(rtt),
                ..
            }) if peer == native_peer_id => {
                eprintln!("M9 ping: RTT to {peer} = {rtt:?}");
                got_ping = true;
                break;
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                panic!("dial failed: {error}");
            }
            // Exit the loop when the connection closes so the wstd reactor
            // can drain cleanly rather than waiting forever for swarm events
            // that will never arrive on a closed connection.
            SwarmEvent::ConnectionClosed { .. } => {
                eprintln!("M9 ping: connection closed");
                break;
            }
            ev => {
                eprintln!("M9 ping event: {ev:?}");
            }
        }
    }

    if !got_ping {
        panic!("M9 ping: connection closed before ping RTT was received");
    }
    eprintln!("M9 ping: PASS");
}
