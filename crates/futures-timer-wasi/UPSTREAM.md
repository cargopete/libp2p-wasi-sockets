# Upstreaming a `wasm32-wasip2` backend to `futures-timer`

## The problem this crate works around

`futures-timer` selects its `Delay` backend like so (as of v3.0.4):

| target | backend | mechanism |
|---|---|---|
| `wasm32 + feature = "wasm-bindgen"` | `wasm` | browser `setTimeout` via `gloo-timers` |
| everything else | `native` | a background timer-wheel **thread** |

On `wasm32-wasip2` there is no `wasm-bindgen`, so the crate falls into the
`native` backend — which spawns a thread. The WASI Preview 2 component model is
single-threaded, so the timer thread never runs and the first `Delay` poll
aborts with:

```
thread 'main' panicked at futures-timer-3.0.4/src/native/delay.rs:126:
timer has gone away
```

Every libp2p behaviour that uses a timer (`ping`, `kad`, `gossipsub`,
`request-response`, `rendezvous`, Swarm idle-timeout, …) hits this. The only
fix available to a *downstream* crate is a `[patch.crates-io]` override in the
**final binary's** workspace root — `[patch]` does not compose for library
consumers and cannot be published. Hence this shim crate.

## The real fix: a target-gated backend upstream

The clean solution is a third backend in `futures-timer` itself, selected for
WASI Preview 2 via `target_env = "p2"` (so `wasip1` and future previews stay on
`native`). Because the `native` backend is **already broken at runtime** on
`wasm32-wasip2`, this is a pure fix with zero regression risk to existing
users, and the `wstd`/`wasip2` dependencies are pulled in *only* for that one
target.

The backend itself is `src/wasi_impl.rs` in this crate (identical API to the
other backends: `Delay::new` / `Delay::reset` / `Future<Output = ()>` /
`Debug`).

### Proposed diff against `async-rs/futures-timer`

`Cargo.toml`:

```toml
# WASI Preview 2 backend: a `wasi:clocks` timer driven by the `wstd` reactor.
# Pulled in only for `wasm32-wasip2`, where the default thread-based backend
# cannot run (the component model is single-threaded).
[target.'cfg(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"))'.dependencies]
wstd = "0.6"
wasip2 = "1.0"
```

`src/lib.rs` (replace the two-way module selection with three-way):

```rust
// On `wasm32-wasip2` the thread-based `native` backend cannot run (the WASI
// component model is single-threaded), so use a `wasi:clocks`-backed timer.
#[cfg(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"))]
mod wasip2;
#[cfg(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"))]
pub use self::wasip2::Delay;

// Browser wasm with the `wasm-bindgen` feature: `setTimeout` via gloo-timers.
#[cfg(all(
    target_arch = "wasm32",
    feature = "wasm-bindgen",
    not(all(target_os = "wasi", target_env = "p2"))
))]
mod wasm;
#[cfg(all(
    target_arch = "wasm32",
    feature = "wasm-bindgen",
    not(all(target_os = "wasi", target_env = "p2"))
))]
pub use self::wasm::Delay;

// All other targets: the thread-backed timer wheel.
#[cfg(not(any(
    all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"),
    all(target_arch = "wasm32", feature = "wasm-bindgen")
)))]
mod native;
#[cfg(not(any(
    all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"),
    all(target_arch = "wasm32", feature = "wasm-bindgen")
)))]
pub use self::native::Delay;
```

plus a new `src/wasip2.rs` = the contents of this crate's `src/wasi_impl.rs`.

### Verification

The change builds cleanly for `wasm32-wasip2`, `wasm32-wasip1` (stays on
`native`, no `wstd` pulled), and the host target (unchanged). Pointing the
`tests/component/ping` patch at the patched fork, the `m9_ping` integration
test passes with a real RTT and no "timer has gone away" panic.

### Open design question for the maintainer

`wstd` is the de-facto async reactor for `wasm32-wasip2`; the backend awaits the
`wasi:clocks` pollable through it. A reactor-agnostic implementation isn't
possible because WASI 0.2 has no ambient reactor — each runtime supplies its
own. If `wstd` as a (target-gated) dependency is unwelcome, the backend can be
placed behind an opt-in `wasip2` feature instead of being auto-selected.

## Until it lands

Downstream users add this to their **binary crate's** `Cargo.toml`:

```toml
[patch.crates-io]
futures-timer = { git = "https://github.com/cargopete/libp2p-wasi-sockets", package = "futures-timer" }
```

Once the upstream backend is released, the patch can be dropped entirely.
