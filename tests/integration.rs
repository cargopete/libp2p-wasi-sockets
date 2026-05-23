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
    run_component_with_options(wasm_path, env, false).await
}

/// Like `run_component` but also enables `ip_name_lookup` (required for DNS).
///
/// DNS lookup is disabled by default in wasmtime-wasi even with `inherit_network`;
/// this opt-in is required for components that resolve hostnames.
async fn run_component_with_dns(wasm_path: &std::path::Path) -> Result<()> {
    run_component_with_options(wasm_path, &[], true).await
}

async fn run_component_with_options(
    wasm_path: &std::path::Path,
    env: &[(&str, &str)],
    allow_dns: bool,
) -> Result<()> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    // async_support is always-on in wasmtime ≥ 44; no call needed.
    let engine = Engine::new(&config)?;

    let component = Component::from_file(&engine, wasm_path)?;

    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdio().inherit_network();
    if allow_dns {
        builder.allow_ip_name_lookup(true);
    }
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

/// M7 — DNS multiaddr dial: `/dns4/localhost/tcp/<port>`.
///
/// Builds and runs `tests/component/dns/` under Wasmtime with
/// `allow_ip_name_lookup` enabled.  The WASM component binds an ephemeral
/// listener on `127.0.0.1`, then dials itself via `/dns4/localhost/tcp/<port>`.
/// This exercises the full `wasi:sockets/ip-name-lookup` path inside
/// `WasiTcpTransport::dial`.
#[tokio::test]
async fn m7_dns_dial() -> Result<()> {
    let wasm = build_component("dns")?;
    run_component_with_dns(&wasm).await
}

// ── Protobuf / multistream helpers for M10 ───────────────────────────────────

/// Append a protobuf length-delimited field (wire type 2) to `buf`.
fn proto_bytes(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
    proto_varint(buf, (field as u64) << 3 | 2);
    proto_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

/// Append an unsigned varint to `buf` (compatible with both protobuf and
/// `unsigned_varint` / the libp2p length-prefix format).
fn proto_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        if v < 0x80 {
            buf.push(v as u8);
            return;
        }
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
}

/// Build a length-prefixed `Identify` protobuf message for the native peer.
///
/// Fields:
///   1 = publicKey  (protobuf-encoded ed25519 key)
///   3 = protocols  (repeated string)
///   4 = observedAddr (multiaddr bytes — /ip4/127.0.0.1/tcp/0 placeholder)
///   5 = protocolVersion
///   6 = agentVersion
fn build_identify_frame(keypair: &libp2p_identity::Keypair) -> Vec<u8> {
    let pub_key = keypair.public().encode_protobuf();

    let mut msg = Vec::new();
    proto_bytes(&mut msg, 1, &pub_key);
    proto_bytes(&mut msg, 3, b"/ipfs/id/1.0.0");
    proto_bytes(&mut msg, 3, b"/ipfs/ping/1.0.0");
    // /ip4/127.0.0.1/tcp/0 as raw multiaddr bytes
    proto_bytes(&mut msg, 4, &[0x04, 0x7f, 0x00, 0x00, 0x01, 0x06, 0x00, 0x00]);
    proto_bytes(&mut msg, 5, b"ipfs/0.1.0");
    proto_bytes(&mut msg, 6, b"test-native/1.0");

    // Prepend the unsigned_varint length prefix used by the identify framing.
    let mut framed = Vec::new();
    proto_varint(&mut framed, msg.len() as u64);
    framed.extend_from_slice(&msg);
    framed
}

