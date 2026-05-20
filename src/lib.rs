//! WASI 0.2 sockets transport for rust-libp2p.
//!
//! This crate implements [`libp2p_core::Transport`] over the `wasi:sockets/tcp` interface,
//! enabling rust-libp2p applications to run as Wasm Components on any WASI 0.2 host
//! (Wasmtime, Spin, jco, Wasmer) without modification to the rest of the libp2p stack.
//!
//! # Supported multiaddrs
//!
//! | Pattern | Supported |
//! |---|---|
//! | `/ip4/<addr>/tcp/<port>[/p2p/<peer-id>]` | ✅ |
//! | `/ip6/<addr>/tcp/<port>[/p2p/<peer-id>]` | ✅ |
//! | `/dns4/<host>/tcp/<port>` | ✅ resolved via `wasi:sockets/ip-name-lookup` |
//! | `/dns6/<host>/tcp/<port>` | ✅ resolved (IPv6 TCP requires wstd ≥ next) |
//! | `/dns/<host>/tcp/<port>` | ✅ first address of either family |
//!
//! # Not supported
//!
//! - `/dnsaddr` — TXT-record-based multiaddr discovery; out of scope.
//! - `/quic-v1`, `/ws`, `/wss`, `/webrtc`, `/p2p-circuit` — out of scope.
//! - UDP — out of scope.
//!
//! # DNS permissions
//!
//! DNS resolution requires the WASI host to grant `ip-name-lookup` access.
//! Under Wasmtime, pass `-S inherit-network` **and** add `allow_ip_name_lookup(true)`
//! when constructing [`WasiCtxBuilder`] (it is disabled by default even with
//! `inherit_network`).
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

mod dns;
mod error;
mod multiaddr;
mod stream;
mod transport;

pub use error::Error;
pub use stream::WasiTcpStream;
pub use transport::{Config, WasiTcpTransport};
