# libp2p-wasi-sockets

A [WASI 0.2](https://wasi.dev/) sockets transport for [rust-libp2p](https://github.com/libp2p/rust-libp2p).

Implements `libp2p_core::Transport` over `wasi:sockets/tcp`, enabling rust-libp2p applications to
run as Wasm Components on any WASI 0.2 host (Wasmtime, Spin, jco, Wasmer) without modification to
the rest of the libp2p stack.

## Status

**Pre-release (v0.1.0-dev).** API is unstable. See [RFC-0001](docs/RFC-0001.md) for the design.

## Quick start

```toml
[dependencies]
libp2p-wasi-sockets = "0.1"
libp2p-swarm        = "0.47"
libp2p-noise        = "0.46"
libp2p-yamux        = "0.47"
libp2p-ping         = "0.47"
wstd                = "0.6"
```

```rust
use libp2p_swarm::{SwarmBuilder, SwarmEvent};
use libp2p_wasi_sockets::WasiTcpTransport;
use std::time::Duration;

#[wstd::main]
async fn main() -> anyhow::Result<()> {
    let keypair = libp2p_identity::Keypair::generate_ed25519();
    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_other_transport(|_| WasiTcpTransport::default())?
        .with_behaviour(|_| libp2p_ping::Behaviour::default())?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => eprintln!("listening on {address}"),
            SwarmEvent::Behaviour(event) => eprintln!("{event:?}"),
            _ => {}
        }
    }
}
```

Build and run:

```bash
cargo build --release --target wasm32-wasip2
wasmtime run -S inherit-network ./target/wasm32-wasip2/release/my_app.wasm
```

> **Note**: `-S inherit-network` is required. Wasmtime denies all network access by default.
> Without it, all dials return `Error::AccessDenied`.

## Supported multiaddrs

| Pattern | Status |
|---|---|
| `/ip4/<addr>/tcp/<port>` | ✅ |
| `/ip6/<addr>/tcp/<port>` | ✅ |
| `…/p2p/<peer-id>` suffix | ✅ (stripped) |
| `/dns4`, `/dns6`, `/dnsaddr` | ❌ (planned) |
| `/quic-v1`, `/ws`, `/wss` | ❌ (out of scope) |

## Requirements

- Rust 1.83+
- Target: `wasm32-wasip2`
- Host: Wasmtime ≥ 44, or any WASI 0.2.1-compatible runtime

```bash
rustup target add wasm32-wasip2
```

## Avoiding `libp2p-tcp`

Do **not** enable the `tcp` feature on the umbrella `libp2p` crate — it will fail to compile.
Depend on sub-crates (`libp2p-swarm`, `libp2p-noise`, etc.) directly.

Verify with:

```bash
cargo tree --target wasm32-wasip2 | grep libp2p-tcp
# should produce no output
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
