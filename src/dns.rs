//! Async DNS resolution via `wasi:sockets/ip-name-lookup`.
//!
//! Only compiled for `wasm32`: on host targets the raw `wasip2` bindings call
//! `unreachable!()`, and `WasiTcpTransport` never dials on the host anyway.

#![cfg(target_arch = "wasm32")]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use wasip2::sockets::instance_network::instance_network;
use wasip2::sockets::ip_name_lookup::resolve_addresses;
use wasip2::sockets::network::{ErrorCode, IpAddress};
use wstd::runtime::AsyncPollable;

use crate::error::Error;
use crate::multiaddr::DnsFamily;

/// Resolve `host` via `wasi:sockets/ip-name-lookup` and return the first
/// address whose family satisfies `family`, combined with `port`.
///
/// Polls `ResolveAddressStream` in a loop, yielding on `WouldBlock` by
/// awaiting the stream's `Pollable`.
pub(crate) async fn resolve_host(
    host: &str,
    port: u16,
    family: DnsFamily,
) -> Result<SocketAddr, Error> {
    let network = instance_network();
    let stream = resolve_addresses(&network, host).map_err(|e| {
        Error::Io(std::io::Error::other(format!(
            "DNS: could not start resolution for '{host}': {e:?}"
        )))
    })?;

    loop {
        match stream.resolve_next_address() {
            Ok(Some(ip)) => {
                let std_ip = wasi_ip_to_std(ip);
                if family.matches(std_ip) {
                    return Ok(SocketAddr::new(std_ip, port));
                }
                // Wrong address family — continue iterating.
            }
            Ok(None) => {
                return Err(Error::Io(std::io::Error::other(format!(
                    "DNS: no {} addresses found for '{host}'",
                    family.description()
                ))));
            }
            Err(ErrorCode::WouldBlock) => {
                let pollable = AsyncPollable::new(stream.subscribe());
                pollable.wait_for().await;
            }
            Err(e) => {
                return Err(Error::Io(std::io::Error::other(format!(
                    "DNS: resolution failed for '{host}': {e:?}"
                ))));
            }
        }
    }
}

fn wasi_ip_to_std(ip: IpAddress) -> IpAddr {
    match ip {
        IpAddress::Ipv4((a, b, c, d)) => IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
        IpAddress::Ipv6((a, b, c, d, e, f, g, h)) => {
            IpAddr::V6(Ipv6Addr::new(a, b, c, d, e, f, g, h))
        }
    }
}
