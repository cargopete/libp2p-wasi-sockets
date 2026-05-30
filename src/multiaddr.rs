use std::net::{IpAddr, SocketAddr};

use libp2p_core::multiaddr::{Multiaddr, Protocol};

use crate::error::Error;

// ── DNS types ────────────────────────────────────────────────────────────────

/// IP address family constraint for a DNS dial target.
#[derive(Copy, Clone, Debug)]
pub(crate) enum DnsFamily {
    /// `/dns4` — accept only IPv4 addresses.
    V4,
    /// `/dns6` — accept only IPv6 addresses.
    V6,
    /// `/dns` — accept the first address of either family.
    Any,
}

impl DnsFamily {
    pub(crate) fn matches(self, ip: IpAddr) -> bool {
        matches!(
            (self, ip),
            (DnsFamily::V4, IpAddr::V4(_)) | (DnsFamily::V6, IpAddr::V6(_)) | (DnsFamily::Any, _)
        )
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            DnsFamily::V4 => "IPv4",
            DnsFamily::V6 => "IPv6",
            DnsFamily::Any => "any-family",
        }
    }
}

/// Parsed dial target extracted from a [`Multiaddr`].
pub(crate) enum DialTarget {
    /// Already resolved — connect directly.
    Addr(SocketAddr),
    /// DNS hostname — resolve asynchronously, then connect.
    Dns {
        host: String,
        port: u16,
        family: DnsFamily,
    },
}

/// Parse a [`Multiaddr`] into a [`DialTarget`].
///
/// Accepted forms (all optionally ending in `/p2p/<peer-id>`):
///   - `/ip4/<addr>/tcp/<port>`
///   - `/ip6/<addr>/tcp/<port>`
///   - `/dns4/<host>/tcp/<port>` — resolve to IPv4, then connect
///   - `/dns6/<host>/tcp/<port>` — resolve to IPv6, then connect
///   - `/dns/<host>/tcp/<port>`  — resolve to any family, then connect
///
/// `/dnsaddr` (libp2p TXT-record discovery) is not supported.
pub(crate) fn multiaddr_to_dial_target(addr: &Multiaddr) -> Result<DialTarget, Error> {
    let mut iter = addr.iter();

    enum First {
        Ip(IpAddr),
        Dns(String, DnsFamily),
    }

    let first = match iter.next() {
        Some(Protocol::Ip4(a)) => First::Ip(IpAddr::V4(a)),
        Some(Protocol::Ip6(a)) => First::Ip(IpAddr::V6(a)),
        Some(Protocol::Dns4(h)) => First::Dns(h.into_owned(), DnsFamily::V4),
        Some(Protocol::Dns6(h)) => First::Dns(h.into_owned(), DnsFamily::V6),
        Some(Protocol::Dns(h)) => First::Dns(h.into_owned(), DnsFamily::Any),
        _ => return Err(Error::UnsupportedMultiaddr(addr.clone())),
    };

    let port = match iter.next() {
        Some(Protocol::Tcp(p)) => p,
        _ => return Err(Error::UnsupportedMultiaddr(addr.clone())),
    };

    match iter.next() {
        None | Some(Protocol::P2p(_)) => {}
        _ => return Err(Error::UnsupportedMultiaddr(addr.clone())),
    }

    Ok(match first {
        First::Ip(ip) => DialTarget::Addr(SocketAddr::new(ip, port)),
        First::Dns(host, family) => DialTarget::Dns { host, port, family },
    })
}

// ── IP-only helpers ───────────────────────────────────────────────────────────

/// Attempt to parse a [`Multiaddr`] into a [`SocketAddr`].
///
/// Accepted forms:
///   - `/ip4/<addr>/tcp/<port>[/p2p/<peer-id>]`
///   - `/ip6/<addr>/tcp/<port>[/p2p/<peer-id>]`
///
/// Everything else returns [`Error::UnsupportedMultiaddr`].
pub(crate) fn multiaddr_to_socketaddr(addr: &Multiaddr) -> Result<SocketAddr, Error> {
    let mut iter = addr.iter();

    let ip: IpAddr = match iter.next() {
        Some(Protocol::Ip4(a)) => IpAddr::V4(a),
        Some(Protocol::Ip6(a)) => IpAddr::V6(a),
        _ => return Err(Error::UnsupportedMultiaddr(addr.clone())),
    };

    let port: u16 = match iter.next() {
        Some(Protocol::Tcp(p)) => p,
        _ => return Err(Error::UnsupportedMultiaddr(addr.clone())),
    };

    // Optional trailing /p2p/<peer-id> — accepted, stripped.
    match iter.next() {
        None | Some(Protocol::P2p(_)) => {}
        _ => return Err(Error::UnsupportedMultiaddr(addr.clone())),
    }

    Ok(SocketAddr::new(ip, port))
}

