// Unit tests — run on the host with `cargo test`.
// No socket I/O; purely exercising the multiaddr logic and config defaults.

use libp2p_wasi_sockets::{Config, WasiTcpTransport};

#[test]
fn default_config_nodelay() {
    let config = Config::default();
    assert!(config.nodelay);
}

#[test]
fn default_config_no_keepalive() {
    let config = Config::default();
    assert!(config.keep_alive.is_none());
}

#[test]
fn default_config_backlog() {
    let config = Config::default();
    assert_eq!(config.listen_backlog, 128);
}

#[test]
fn transport_default_constructs() {
    let _t = WasiTcpTransport::default();
}
