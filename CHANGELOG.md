# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.0] — 2026-05-20

### Added

- **M7** — DNS multiaddr support: `WasiTcpTransport::dial` now resolves `/dns4/<host>`,
  `/dns6/<host>`, and `/dns/<host>` multiaddrs via `wasi:sockets/ip-name-lookup`.
  Resolution is fully async — the `ResolveAddressStream` pollable is driven through wstd's
  `AsyncPollable` reactor with no blocking. Address-family filtering (`dns4` → IPv4 only,
  `dns6` → IPv6 only) is applied before connecting. IPv6 TCP connections are guarded with
  a clear error until wstd adds IPv6 connect support.
  `wasip2` added as a `wasm32`-only dependency to access the raw WASI bindings.
  Integration test: WASM component binds an ephemeral listener then dials itself via
  `/dns4/localhost/tcp/<port>`, exercising the full resolution → connect path.

### Notes

- Wasmtime requires `allow_ip_name_lookup(true)` on `WasiCtxBuilder` for DNS to work;
  `inherit_network()` alone is insufficient.
- `/dnsaddr` (libp2p TXT-record-based multiaddr discovery) remains unsupported.

## [0.1.0] — 2026-05-17

### Added

- **M0** — Initial scaffold: `WasiTcpTransport`, `WasiTcpStream`, `Config`, `Error`,
  multiaddr conversion utilities (`/ip4` and `/ip6` TCP), unit tests, and GitHub Actions CI.

- **M1** — `WasiTcpStream`: `futures::io::AsyncRead` + `AsyncWrite` bridge over
  `wstd::net::TcpStream`. Each in-flight async read/write is boxed as a non-Send
  `Pin<Box<dyn Future>>` and re-polled on subsequent calls. Integration test: Wasmtime
  harness builds and runs an echo component that connects to itself over loopback.

- **M2** — `WasiTcpTransport::listen_on` + `dial`: full `libp2p_core::Transport` impl.
  Listeners bind asynchronously via `wstd::net::TcpListener::bind`; accepts are driven
  by `Transport::poll`. Integration test: component binds a listener, discovers the
  ephemeral port, dials it, and exchanges bytes over `WasiTcpStream`.

- **M3** — Multi-listener support with correct `AddressExpired` → `ListenerClosed` lifecycle
  events. `remove_listener` initiates a graceful close; the next `poll` emits the pair of
  events and removes the entry. Integration test: two listeners, byte exchange through one,
  removal of the other, event sequence verified.

- **M4** — Noise XX + Yamux upgrade over `WasiTcpTransport` using the standard
  libp2p upgrade builder (`.upgrade(Version::V1).authenticate(noise).multiplex(yamux)`).
  Key finding: yamux 0.13 is lazy — the SYN is bundled with the first DATA frame, so
  `poll_write` must precede `poll_inbound` to flush the frame to TCP. Integration test:
  Ed25519 keypair generation, full Noise XX handshake, peer ID assertion, Yamux substream
  echo.

- **M5** — Interop integration test: WASM component running under Wasmtime dials a native
  rust-libp2p peer (raw tokio TCP + Noise XX + Yamux, no `libp2p-tcp` dependency) and
  exchanges a round-trip echo over a Yamux substream. Proves full protocol compatibility
  at the Noise + Yamux layer between the WASM transport and native rust-libp2p.

- **M6** — Docs polish: updated README status table, added docs.rs badge, corrected install
  snippet to use crates.io version. CHANGELOG. `documentation` field in `Cargo.toml`.
