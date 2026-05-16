use std::net::{IpAddr, SocketAddr};

use libp2p_core::multiaddr::{Multiaddr, Protocol};

use crate::error::Error;

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
        let ma: Multiaddr = "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooWGjwWkrTXkqQKGegFKSQpKyUMMU6ZVJZ7suwT1SjTz6Vs"
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
