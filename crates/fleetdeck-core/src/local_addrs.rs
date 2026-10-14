//! The addresses that are assigned to an interface of this machine
//! (tailnet design section 14.3).
//!
//! A bind is not proof: with `ip_nonlocal_bind` the kernel binds an address
//! that no interface has. On Linux the kernel's own tables say which
//! addresses are local: `/proc/net/fib_trie` (IPv4, the `/32 host LOCAL`
//! entries) and `/proc/net/if_inet6` (IPv6). On another platform there is no
//! answer and the caller keeps the bind check.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const FIB_TRIE: &str = "/proc/net/fib_trie";
const IF_INET6: &str = "/proc/net/if_inet6";
const NONLOCAL_V4: &str = "/proc/sys/net/ipv4/ip_nonlocal_bind";
const NONLOCAL_V6: &str = "/proc/sys/net/ipv6/ip_nonlocal_bind";

/// The IPv4 addresses of the `/32 host LOCAL` entries of `/proc/net/fib_trie`.
///
/// An address line is `|-- <address>`; the line after it that starts with
/// `/` holds the prefix length, the scope and the type of that address.
#[must_use]
pub fn parse_fib_trie(text: &str) -> BTreeSet<IpAddr> {
    let mut out = BTreeSet::new();
    let mut last: Option<Ipv4Addr> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("|--") {
            last = rest.trim().parse().ok();
        } else if line.starts_with('/') {
            let mut words = line.split_whitespace();
            if let (Some("/32"), Some("host"), Some("LOCAL"), Some(addr)) =
                (words.next(), words.next(), words.next(), last)
            {
                out.insert(IpAddr::V4(addr));
            }
        } else if line.starts_with("+--") {
            last = None;
        }
    }
    out
}

/// The IPv6 addresses of `/proc/net/if_inet6`: 32 hex digits at the start of
/// each line. A line that does not fit is skipped.
#[must_use]
pub fn parse_if_inet6(text: &str) -> BTreeSet<IpAddr> {
    text.lines()
        .filter_map(|line| {
            let hex = line.split_whitespace().next()?;
            if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let value = u128::from_str_radix(hex, 16).ok()?;
            Some(IpAddr::V6(Ipv6Addr::from(value)))
        })
        .collect()
}

/// The addresses that an interface of this machine has. `None` on another
/// platform than Linux, and when a table cannot be read.
#[must_use]
pub fn local_addresses() -> Option<BTreeSet<IpAddr>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let v4 = read(FIB_TRIE)?;
    let v6 = read(IF_INET6)?;
    let mut all = parse_fib_trie(&v4);
    all.extend(parse_if_inet6(&v6));
    Some(all)
}

fn read(path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) => {
            tracing::debug!(path, error = %e, "local addresses");
            None
        }
    }
}

/// True when the text of a `ip_nonlocal_bind` file is `1`. A file that is
/// missing (`None`) or has another text is off.
#[must_use]
pub fn nonlocal_from(text: Option<&str>) -> bool {
    text.is_some_and(|t| t.trim() == "1")
}

/// True when the IPv4 or the IPv6 setting `ip_nonlocal_bind` is `1`.
#[must_use]
pub fn nonlocal_bind() -> bool {
    [NONLOCAL_V4, NONLOCAL_V6]
        .iter()
        .any(|path| nonlocal_from(std::fs::read_to_string(path).ok().as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIB_TRIE_TEXT: &str = include_str!("../tests/fixtures/local_addrs/fib_trie.txt");
    const IF_INET6_TEXT: &str = include_str!("../tests/fixtures/local_addrs/if_inet6.txt");

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn parse_fib_trie_takes_only_the_slash_32_host_local_entries() {
        let found = parse_fib_trie(FIB_TRIE_TEXT);
        let expected: BTreeSet<IpAddr> = ["192.168.8.223", "10.0.0.9", "100.89.30.8", "127.0.0.1"]
            .into_iter()
            .map(ip)
            .collect();
        assert_eq!(found, expected);
        // A link address of the main table, a broadcast address, a network
        // entry and an address that is not in the table are not local.
        for other in [
            "100.92.251.96",
            "100.100.100.100",
            "10.0.0.255",
            "127.0.0.0",
            "127.255.255.255",
            "192.168.8.0",
        ] {
            assert!(!found.contains(&ip(other)), "{other}");
        }
    }

    #[test]
    fn parse_fib_trie_of_an_empty_text_is_empty() {
        assert!(parse_fib_trie("").is_empty());
        assert!(parse_fib_trie("Main:\n  +-- 0.0.0.0/0 3 0 5\n").is_empty());
    }

    #[test]
    fn parse_if_inet6_reads_the_32_hex_digits_and_skips_bad_lines() {
        let found = parse_if_inet6(IF_INET6_TEXT);
        let expected: BTreeSet<IpAddr> = [
            "fe80::ad95:8cc1:72af:378c",
            "::1",
            "fd7a:115c:a1e0::3d38:1e09",
        ]
        .into_iter()
        .map(ip)
        .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn nonlocal_bind_is_on_only_for_the_text_one() {
        assert!(nonlocal_from(Some("1\n")));
        assert!(nonlocal_from(Some("1")));
        assert!(!nonlocal_from(Some("0\n")));
        assert!(!nonlocal_from(Some("")));
        assert!(!nonlocal_from(Some("2")));
        assert!(!nonlocal_from(None));
    }

    #[test]
    fn the_local_addresses_of_this_machine_have_loopback_on_linux() {
        // `None` is a platform without the tables, or a Linux kernel without
        // IPv6: the callers then keep the bind check.
        if let Some(all) = local_addresses() {
            assert!(all.contains(&ip("127.0.0.1")));
        }
    }
}
