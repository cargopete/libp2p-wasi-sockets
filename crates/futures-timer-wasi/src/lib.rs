//! WASI-compatible drop-in replacement for `futures-timer`.
//!
//! Provides the same `Delay` future API as the upstream crate, but on
//! `wasm32-wasip2` uses `wasi:clocks/monotonic-clock::subscribe_duration` +
//! the `wstd` reactor instead of spawning a background thread (which is
//! unavailable in the WASI component model).
//!
//! On all other targets the implementation is a simple `thread::sleep`-based
//! helper, equivalent to the upstream behaviour.

#![deny(missing_docs)]
#![warn(missing_debug_implementations)]

#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
mod wasi_impl;
#[cfg(not(all(target_arch = "wasm32", target_os = "wasi")))]
mod native_impl;

#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
pub use wasi_impl::Delay;
#[cfg(not(all(target_arch = "wasm32", target_os = "wasi")))]
pub use native_impl::Delay;
