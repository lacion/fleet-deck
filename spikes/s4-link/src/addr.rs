//! Network scope (D13, PLAN §8.1): a node listens and dials only on loopback,
//! private LAN ranges (RFC 1918, IPv6 ULA, link-local) and the Tailscale ranges
//! (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`). Every other address is rejected
//! at bind time, at dial time, and for the source address of an accepted
//! connection.
//!
//! Tailscale membership is only an address range here. It is never
//! authentication: the link always needs mutual TLS with pinned keys.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// The class of an IP address under D13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrClass {
    /// `127.0.0.0/8`, `::1`
    Loopback,
    /// RFC 1918: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`
    PrivateV4,
    /// IPv6 unique local `fc00::/7` (without the Tailscale prefix)
    UniqueLocal,
    /// `169.254.0.0/16`, `fe80::/10`
    LinkLocal,
    /// Tailscale: `100.64.0.0/10` (CGNAT range), `fd7a:115c:a1e0::/48`
    Tailnet,
    /// `0.0.0.0`, `::`: "all interfaces", which includes public ones
    Unspecified,
    /// multicast and the IPv4 broadcast address
    Multicast,
    /// everything else, including documentation and NAT64 ranges
    Public,
}

impl AddrClass {
    /// D13: the node may listen on, dial, or accept from this class.
    pub fn allowed(self) -> bool {
        matches!(
            self,
            Self::Loopback | Self::PrivateV4 | Self::UniqueLocal | Self::LinkLocal | Self::Tailnet
        )
    }
}

/// Why an address is outside the network scope.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddrError {
    #[error(
        "refusing to listen on {addr} ({class:?}): D13 allows only loopback, LAN and tailnet addresses"
    )]
    Bind { addr: SocketAddr, class: AddrClass },
    #[error(
        "refusing to dial {addr} ({class:?}): D13 allows only loopback, LAN and tailnet addresses"
    )]
    Dial { addr: SocketAddr, class: AddrClass },
    #[error(
        "refusing a connection from {addr} ({class:?}): D13 allows only loopback, LAN and tailnet peers"
    )]
    Peer { addr: SocketAddr, class: AddrClass },
    #[error("no address of {host} is inside the network scope (resolved: {resolved:?})")]
    NoAllowedAddress {
        host: String,
        resolved: Vec<SocketAddr>,
    },
}

pub fn classify(ip: IpAddr) -> AddrClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            // `::ffff:a.b.c.d` is the IPv4 address a.b.c.d
            Some(v4) => classify_v4(v4),
            None => classify_v6(v6),
        },
    }
}

fn classify_v4(ip: Ipv4Addr) -> AddrClass {
    let [a, b, _, _] = ip.octets();
    if ip.is_unspecified() {
        AddrClass::Unspecified
    } else if ip.is_loopback() {
        AddrClass::Loopback
    } else if ip.is_private() {
        AddrClass::PrivateV4
    } else if ip.is_link_local() {
        AddrClass::LinkLocal
    } else if a == 100 && (b & 0xc0) == 64 {
        // 100.64.0.0/10: the second octet is 64..=127
        AddrClass::Tailnet
    } else if ip.is_multicast() || ip.is_broadcast() {
        AddrClass::Multicast
    } else {
        AddrClass::Public
    }
}

const TAILNET_V6: [u16; 3] = [0xfd7a, 0x115c, 0xa1e0];

fn classify_v6(ip: Ipv6Addr) -> AddrClass {
    let s = ip.segments();
    if ip.is_unspecified() {
        AddrClass::Unspecified
    } else if ip.is_loopback() {
        AddrClass::Loopback
    } else if s[..3] == TAILNET_V6 {
        AddrClass::Tailnet
    } else if (s[0] & 0xfe00) == 0xfc00 {
        AddrClass::UniqueLocal
    } else if (s[0] & 0xffc0) == 0xfe80 {
        AddrClass::LinkLocal
    } else if ip.is_multicast() {
        AddrClass::Multicast
    } else {
        AddrClass::Public
    }
}

