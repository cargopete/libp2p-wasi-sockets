//! WASI 0.2 sockets transport for rust-libp2p.
//!
//! This crate implements [`libp2p_core::Transport`] over the `wasi:sockets/tcp` interface,
//! enabling rust-libp2p applications to run as Wasm Components on any WASI 0.2 host
//! (Wasmtime, Spin, jco, Wasmer) without modification to the rest of the libp2p stack.
//!
//! # Supported multiaddrs
//!
//! - `/ip4/<addr>/tcp/<port>`
//! - `/ip4/<addr>/tcp/<port>/p2p/<peer-id>`
//! - `/ip6/<addr>/tcp/<port>`
//! - `/ip6/<addr>/tcp/<port>/p2p/<peer-id>`
//!
//! # Not supported
//!
//! - `/dns4`, `/dns6`, `/dnsaddr` — DNS resolution via `wasi:sockets/ip-name-lookup` is a
//!   planned follow-up.
//! - `/quic-v1`, `/ws`, `/wss`, `/webrtc`, `/p2p-circuit` — out of scope.
//! - UDP — out of scope for v0.1.
//!
//! # Quick start
//!
//! ```no_run
//! use libp2p_wasi_sockets::WasiTcpTransport;
//!
//! let transport = WasiTcpTransport::default();
//! ```
//!
//! Build and run under Wasmtime:
//!
//! ```bash
//! cargo build --release --target wasm32-wasip2
//! wasmtime run -S inherit-network ./target/wasm32-wasip2/release/my_app.wasm
//! ```
//!
//! If you omit `-S inherit-network`, all dials will fail with
//! [`Error::AccessDenied`] — Wasmtime denies all outbound connections by default.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

mod error;
mod multiaddr;
mod stream;
mod transport;

pub use error::Error;
pub use stream::WasiTcpStream;
pub use transport::{Config, WasiTcpTransport};