/// M10 — libp2p Identify exchange over WasiTcpTransport.
///
/// Native side: Noise XX + Yamux inbound, accepts the `/ipfs/id/1.0.0`
/// substream, sends a hand-encoded `Identify` protobuf, half-closes the
/// stream, then drives the muxer until WASM closes the connection.
/// WASM side: `libp2p_identify::Behaviour`, breaks on `IdentifyEvent::Received`.
#[tokio::test]
async fn m10_identify() -> Result<()> {
    use std::pin::Pin;
    use std::task::Poll;

    use futures::future::poll_fn;
    use futures::io::AsyncWrite;
    use libp2p_core::muxing::StreamMuxer;
    use libp2p_core::upgrade::{InboundConnectionUpgrade, UpgradeInfo};
    use libp2p_identity::Keypair;
    use multistream_select::listener_select_proto;
    use tokio_util::compat::TokioAsyncReadCompatExt;

    let native_key = Keypair::generate_ed25519();
    let native_peer_id = native_key.public().to_peer_id();

    let native_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let native_port = native_listener.local_addr()?.port();
    let native_multiaddr: libp2p_core::Multiaddr =
        format!("/ip4/127.0.0.1/tcp/{native_port}").parse().unwrap();

    eprintln!("M10: native listener at {native_multiaddr}, peer_id={native_peer_id}");

    let wasm = build_component("identify")?;

    let env = [
        ("NATIVE_ADDR".to_string(), native_multiaddr.to_string()),
        ("NATIVE_PEER_ID".to_string(), native_peer_id.to_string()),
    ];
    let env_refs: Vec<(&str, &str)> =
        env.iter().map(|(k, v)| (k.as_ref(), v.as_ref())).collect();

    let (wasm_result, native_result) = tokio::join!(
        run_component_with_env(&wasm, &env_refs),
        async move {
            let (tcp_stream, _) = native_listener.accept().await.context("accept")?;
            let compat = tcp_stream.compat();

            // Noise XX inbound
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

            // Yamux inbound
            let yamux_cfg = libp2p_yamux::Config::default();
            let (yamux_proto, negotiated2) =
                listener_select_proto(noise_stream, yamux_cfg.protocol_info())
                    .await
                    .map_err(|e| anyhow::anyhow!("yamux negotiation: {e}"))?;
            let mut muxer = yamux_cfg
                .upgrade_inbound(negotiated2, yamux_proto)
                .await
                .map_err(|e| anyhow::anyhow!("yamux upgrade: {e}"))?;

            eprintln!("M10 native: handshake complete, waiting for identify substream");

            let identify_frame = build_identify_frame(&native_key);

            type NegFut = Pin<Box<dyn std::future::Future<
                Output = std::result::Result<
                    (&'static str, multistream_select::Negotiated<libp2p_yamux::Stream>),
                    multistream_select::NegotiationError,
                >,
            >>>;

            let mut stream_opt: Option<libp2p_yamux::Stream> = None;
            let mut neg_fut: Option<NegFut> = None;
            let mut neg_stream: Option<
                multistream_select::Negotiated<libp2p_yamux::Stream>,
            > = None;
            let mut written: usize = 0;
            let mut flushed = false;
            let mut closed = false;

            poll_fn(|cx| {
                // Step 1: accept inbound Yamux stream.
                if stream_opt.is_none() && neg_fut.is_none() && neg_stream.is_none() {
                    match Pin::new(&mut muxer).poll_inbound(cx) {
                        Poll::Ready(Ok(s)) => stream_opt = Some(s),
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("poll_inbound: {e}")))
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }

                // Step 2: negotiate /ipfs/id/1.0.0 (listener = info sender).
                if neg_fut.is_none() && neg_stream.is_none() {
                    if let Some(stream) = stream_opt.take() {
                        neg_fut = Some(Box::pin(listener_select_proto(
                            stream,
                            std::iter::once("/ipfs/id/1.0.0"),
                        )));
                    }
                }

                // Step 3: drive negotiation.
                if let Some(fut) = neg_fut.as_mut() {
                    match fut.as_mut().poll(cx) {
                        Poll::Ready(Ok((_proto, ns))) => {
                            neg_stream = Some(ns);
                            neg_fut = None;
                        }
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("identify negotiation: {e}")))
                        }
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                let Some(ns) = neg_stream.as_mut() else {
                    return Poll::Pending;
                };

                // Step 4: write the length-prefixed Identify protobuf.
                while written < identify_frame.len() {
                    match Pin::new(&mut *ns).poll_write(cx, &identify_frame[written..]) {
                        Poll::Ready(Ok(n)) => written += n,
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("identify write: {e}")))
                        }
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                // Step 5: flush.
                if !flushed {
                    match Pin::new(&mut *ns).poll_flush(cx) {
                        Poll::Ready(Ok(())) => flushed = true,
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("identify flush: {e}")))
                        }
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                // Step 6: half-close the write side (identify protocol requirement).
                // This signals EOF to the WASM reader so it knows the message is complete.
                if !closed {
                    match Pin::new(&mut *ns).poll_close(cx) {
                        Poll::Ready(Ok(())) => closed = true,
                        Poll::Ready(Err(_)) => closed = true, // yamux may error on close; ok
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                // Step 7: drive muxer until WASM closes the connection.
                match Pin::new(&mut muxer).poll_inbound(cx) {
                    Poll::Ready(Err(_)) => {
                        eprintln!("M10 native: identify sent, connection closed by remote");
                        Poll::Ready(Ok::<(), anyhow::Error>(()))
                    }
                    Poll::Ready(Ok(s)) => {
                        drop(s);
                        Poll::Pending
                    }
                    Poll::Pending => Poll::Pending,
                }
            })
            .await?;

            drop(muxer);
            eprintln!("M10 native: done");
            Ok::<(), anyhow::Error>(())
        }
    );

    wasm_result.context("WASM identify component failed")?;
    native_result.context("native identify responder failed")?;
    Ok(())
}