/// Checks a listen address. `0.0.0.0` and `::` are refused: they also listen
/// on public interfaces. The owner gives a concrete LAN or tailnet address.
pub fn check_bind(addr: SocketAddr) -> Result<(), AddrError> {
    let class = classify(addr.ip());
    if class.allowed() {
        Ok(())
    } else {
        Err(AddrError::Bind { addr, class })
    }
}

/// Checks an address before the node opens a connection to it.
pub fn check_dial(addr: SocketAddr) -> Result<(), AddrError> {
    let class = classify(addr.ip());
    if class.allowed() {
        Ok(())
    } else {
        Err(AddrError::Dial { addr, class })
    }
}

/// Checks the source address of an accepted connection (a port forward could
/// route a public peer to a LAN listener).
pub fn check_peer(addr: SocketAddr) -> Result<(), AddrError> {
    let class = classify(addr.ip());
    if class.allowed() {
        Ok(())
    } else {
        Err(AddrError::Peer { addr, class })
    }
}

/// Keeps the resolved addresses of `host` that are inside the scope. The node
/// then dials one of these exact addresses, never the host name again, so a
/// second DNS answer cannot move the connection to a public address.
pub fn filter_resolved(
    host: &str,
    resolved: Vec<SocketAddr>,
) -> Result<Vec<SocketAddr>, AddrError> {
    let allowed: Vec<SocketAddr> = resolved
        .iter()
        .copied()
        .filter(|a| check_dial(*a).is_ok())
        .collect();
    if allowed.is_empty() {
        Err(AddrError::NoAllowedAddress {
            host: host.to_owned(),
            resolved,
        })
    } else {
        Ok(allowed)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use AddrClass::*;

    fn c(s: &str) -> AddrClass {
        classify(s.parse().unwrap())
    }

    #[test]
    fn ipv4_ranges() {
        let table = [
            ("127.0.0.1", Loopback),
            ("127.255.255.254", Loopback),
            ("10.0.0.1", PrivateV4),
            ("10.255.255.255", PrivateV4),
            ("172.16.0.1", PrivateV4),
            ("172.31.255.255", PrivateV4),
            ("172.15.255.255", Public),
            ("172.32.0.0", Public),
            ("192.168.1.10", PrivateV4),
            ("192.169.0.1", Public),
            ("169.254.10.10", LinkLocal),
            ("0.0.0.0", Unspecified),
            ("224.0.0.251", Multicast),
            ("255.255.255.255", Multicast),
            ("8.8.8.8", Public),
            ("192.0.2.1", Public),
        ];
        for (ip, want) in table {
            assert_eq!(c(ip), want, "{ip}");
        }
    }

    #[test]
    fn tailnet_ipv4_range_edges() {
        assert_eq!(c("100.63.255.255"), Public);
        assert_eq!(c("100.64.0.0"), Tailnet);
        assert_eq!(c("100.100.100.100"), Tailnet);
        assert_eq!(c("100.127.255.255"), Tailnet);
        assert_eq!(c("100.128.0.0"), Public);
    }

    #[test]
    fn ipv6_ranges() {
        let table = [
            ("::1", Loopback),
            ("::", Unspecified),
            ("fc00::1", UniqueLocal),
            ("fdff:ffff::1", UniqueLocal),
            ("fe80::1", LinkLocal),
            ("febf::1", LinkLocal),
            ("fec0::1", Public),
            ("ff02::fb", Multicast),
            ("2001:db8::1", Public),
            ("2606:4700::1111", Public),
            ("64:ff9b::808:808", Public),
        ];
        for (ip, want) in table {
            assert_eq!(c(ip), want, "{ip}");
        }
    }

    #[test]
    fn tailnet_ipv6_prefix_is_exactly_a_48() {
        assert_eq!(c("fd7a:115c:a1e0::1"), Tailnet);
        assert_eq!(c("fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff"), Tailnet);
        // The neighbours of the /48 are ordinary ULA addresses.
        assert_eq!(c("fd7a:115c:a1e1::1"), UniqueLocal);
        assert_eq!(c("fd7a:115c:a1df::1"), UniqueLocal);
    }

    #[test]
    fn ipv4_mapped_ipv6_uses_the_ipv4_rules() {
        assert_eq!(c("::ffff:10.0.0.1"), PrivateV4);
        assert_eq!(c("::ffff:100.64.1.1"), Tailnet);
        assert_eq!(c("::ffff:8.8.8.8"), Public);
        assert_eq!(c("::ffff:127.0.0.1"), Loopback);
    }

    #[test]
    fn bind_dial_and_peer_checks() {
        let sa = |s: &str| -> SocketAddr { s.parse().unwrap() };
        assert!(check_bind(sa("127.0.0.1:0")).is_ok());
        assert!(check_bind(sa("[fd7a:115c:a1e0::5]:7000")).is_ok());
        assert!(matches!(
            check_bind(sa("0.0.0.0:7000")),
            Err(AddrError::Bind {
                class: Unspecified,
                ..
            })
        ));
        assert!(matches!(
            check_bind(sa("[::]:7000")),
            Err(AddrError::Bind {
                class: Unspecified,
                ..
            })
        ));
        assert!(matches!(
            check_bind(sa("203.0.113.9:7000")),
            Err(AddrError::Bind { class: Public, .. })
        ));
        assert!(check_dial(sa("192.168.0.2:7000")).is_ok());
        assert!(check_dial(sa("100.101.102.103:7000")).is_ok());
        assert!(matches!(
            check_dial(sa("1.1.1.1:443")),
            Err(AddrError::Dial { class: Public, .. })
        ));
        assert!(matches!(
            check_dial(sa("0.0.0.0:443")),
            Err(AddrError::Dial {
                class: Unspecified,
                ..
            })
        ));
        assert!(check_peer(sa("[fe80::1]:5555")).is_ok());
        assert!(matches!(
            check_peer(sa("8.8.4.4:5555")),
            Err(AddrError::Peer { class: Public, .. })
        ));
    }

    #[test]
    fn resolved_names_keep_only_allowed_addresses() {
        let sa = |s: &str| -> SocketAddr { s.parse().unwrap() };
        let kept = filter_resolved(
            "deck.lan",
            vec![sa("93.184.216.34:7000"), sa("192.168.1.5:7000")],
        )
        .unwrap();
        assert_eq!(kept, vec![sa("192.168.1.5:7000")]);
        assert!(matches!(
            filter_resolved("example.com", vec![sa("93.184.216.34:443")]),
            Err(AddrError::NoAllowedAddress { .. })
        ));
    }

    proptest! {
        #[test]
        fn mapped_addresses_match_their_ipv4_class(bits in any::<u32>()) {
            let v4 = Ipv4Addr::from(bits);
            prop_assert_eq!(classify(IpAddr::V6(v4.to_ipv6_mapped())), classify(IpAddr::V4(v4)));
        }

        #[test]
        fn only_listed_ranges_are_allowed_v4(bits in any::<u32>()) {
            let ip = Ipv4Addr::from(bits);
            let [a, b, _, _] = ip.octets();
            let listed = a == 127
                || a == 10
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254)
                || (a == 100 && (64..=127).contains(&b));
            prop_assert_eq!(classify(IpAddr::V4(ip)).allowed(), listed, "{}", ip);
        }

        #[test]
        fn only_listed_ranges_are_allowed_v6(bits in any::<u128>()) {
            let ip = Ipv6Addr::from(bits);
            prop_assume!(ip.to_ipv4_mapped().is_none());
            let s0 = ip.segments()[0];
            let listed = ip == Ipv6Addr::LOCALHOST
                || (s0 & 0xfe00) == 0xfc00
                || (s0 & 0xffc0) == 0xfe80;
            prop_assert_eq!(classify(IpAddr::V6(ip)).allowed(), listed, "{}", ip);
        }
    }
}
