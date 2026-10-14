//! The deck address of `fleetdeck station join --tailnet <host>` (DL-178,
//! tailnet design 5.2): the host must be a peer of the station's own
//! `tailscale status`, and the station dials exactly the tailnet addresses
//! of that peer. No general DNS resolution, no redirect.
//!
//! A host matches a peer by its DNS name (case-insensitive, trailing dot
//! ignored), by its short host name, or by one of its address literals.

use std::net::IpAddr;

use fleetdeck_core::tailscale::{Node, Status};

/// Why a host does not name one peer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    #[error("`{0}` is not a device in `tailscale status` of this machine")]
    NotInTailnet(String),
    #[error("`{0}` names more than one device in `tailscale status`; use its DNS name")]
    Ambiguous(String),
    #[error("the device `{0}` has no tailnet address")]
    NoAddress(String),
}

fn name_of(text: &str) -> String {
    text.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// The addresses of the peer that `host` names: IPv4 first, then IPv6.
///
/// # Errors
/// [`TargetError`] when `host` names no peer, or more than one.
pub fn resolve(status: &Status, host: &str) -> Result<Vec<IpAddr>, TargetError> {
    let wanted = name_of(host);
    let literal = wanted
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok()
        .map(|ip| ip.to_canonical());
    let matches = |peer: &&Node| -> bool {
        match literal {
            Some(ip) => peer.ips.iter().any(|p| p.to_canonical() == ip),
            None => {
                (!peer.dns_name.is_empty() && name_of(&peer.dns_name) == wanted)
                    || (!peer.host_name.is_empty() && name_of(&peer.host_name) == wanted)
            }
        }
    };
    let found: Vec<&Node> = status.peers.iter().filter(matches).collect();
    let peer = match found.as_slice() {
        [] => return Err(TargetError::NotInTailnet(host.to_owned())),
        [one] => *one,
        _ => return Err(TargetError::Ambiguous(host.to_owned())),
    };
    let mut ips = peer.ips.clone();
    ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    ips.dedup();
    if ips.is_empty() {
        return Err(TargetError::NoAddress(host.to_owned()));
    }
    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(host: &str, dns: &str, ips: &[&str]) -> Node {
        Node {
            host_name: host.into(),
            dns_name: dns.into(),
            ips: ips.iter().map(|i| i.parse().unwrap()).collect(),
            os: None,
            login: None,
            tags: Vec::new(),
            online: true,
        }
    }

    fn status() -> Status {
        Status {
            this: node("me", "me.tail.ts.net", &["100.64.0.1"]),
            peers: vec![
                node(
                    "Deck",
                    "deck.tail.ts.net",
                    &["fd7a:115c:a1e0::5", "100.64.0.5"],
                ),
                node("laptop", "laptop.tail.ts.net", &["100.64.0.6"]),
                node("laptop", "laptop.other.ts.net", &["100.64.0.7"]),
            ],
            magic_dns_suffix: None,
        }
    }

    #[test]
    fn a_dns_name_a_host_name_or_an_address_names_a_peer_and_ipv4_comes_first() {
        let s = status();
        let want: Vec<IpAddr> = vec!["100.64.0.5".parse().unwrap(), "fd7a:115c:a1e0::5".parse().unwrap()];
        for host in [
            "deck.tail.ts.net",
            "DECK.tail.ts.net.",
            "deck",
            "100.64.0.5",
            "fd7a:115c:a1e0::5",
            "[fd7a:115c:a1e0::5]",
        ] {
            assert_eq!(resolve(&s, host).unwrap(), want, "{host}");
        }
    }

    #[test]
    fn a_host_that_is_not_a_peer_is_refused() {
        let s = status();
        for host in ["me", "me.tail.ts.net", "100.64.0.1", "example.com", "10.0.0.1", ""] {
            assert!(
                matches!(resolve(&s, host), Err(TargetError::NotInTailnet(_))),
                "{host}"
            );
        }
    }

    #[test]
    fn a_short_name_of_two_devices_is_ambiguous_and_the_dns_name_is_not() {
        let s = status();
        assert!(matches!(resolve(&s, "laptop"), Err(TargetError::Ambiguous(_))));
        assert_eq!(
            resolve(&s, "laptop.other.ts.net").unwrap(),
            vec!["100.64.0.7".parse::<IpAddr>().unwrap()]
        );
    }
}
