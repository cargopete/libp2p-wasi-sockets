# libp2p-wasi-sockets

[![CI](https://github.com/cargopete/libp2p-wasi-sockets/actions/workflows/ci.yml/badge.svg)](https://github.com/cargopete/libp2p-wasi-sockets/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/libp2p-wasi-sockets)](https://crates.io/crates/libp2p-wasi-sockets)
[![docs.rs](https://img.shields.io/docsrs/libp2p-wasi-sockets)](https://docs.rs/libp2p-wasi-sockets)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE-MIT)

A [WASI 0.2](https://github.com/WebAssembly/WASI/blob/v0.2.1/README.md) sockets transport for [rust-libp2p](https://github.com/libp2p/rust-libp2p).

Implements `libp2p_core::Transport` over `wasi:sockets/tcp`, enabling rust-libp2p applications to run as **Wasm Components** on any WASI 0.2 host — Wasmtime, Spin, jco, Wasmer — without any modification to the rest of the libp2p stack.

---

## Why does this exist?

Every major rust-libp2p sub-crate (`libp2p-core`, `libp2p-swarm`, `libp2p-noise`, `libp2p-yamux`, `libp2p-gossipsub`, …) **compiles cleanly for `wasm32-wasip2`**. The only missing piece was a transport: `libp2p-tcp` is gated out for all wasm targets, and no `wasi:sockets`-based alternative existed.

This crate closes that gap.

### What you can do with it

- Run libp2p-based protocols as **sandboxed Wasm Components** in Wasmtime or Spin.
- Drop P2P functionality into plugin systems that already embed a WASI 0.2 runtime.
- Leverage the WASI **network capability model** — the host explicitly grants (or denies) network access per component, giving stronger isolation than containers.

### What is deliberately out of scope

- **UDP / QUIC** — `wasi:sockets/udp` doesn't expose the socket controls `quinn` needs.
- **`/dnsaddr`** — TXT-record-based multiaddr discovery; out of scope.
- **Browser** — already covered by `libp2p-websocket-websys` / `libp2p-webtransport-websys`.
- **NAT traversal** — AutoNAT, circuit-relay, DCUtR are not supported by the `wasi-sockets` API.

---

## Status

**v0.5.0** — WASM-to-WASM connections and `libp2p-gossipsub` pub/sub verified on wasm32-wasip2.

| Milestone | Description | Status |
|---|---|---|
| M0 | Scaffold, Cargo.toml, CI, multiaddr utils, unit tests | ✅ |
| M1 | `WasiTcpStream` AsyncRead/AsyncWrite bridge + echo integration test | ✅ |
| M2 | `WasiTcpTransport` listen\_on + dial integration test | ✅ |
| M3 | Multi-listener, AddressExpired / ListenerClosed lifecycle events | ✅ |
| M4 | Noise XX + Yamux upgrade over `WasiTcpTransport` | ✅ |
| M5 | Interop test: WASM component ↔ native rust-libp2p (tokio) | ✅ |
| M6 | Docs polish, examples, 0.1.0 crates.io release | ✅ |
| M7 | DNS multiaddr support: `/dns4`, `/dns6`, `/dns` via `wasi:sockets/ip-name-lookup` | ✅ |
| M8 | libp2p `Swarm` + full upgrade chain (Noise XX + Yamux + PeerId auth) on wasip2 | ✅ |
| M9 | `libp2p-ping` round-trip on wasip2 — WASI-native `futures_timer` via `wasi:clocks` | ✅ |
| M10 | `libp2p-identify` on wasip2 — Wasm component receives `IdentifyEvent::Received` | ✅ |
| M11 | Inbound connections — Wasm component acts as TCP listener, native peer dials in | ✅ |
| M12 | WASM-to-WASM — two Wasm components connect directly, no native peer involved | ✅ |
| M13 | `libp2p-gossipsub` pub/sub — two WASM components exchange a gossipsub message | ✅ |

---

## Quick start

```toml
# Cargo.toml — do NOT enable the `tcp` feature on the umbrella `libp2p` crate
[dependencies]
libp2p-wasi-sockets = "0.3"
libp2p-core         = { version = "0.43", default-features = false }
libp2p-noise        = "0.46"
libp2p-yamux        = "0.47"
libp2p-ping         = "0.47"
libp2p-swarm        = "0.47"
libp2p-identity     = { version = "0.2", features = ["ed25519", "rand"] }
wstd                = "0.6"
futures             = { version = "0.3", default-features = false, features = ["std", "async-await"] }

# Replace the thread-spawning futures-timer with the WASI-native version.
# Required for any behaviour that uses futures_timer::Delay (ping, swarm
# idle timeout, etc.).  See crates/futures-timer-wasi in the libp2p-wasi-sockets
# repository, or vendor your own.
[patch.crates-io]
futures-timer = { git = "https://github.com/cargopete/libp2p-wasi-sockets", tag = "v0.3.0", package = "futures-timer" }
```

```rust
use std::pin::Pin;
use std::time::Duration;
use futures::StreamExt as _;
use libp2p_core::{muxing::StreamMuxerBox, transport::Boxed, upgrade::Version, Transport as _};
use libp2p_identity::Keypair;
use libp2p_swarm::{Config as SwarmConfig, Executor, Swarm, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;

struct WstdExecutor;
impl Executor for WstdExecutor {
    fn exec(&self, fut: Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        wstd::runtime::spawn(fut).detach();
    }
}

#[wstd::main]
async fn main() {
    let keypair = Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();

    let transport: Boxed<(_, StreamMuxerBox)> = WasiTcpTransport::default()
        .upgrade(Version::V1)
        .authenticate(libp2p_noise::Config::new(&keypair).unwrap())
        .multiplex(libp2p_yamux::Config::default())
        .boxed();

    let behaviour = libp2p_ping::Behaviour::new(
        libp2p_ping::Config::new().with_interval(Duration::from_secs(15)),
    );
    let mut swarm = Swarm::new(transport, behaviour, peer_id,
                               SwarmConfig::with_executor(WstdExecutor));

    swarm.dial("/ip4/1.2.3.4/tcp/4001".parse().unwrap()).unwrap();

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => eprintln!("connected: {peer_id}"),
            SwarmEvent::Behaviour(libp2p_ping::Event { peer, result: Ok(rtt), .. }) => {
                eprintln!("ping {peer}: {rtt:?}");
            }
            SwarmEvent::ConnectionClosed { .. } => break,
            _ => {}
        }
    }
}
```

Build and run:

```bash
rustup target add wasm32-wasip2

cargo build --release --target wasm32-wasip2

# -S inherit-network grants the component access to the host network.
# Without it, all dials return Error::AccessDenied.
wasmtime run -S inherit-network ./target/wasm32-wasip2/release/my_app.wasm
```

---

## Supported multiaddrs

| Pattern | Status |
|---|---|
| `/ip4/<addr>/tcp/<port>` | ✅ |
| `/ip6/<addr>/tcp/<port>` | ✅ |
| `…/p2p/<peer-id>` suffix | ✅ stripped, not dialled |
| `/ip4/0.0.0.0/tcp/0` (ephemeral port) | ✅ |
| `/ip6/::/tcp/0` (ephemeral port) | ✅ |
| `/dns4/<host>/tcp/<port>` | ✅ resolves via `wasi:sockets/ip-name-lookup` |
| `/dns6/<host>/tcp/<port>` | ✅ resolved (IPv6 TCP requires wstd ≥ next) |
| `/dns/<host>/tcp/<port>` | ✅ first address of either family |
| `/dnsaddr` | ❌ TXT-record discovery; out of scope |
| `/quic-v1`, `/ws`, `/wss`, `/webrtc` | ❌ out of scope |

---

## Architecture

```
┌─────────────────────────────────────────┐
│  libp2p Swarm (user code)               │
│  Behaviour: ping / gossipsub / kad / …  │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────┴──────────────────────┐
│  Yamux (muxer) · Noise (security)       │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────┴──────────────────────┐
│  libp2p-wasi-sockets  (this crate)      │
│                                         │
│  WasiTcpTransport  impl Transport       │
│  WasiTcpStream     impl AsyncRead       │
│                         AsyncWrite      │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────┴──────────────────────┐
│  wstd 0.6  (wasi:sockets/tcp wrapper)   │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────┴──────────────────────┐
│  WASI 0.2 host (Wasmtime / Spin / …)    │
└─────────────────────────────────────────┘
```

### Async bridge

`wstd`'s `TcpStream::read` / `write` are `async fn`s; `futures::io::AsyncRead` / `AsyncWrite` are poll-based. The bridge works by boxing each in-flight wstd operation as a `Pin<Box<dyn Future>>` and re-polling it on subsequent `poll_*` calls. On `wasm32-wasip2` there are no threads, so the `Send` bound is satisfied via `unsafe impl` — safe because WASI resource handles are plain integers.

---

## Pitfalls

### Behaviours that rely on `futures_timer` need a WASI-compatible patch

Several libp2p behaviours (`libp2p-ping`, `libp2p-swarm`'s idle-connection timeout, etc.) use [`futures_timer`](https://crates.io/crates/futures-timer) internally. On `wasm32-wasip2`, the upstream `futures_timer` crate falls back to its **native** path, which tries to spawn a background timer thread. Thread spawning is not supported by the WASI component model, so the spawn fails silently and any future that polls a `Delay` panics with "timer has gone away".

**The fix**: patch `futures_timer` with a WASI-native implementation that uses `wasi:clocks/monotonic-clock::subscribe_duration` instead of a background thread. This crate ships `crates/futures-timer-wasi/` for exactly this purpose — add it to your `[patch.crates-io]` as shown in the Quick Start above.

### Do not pull in `libp2p-tcp`

The umbrella `libp2p` crate gates `libp2p-tcp` off for all wasm targets, but if you pull it in manually the build will fail. Depend on sub-crates directly and verify:

```bash
cargo tree --target wasm32-wasip2 | grep libp2p-tcp
# must produce no output
```

### Wasmtime network permissions

Wasmtime denies all network access by default:

```bash
# Grant full network access (development)
wasmtime run -S inherit-network my_app.wasm

# Grant only specific addresses (production)
wasmtime run --wasi tcp=127.0.0.1:4001 my_app.wasm
```

### DNS requires an explicit opt-in

`inherit_network` does **not** enable hostname resolution. When embedding Wasmtime
programmatically, call `allow_ip_name_lookup(true)` on `WasiCtxBuilder`:

```rust
let wasi = WasiCtxBuilder::new()
    .inherit_network()
    .allow_ip_name_lookup(true)  // required for /dns4, /dns6, /dns multiaddrs
    .build();
```

Without it, any dial to a DNS multiaddr will fail with a `wasi:sockets/ip-name-lookup`
permission error.

---

## Requirements

- Rust **1.83+**
- Target: `wasm32-wasip2`
- WASI 0.2 host: Wasmtime ≥ 44, Spin ≥ 3.5, or any WASI 0.2.1-compatible runtime

---

## License

Licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option. This matches the license of rust-libp2p.
