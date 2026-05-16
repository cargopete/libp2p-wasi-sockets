use libp2p_core::multiaddr::Multiaddr;
use libp2p_core::transport::ListenerId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported multiaddr: {0}")]
    UnsupportedMultiaddr(Multiaddr),

    #[error("invalid multiaddr: {0}")]
    InvalidMultiaddr(String),

    #[error("listener {0:?} not found")]
    UnknownListener(ListenerId),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The WASI host's network policy denied the connection.
    ///
    /// Under Wasmtime, pass `-S inherit-network` (or `--wasi inherit-network`)
    /// to grant the component access to the host network.
    #[error("network capability denied by host (did you pass -S inherit-network to wasmtime?)")]
    AccessDenied,
}