/// M11 — Inbound libp2p Swarm connection: WASM as listener, native as dialer.
///
/// First time the test exercises the inbound path of WasiTcpTransport inside a
/// full Swarm.  The native side uses `OutboundConnectionUpgrade` (Noise XX
/// initiator + Yamux client) and `dialer_select_proto` — the mirror image of
/// all previous tests where native was the responder.
///
/// Port coordination: native pre-allocates an ephemeral port (bind-0 then
/// drop), hands it to WASM via `LISTEN_PORT`, waits 300 ms for WASM to bind,
/// then dials.  The brief sleep is enough on loopback; if the race ever bites
/// the test will fail with ECONNREFUSED and can simply be re-run.
#[tokio::test]
async fn m11_listener() -> Result<()> {
    use std::pin::Pin;
    use std::task::Poll;

    use futures::future::poll_fn;
    use libp2p_core::muxing::StreamMuxer;
    use libp2p_core::upgrade::{OutboundConnectionUpgrade, UpgradeInfo};
    use libp2p_identity::Keypair;
    use multistream_select::{Version, dialer_select_proto};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    let native_key = Keypair::generate_ed25519();
    let native_peer_id = native_key.public().to_peer_id();

    // Grab a free port then immediately release it so WASM can bind it.
    let tmp = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let wasm_port = tmp.local_addr()?.port();
    drop(tmp);

    eprintln!("M11: WASM will listen on port {wasm_port}, native peer_id={native_peer_id}");

    let wasm = build_component("listener")?;

    let env = [
        ("LISTEN_PORT".to_string(), wasm_port.to_string()),
        ("NATIVE_PEER_ID".to_string(), native_peer_id.to_string()),
    ];
    let env_refs: Vec<(&str, &str)> =
        env.iter().map(|(k, v)| (k.as_ref(), v.as_ref())).collect();

    let (wasm_result, native_result) = tokio::join!(
        run_component_with_env(&wasm, &env_refs),
        async move {
            // Give WASM time to bind and start listening.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;

            let tcp = tokio::net::TcpStream::connect(format!("127.0.0.1:{wasm_port}"))
                .await
                .context("connect to WASM listener")?;
            let compat = tcp.compat();

            // Noise XX outbound (initiator)
            let noise_cfg =
                libp2p_noise::Config::new(&native_key).context("native noise config")?;
            let (noise_proto, negotiated) =
                dialer_select_proto(compat, noise_cfg.protocol_info(), Version::V1)
                    .await
                    .map_err(|e| anyhow::anyhow!("noise negotiation: {e}"))?;
            let (_remote_peer_id, noise_stream) = noise_cfg
                .upgrade_outbound(negotiated, noise_proto)
                .await
                .map_err(|e| anyhow::anyhow!("noise upgrade: {e}"))?;

            // Yamux outbound (client)
            let yamux_cfg = libp2p_yamux::Config::default();
            let (yamux_proto, negotiated2) =
                dialer_select_proto(noise_stream, yamux_cfg.protocol_info(), Version::V1)
                    .await
                    .map_err(|e| anyhow::anyhow!("yamux negotiation: {e}"))?;
            let mut muxer = yamux_cfg
                .upgrade_outbound(negotiated2, yamux_proto)
                .await
                .map_err(|e| anyhow::anyhow!("yamux upgrade: {e}"))?;

            eprintln!("M11 native: handshake complete, waiting for WASM to close");

            // Drive the muxer until WASM closes the connection (after it
            // processes ConnectionEstablished and main() returns).
            poll_fn(|cx| {
                match Pin::new(&mut muxer).poll_inbound(cx) {
                    Poll::Ready(Err(_)) => {
                        eprintln!("M11 native: connection closed by WASM");
                        Poll::Ready(Ok::<(), anyhow::Error>(()))
                    }
                    Poll::Ready(Ok(s)) => {
                        drop(s);
                        Poll::Pending
                    }
                    Poll::Pending => Poll::Pending,
                }
            })
            .await?;

            drop(muxer);
            eprintln!("M11 native: done");
            Ok::<(), anyhow::Error>(())
        }
    );

    wasm_result.context("WASM listener component failed")?;
    native_result.context("native dialer failed")?;
    Ok(())
}

