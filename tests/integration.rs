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
    let mut config = Config::new();
    config.wasm_component_model(true);
    // async_support is always-on in wasmtime ≥ 44; no call needed.
    let engine = Engine::new(&config)?;

    let component = Component::from_file(&engine, wasm_path)?;

    let wasi = WasiCtxBuilder::new()
        .inherit_stdio()
        .inherit_network()
        .build();

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
