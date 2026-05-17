//! Integration tests that build wasm32-wasip2 component binaries and run them
//! under Wasmtime.
//!
//! These tests require:
//!   - `wasm32-wasip2` rustup target installed
//!   - `cargo` on `$PATH`
//!   - Wasmtime 44+ (pulled in as a dev-dependency; no CLI install needed)
//!
//! Run with:
//!   `cargo test --test integration`

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use wasmtime::{
    Config, Engine, Store,
    component::{Component, Linker},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

// ── WASI host state ──────────────────────────────────────────────────────────

struct State {
    ctx: WasiCtx,
    table: wasmtime::component::ResourceTable,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

// ── Component build helper ───────────────────────────────────────────────────

/// Build a component binary for `wasm32-wasip2` and return its path.
///
/// Output is placed in `target/component-tests/<name>/wasm32-wasip2/debug/<name>.wasm`
/// (relative to the workspace root) so it doesn't pollute the component's own
/// `target/` directory.
fn build_component(name: &str) -> Result<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let manifest = format!("{manifest_dir}/tests/component/{name}/Cargo.toml");
    let target_dir = format!("{manifest_dir}/target/component-tests/{name}");

    let status = Command::new("cargo")
        .args([
            "build",
            "--target",
            "wasm32-wasip2",
            "--manifest-path",
            &manifest,
            "--target-dir",
            &target_dir,
        ])
        .status()
        .with_context(|| format!("cargo build for component '{name}'"))?;

    if !status.success() {
        bail!("build failed for component '{name}'");
    }

    let wasm = PathBuf::from(format!(
        "{target_dir}/wasm32-wasip2/debug/{name}.wasm"
    ));

    if !wasm.exists() {
        bail!(
            "expected wasm at '{}' but it does not exist",
            wasm.display()
        );
    }

    Ok(wasm)
}

/// Run a pre-built component under Wasmtime with `inherit_network`.
///
/// Returns `Ok(())` if the component exits with code 0, `Err` otherwise.
async fn run_component(wasm_path: &std::path::Path) -> Result<()> {
    run_component_with_env(wasm_path, &[]).await
}

/// Like `run_component` but also injects the given environment variables.
async fn run_component_with_env(
    wasm_path: &std::path::Path,
    env: &[(&str, &str)],
) -> Result<()> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    // async_support is always-on in wasmtime ≥ 44; no call needed.
    let engine = Engine::new(&config)?;

    let component = Component::from_file(&engine, wasm_path)?;

    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdio().inherit_network();
    for (k, v) in env {
        builder.env(k, v);
    }
    let wasi = builder.build();

    let mut store = Store::new(
        &engine,
        State {
            ctx: wasi,
            table: wasmtime::component::ResourceTable::new(),
        },
    );

    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

    let command = wasmtime_wasi::p2::bindings::Command::instantiate_async(
        &mut store,
        &component,
        &linker,
    )
    .await?;

    command
        .wasi_cli_run()
        .call_run(&mut store)
        .await?
        .map_err(|()| anyhow::anyhow!("component exited with non-zero status"))
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// M1 — WasiTcpStream AsyncRead/AsyncWrite bridge.
///
/// Builds and runs `tests/component/echo/` under Wasmtime.  The component
/// opens a loopback listener, dials it, and performs an echo round-trip using
/// our `WasiTcpStream` wrapper.  Any assertion failure inside the component
/// causes a non-zero exit code which surfaces here as an `Err`.
#[tokio::test]
async fn m1_echo_stream() -> Result<()> {
    let wasm = build_component("echo")?;
    run_component(&wasm).await
}

/// M2 — WasiTcpTransport listen_on + dial.
///
/// Builds and runs `tests/component/transport/` under Wasmtime.  The component
/// calls `Transport::listen_on`, discovers the ephemeral port via
/// `Transport::poll`, dials it, and exchanges bytes over the resulting
/// `WasiTcpStream` pair.
#[tokio::test]
async fn m2_transport_dial() -> Result<()> {
    let wasm = build_component("transport")?;
    run_component(&wasm).await
}

/// M3 — Multi-listener + AddressExpired / ListenerClosed lifecycle events.
///
/// Builds and runs `tests/component/multi/` under Wasmtime.  The component
/// binds two listeners, exchanges bytes through one, removes the other, and
/// asserts the correct sequence of `AddressExpired` → `ListenerClosed` events.
#[tokio::test]
async fn m3_multi_listener() -> Result<()> {
    let wasm = build_component("multi")?;
    run_component(&wasm).await
}

/// M4 — Noise XX + Yamux upgrade on top of WasiTcpTransport.
///
/// Builds and runs `tests/component/noise/` under Wasmtime.  The component
/// generates Ed25519 keypairs, performs the full Noise XX handshake and Yamux
/// negotiation via the libp2p upgrade builder, asserts peer IDs, then
/// exchanges an echo over a Yamux substream.
#[tokio::test]
async fn m4_noise_yamux() -> Result<()> {
    let wasm = build_component("noise")?;
    run_component(&wasm).await
}

/// M5 — Interop: WasiTcpTransport (WASM) ↔ native rust-libp2p (tokio).
///
/// The Wasmtime harness spins up a native tokio TCP + Noise XX + Yamux listener
/// using raw upgrades (no libp2p-tcp dependency), then runs
/// `tests/component/interop/` alongside it.  The WASM component dials the
/// native peer and exchanges a round-trip echo over a Yamux substream.
/// Passing proves full protocol compatibility at the Noise + Yamux layer.
#[tokio::test]
async fn m5_interop() -> Result<()> {
    use std::pin::Pin;
    use std::task::Poll;

    use futures::future::poll_fn;
    use futures::io::{AsyncRead, AsyncWrite};
    use libp2p_core::muxing::StreamMuxer;
    use libp2p_core::upgrade::{InboundConnectionUpgrade, UpgradeInfo};
    use libp2p_identity::Keypair;
    use multistream_select::listener_select_proto;
    use tokio_util::compat::TokioAsyncReadCompatExt;

    // ── Native peer setup ─────────────────────────────────────────────────────
    let native_key = Keypair::generate_ed25519();
    let native_peer_id = native_key.public().to_peer_id();

    let native_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let native_port = native_listener.local_addr()?.port();
    let native_multiaddr: libp2p_core::Multiaddr =
        format!("/ip4/127.0.0.1/tcp/{native_port}").parse().unwrap();

    eprintln!("M5: native listener at {native_multiaddr}");

    // ── Build WASM interop component ──────────────────────────────────────────
    let wasm = build_component("interop")?;

    const INTEROP_MSG: &[u8] = b"hello from WasiTcpTransport M5 interop";

    let env = [
        ("NATIVE_ADDR".to_string(), native_multiaddr.to_string()),
        ("NATIVE_PEER_ID".to_string(), native_peer_id.to_string()),
    ];
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_ref(), v.as_ref())).collect();

    // ── Run WASM client and native echo server concurrently ───────────────────
    let (wasm_result, native_result) = tokio::join!(
        run_component_with_env(&wasm, &env_refs),
        async move {
            // Accept one TCP connection from the WASM client.
            let (tcp_stream, _) = native_listener.accept().await.context("accept")?;
            let compat = tcp_stream.compat();

            // ── Noise XX upgrade (inbound = server / responder) ───────────────
            let noise_cfg =
                libp2p_noise::Config::new(&native_key).context("native noise config")?;
            let (noise_proto, negotiated) =
                listener_select_proto(compat, noise_cfg.protocol_info())
                    .await
                    .map_err(|e| anyhow::anyhow!("noise negotiation: {e}"))?;
            let (_remote_peer_id, noise_stream) = noise_cfg
                .upgrade_inbound(negotiated, noise_proto)
                .await
                .map_err(|e| anyhow::anyhow!("noise upgrade: {e}"))?;

            // ── Yamux upgrade (inbound = server / responder) ──────────────────
            let yamux_cfg = libp2p_yamux::Config::default();
            let (yamux_proto, negotiated2) =
                listener_select_proto(noise_stream, yamux_cfg.protocol_info())
                    .await
                    .map_err(|e| anyhow::anyhow!("yamux negotiation: {e}"))?;
            let mut muxer = yamux_cfg
                .upgrade_inbound(negotiated2, yamux_proto)
                .await
                .map_err(|e| anyhow::anyhow!("yamux upgrade: {e}"))?;

            eprintln!("M5 native: handshake complete");

            // ── Echo handler ──────────────────────────────────────────────────
            //
            // Same combined poll_fn pattern as M4: drive Active::poll via
            // poll_inbound so frames flow between yamux's internal channel and TCP.
            let mut ss_opt: Option<libp2p_yamux::Stream> = None;
            let mut server_pos: usize = 0;
            let mut server_buf = vec![0u8; INTEROP_MSG.len()];
            let mut server_written: usize = 0;
            let mut server_flushed = false;

            poll_fn(|cx| {
                // Step 1: accept inbound stream (DATA+SYN from WASM client).
                if ss_opt.is_none() {
                    match Pin::new(&mut muxer).poll_inbound(cx) {
                        Poll::Ready(Ok(s)) => ss_opt = Some(s),
                        Poll::Ready(Err(e)) => panic!("native poll_inbound: {e}"),
                        Poll::Pending => return Poll::Pending,
                    }
                }

                // Step 2: read MSG (already in receive buffer from DATA+SYN).
                if server_pos < INTEROP_MSG.len() {
                    let ss = ss_opt.as_mut().unwrap();
                    while server_pos < INTEROP_MSG.len() {
                        match Pin::new(&mut *ss)
                            .poll_read(cx, &mut server_buf[server_pos..])
                        {
                            Poll::Ready(Ok(0)) => panic!("native: unexpected EOF"),
                            Poll::Ready(Ok(n)) => server_pos += n,
                            Poll::Ready(Err(e)) => panic!("native read: {e}"),
                            Poll::Pending => break,
                        }
                    }
                }
                if server_pos < INTEROP_MSG.len() {
                    let _ = Pin::new(&mut muxer).poll_inbound(cx);
                    return Poll::Pending;
                }

                // Step 3: write echo into stream channel.
                if server_written < server_buf.len() {
                    let ss = ss_opt.as_mut().unwrap();
                    match Pin::new(&mut *ss).poll_write(cx, &server_buf[server_written..]) {
                        Poll::Ready(Ok(n)) => server_written += n,
                        Poll::Ready(Err(e)) => panic!("native echo write: {e}"),
                        Poll::Pending => {}
                    }
                }
                if server_written == server_buf.len() && !server_flushed {
                    let ss = ss_opt.as_mut().unwrap();
                    match Pin::new(&mut *ss).poll_flush(cx) {
                        Poll::Ready(Ok(())) => server_flushed = true,
                        Poll::Ready(Err(e)) => panic!("native echo flush: {e}"),
                        Poll::Pending => {}
                    }
                }

                // Step 4: drive muxer — flushes echo channel → TCP, then
                // keeps the connection alive until the WASM client closes it
                // (after reading the echo).  Returning Poll::Ready before the
                // remote closes would drop the muxer and reset the TCP socket
                // before the WASM side can read the echo data.
                match Pin::new(&mut muxer).poll_inbound(cx) {
                    Poll::Ready(Err(_)) if server_flushed => {
                        eprintln!("M5 native: echo sent, connection closed by remote");
                        Poll::Ready(Ok::<(), anyhow::Error>(()))
                    }
                    Poll::Ready(Err(e)) => {
                        panic!("native muxer error before echo was flushed: {e}")
                    }
                    Poll::Ready(Ok(s)) => {
                        drop(s); // unexpected extra stream, ignore
                        Poll::Pending
                    }
                    Poll::Pending => Poll::Pending,
                }
            })
            .await
        }
    );

    wasm_result.context("WASM component failed")?;
    native_result.context("native echo handler failed")?;

    Ok(())
}