/// M9 — libp2p Ping round-trip over WasiTcpTransport.
///
/// Demonstrates that `futures_timer`-based behaviours work on wasm32-wasip2
/// after patching `futures-timer` with a WASI-native implementation backed by
/// `wasi:clocks/monotonic-clock`.
///
/// Native side: Noise XX + Yamux inbound, then accepts the ping substream
/// (`/ipfs/ping/1.0.0`), echoes the 32-byte payload, and drops the connection.
/// WASM side: full `libp2p_ping::Behaviour` Swarm, breaks on first RTT.
#[tokio::test]
async fn m9_ping() -> Result<()> {
    use std::pin::Pin;
    use std::task::Poll;

    use futures::future::poll_fn;
    use futures::io::{AsyncRead, AsyncWrite};
    use libp2p_core::muxing::StreamMuxer;
    use libp2p_core::upgrade::{InboundConnectionUpgrade, UpgradeInfo};
    use libp2p_identity::Keypair;
    use multistream_select::listener_select_proto;
    use tokio_util::compat::TokioAsyncReadCompatExt;

    let native_key = Keypair::generate_ed25519();
    let native_peer_id = native_key.public().to_peer_id();

    let native_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let native_port = native_listener.local_addr()?.port();
    let native_multiaddr: libp2p_core::Multiaddr =
        format!("/ip4/127.0.0.1/tcp/{native_port}").parse().unwrap();

    eprintln!("M9: native listener at {native_multiaddr}, peer_id={native_peer_id}");

    let wasm = build_component("ping")?;

    let env = [
        ("NATIVE_ADDR".to_string(), native_multiaddr.to_string()),
        ("NATIVE_PEER_ID".to_string(), native_peer_id.to_string()),
    ];
    let env_refs: Vec<(&str, &str)> =
        env.iter().map(|(k, v)| (k.as_ref(), v.as_ref())).collect();

    let (wasm_result, native_result) = tokio::join!(
        run_component_with_env(&wasm, &env_refs),
        async move {
            let (tcp_stream, _) = native_listener.accept().await.context("accept")?;
            let compat = tcp_stream.compat();

            // Noise XX inbound
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

            // Yamux inbound
            let yamux_cfg = libp2p_yamux::Config::default();
            let (yamux_proto, negotiated2) =
                listener_select_proto(noise_stream, yamux_cfg.protocol_info())
                    .await
                    .map_err(|e| anyhow::anyhow!("yamux negotiation: {e}"))?;
            let mut muxer = yamux_cfg
                .upgrade_inbound(negotiated2, yamux_proto)
                .await
                .map_err(|e| anyhow::anyhow!("yamux upgrade: {e}"))?;

            eprintln!("M9 native: handshake complete, waiting for ping substream");

            // State for the ping responder state machine.
            // Using poll_fn so we can drive the yamux muxer alongside the
            // substream operations (yamux needs its connection polled to
            // process frames and deliver data to open streams).
            type NegFut = Pin<Box<dyn std::future::Future<
                Output = std::result::Result<
                    (&'static str, multistream_select::Negotiated<libp2p_yamux::Stream>),
                    multistream_select::NegotiationError,
                >,
            >>>;

            let mut stream_opt: Option<libp2p_yamux::Stream> = None;
            let mut neg_fut: Option<NegFut> = None;
            let mut neg_stream: Option<
                multistream_select::Negotiated<libp2p_yamux::Stream>,
            > = None;
            let mut ping_buf = [0u8; 32];
            let mut read_pos: usize = 0;
            let mut written: usize = 0;
            let mut flushed = false;

            poll_fn(|cx| {
                // Step 1: accept inbound Yamux stream.
                if stream_opt.is_none() && neg_fut.is_none() && neg_stream.is_none() {
                    match Pin::new(&mut muxer).poll_inbound(cx) {
                        Poll::Ready(Ok(s)) => stream_opt = Some(s),
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("poll_inbound: {e}")))
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }

                // Step 2: kick off /ipfs/ping/1.0.0 negotiation.
                if neg_fut.is_none() && neg_stream.is_none() {
                    if let Some(stream) = stream_opt.take() {
                        neg_fut = Some(Box::pin(listener_select_proto(
                            stream,
                            std::iter::once("/ipfs/ping/1.0.0"),
                        )));
                    }
                }

                // Step 3: drive negotiation future.
                if let Some(fut) = neg_fut.as_mut() {
                    match fut.as_mut().poll(cx) {
                        Poll::Ready(Ok((_proto, ns))) => {
                            neg_stream = Some(ns);
                            neg_fut = None;
                        }
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("ping negotiation: {e}")))
                        }
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                let Some(ns) = neg_stream.as_mut() else {
                    return Poll::Pending;
                };

                // Step 4: read the 32-byte ping payload.
                while read_pos < 32 {
                    match Pin::new(&mut *ns).poll_read(cx, &mut ping_buf[read_pos..]) {
                        Poll::Ready(Ok(0)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("unexpected EOF reading ping")))
                        }
                        Poll::Ready(Ok(n)) => read_pos += n,
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("ping read: {e}")))
                        }
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                // Step 5: echo it back.
                while written < 32 {
                    match Pin::new(&mut *ns).poll_write(cx, &ping_buf[written..]) {
                        Poll::Ready(Ok(n)) => written += n,
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("ping write: {e}")))
                        }
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                // Step 6: flush.
                if !flushed {
                    match Pin::new(&mut *ns).poll_flush(cx) {
                        Poll::Ready(Ok(())) => flushed = true,
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(anyhow::anyhow!("ping flush: {e}")))
                        }
                        Poll::Pending => {
                            let _ = Pin::new(&mut muxer).poll_inbound(cx);
                            return Poll::Pending;
                        }
                    }
                }

                // Step 7: drive the muxer until the remote closes the connection.
                // This is critical: poll_flush only flushes the stream's buffer
                // into yamux's connection-level send buffer.  The connection
                // must continue to be polled so that yamux can write those
                // frames to TCP and the WASM side actually receives the echo.
                // The WASM side closes the connection after it processes the
                // ping RTT (main() returns, swarm drops, TCP FIN sent).
                match Pin::new(&mut muxer).poll_inbound(cx) {
                    Poll::Ready(Err(_)) => {
                        eprintln!("M9 native: ping echo sent, connection closed by remote");
                        Poll::Ready(Ok::<(), anyhow::Error>(()))
                    }
                    Poll::Ready(Ok(s)) => {
                        drop(s); // unexpected extra stream, ignore
                        Poll::Pending
                    }
                    Poll::Pending => Poll::Pending,
                }
            })
            .await?;

            drop(muxer);
            eprintln!("M9 native: done");
            Ok::<(), anyhow::Error>(())
        }
    );

    wasm_result.context("WASM ping component failed")?;
    native_result.context("native ping responder failed")?;
    Ok(())
}