/// Convert a [`SocketAddr`] to a `/ip4/.../tcp/...` or `/ip6/.../tcp/...` [`Multiaddr`].
pub(crate) fn socketaddr_to_multiaddr(addr: SocketAddr) -> Multiaddr {
    let mut ma = Multiaddr::empty();
    match addr.ip() {
        IpAddr::V4(a) => ma.push(Protocol::Ip4(a)),
        IpAddr::V6(a) => ma.push(Protocol::Ip6(a)),
    }
    ma.push(Protocol::Tcp(addr.port()));
    ma
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn roundtrip_ipv4() {
        let sa: SocketAddr = "127.0.0.1:4001".parse().unwrap();
        let ma = socketaddr_to_multiaddr(sa);
        assert_eq!(multiaddr_to_socketaddr(&ma).unwrap(), sa);
    }

    #[test]
    fn roundtrip_ipv6() {
        let sa: SocketAddr = "[::1]:4001".parse().unwrap();
        let ma = socketaddr_to_multiaddr(sa);
        assert_eq!(multiaddr_to_socketaddr(&ma).unwrap(), sa);
    }

    #[test]
    fn strips_p2p_suffix() {
        let ma: Multiaddr =
            "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooWGjwWkrTXkqQKGegFKSQpKyUMMU6ZVJZ7suwT1SjTz6Vs"
                .parse()
                .unwrap();
        let sa = multiaddr_to_socketaddr(&ma).unwrap();
        assert_eq!(sa.port(), 4001);
        assert_eq!(sa.ip(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    }

    #[test]
    fn rejects_dns() {
        let ma: Multiaddr = "/dns4/example.com/tcp/4001".parse().unwrap();
        assert!(multiaddr_to_socketaddr(&ma).is_err());
    }

    // ── multiaddr_to_dial_target ──────────────────────────────────────────────

    #[test]
    fn dial_target_ipv4() {
        let ma: Multiaddr = "/ip4/1.2.3.4/tcp/1234".parse().unwrap();
        let target = multiaddr_to_dial_target(&ma).unwrap();
        let DialTarget::Addr(sa) = target else {
            panic!("expected Addr")
        };
        assert_eq!(sa, "1.2.3.4:1234".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn dial_target_ipv6() {
        let ma: Multiaddr = "/ip6/::1/tcp/4001".parse().unwrap();
        let target = multiaddr_to_dial_target(&ma).unwrap();
        let DialTarget::Addr(sa) = target else {
            panic!("expected Addr")
        };
        assert_eq!(sa.port(), 4001);
    }

    #[test]
    fn dial_target_dns4() {
        let ma: Multiaddr = "/dns4/example.com/tcp/80".parse().unwrap();
        let target = multiaddr_to_dial_target(&ma).unwrap();
        let DialTarget::Dns { host, port, family } = target else {
            panic!("expected Dns")
        };
        assert_eq!(host, "example.com");
        assert_eq!(port, 80);
        assert!(matches!(family, DnsFamily::V4));
    }

    #[test]
    fn dial_target_dns6() {
        let ma: Multiaddr = "/dns6/example.com/tcp/443".parse().unwrap();
        let target = multiaddr_to_dial_target(&ma).unwrap();
        let DialTarget::Dns { family, .. } = target else {
            panic!("expected Dns")
        };
        assert!(matches!(family, DnsFamily::V6));
    }

    #[test]
    fn dial_target_dns_any() {
        let ma: Multiaddr = "/dns/example.com/tcp/8080".parse().unwrap();
        let target = multiaddr_to_dial_target(&ma).unwrap();
        let DialTarget::Dns { family, .. } = target else {
            panic!("expected Dns")
        };
        assert!(matches!(family, DnsFamily::Any));
    }

    #[test]
    fn dial_target_dns4_strips_p2p() {
        let ma: Multiaddr =
            "/dns4/example.com/tcp/4001/p2p/12D3KooWGjwWkrTXkqQKGegFKSQpKyUMMU6ZVJZ7suwT1SjTz6Vs"
                .parse()
                .unwrap();
        let target = multiaddr_to_dial_target(&ma).unwrap();
        let DialTarget::Dns { host, port, .. } = target else {
            panic!("expected Dns")
        };
        assert_eq!(host, "example.com");
        assert_eq!(port, 4001);
    }

    #[test]
    fn dial_target_rejects_dnsaddr() {
        let ma: Multiaddr = "/dnsaddr/bootstrap.libp2p.io/tcp/4001".parse().unwrap();
        assert!(multiaddr_to_dial_target(&ma).is_err());
    }

    #[test]
    fn zero_port_ipv4() {
        let sa = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let ma = socketaddr_to_multiaddr(sa);
        assert_eq!(multiaddr_to_socketaddr(&ma).unwrap(), sa);
    }

    #[test]
    fn zero_port_ipv6() {
        let sa = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let ma = socketaddr_to_multiaddr(sa);
        assert_eq!(multiaddr_to_socketaddr(&ma).unwrap(), sa);
    }
}