/// M8 — libp2p Swarm over WasiTcpTransport.
///
/// Spins up a native Noise XX + Yamux listener, then runs
/// `tests/component/swarm/` alongside it.  The WASM Swarm dials the native
/// peer, verifies `ConnectionEstablished` fires for the correct peer ID, and
/// exits cleanly.  Passing proves the full Swarm upgrade chain (Noise XX +
/// Yamux + PeerId authentication) works on wasm32-wasip2.
///
/// Note: `libp2p_ping` is not used because `futures_timer` — its timer
/// dependency — relies on a background thread that cannot be spawned in
/// single-threaded wasm32-wasip2 under Wasmtime.
#[tokio::test]
async fn m8_swarm_connect() -> Result<()> {
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

    eprintln!("M8: native listener at {native_multiaddr}, peer_id={native_peer_id}");

    // ── Build WASM swarm component ────────────────────────────────────────────
    let wasm = build_component("swarm")?;

    let env = [
        ("NATIVE_ADDR".to_string(), native_multiaddr.to_string()),
        ("NATIVE_PEER_ID".to_string(), native_peer_id.to_string()),
    ];
    let env_refs: Vec<(&str, &str)> =
        env.iter().map(|(k, v)| (k.as_ref(), v.as_ref())).collect();

    // ── Run WASM swarm and native handshake peer concurrently ─────────────────
    let (wasm_result, native_result) = tokio::join!(
        run_component_with_env(&wasm, &env_refs),
        async move {
            let (tcp_stream, _) = native_listener.accept().await.context("accept")?;
            let compat = tcp_stream.compat();

            // ── Noise XX (inbound/responder) ──────────────────────────────────
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

            // ── Yamux (inbound/responder) ─────────────────────────────────────
            let yamux_cfg = libp2p_yamux::Config::default();
            let (yamux_proto, negotiated2) =
                listener_select_proto(noise_stream, yamux_cfg.protocol_info())
                    .await
                    .map_err(|e| anyhow::anyhow!("yamux negotiation: {e}"))?;
            let muxer = yamux_cfg
                .upgrade_inbound(negotiated2, yamux_proto)
                .await
                .map_err(|e| anyhow::anyhow!("yamux upgrade: {e}"))?;

            eprintln!("M8 native: handshake complete, closing connection");

            // Drop the muxer immediately: this sends a yamux GoAway + TCP
            // FIN.  The WASM side's TCP I/O pollable fires on EOF, which
            // unblocks wstd's block_on() loop so the spawned connection tasks
            // can run, see the closed channel, and exit cleanly — without the
            // ~85 s deadlock caused by the reactor waiting for a pollable that
            // never fires when both sides hold the connection open.
            drop(muxer);
            eprintln!("M8 native: done");
            Ok::<(), anyhow::Error>(())
        }
    );

    wasm_result.context("WASM swarm component failed")?;
    native_result.context("native handshake peer failed")?;

    Ok(())
}
