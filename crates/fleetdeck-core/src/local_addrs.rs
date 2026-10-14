//! The addresses that are assigned to an interface of this machine
//! (tailnet design section 14.3).
//!
//! A bind is not proof: with `ip_nonlocal_bind` the kernel binds an address
//! that no interface has. On Linux the kernel's own tables say which
//! addresses are local: `/proc/net/fib_trie` (IPv4, the `/32 host LOCAL`
//! entries) and `/proc/net/if_inet6` (IPv6). Each address family is checked
//! with its own table; a table that cannot be read on Linux means "not
//! local" (fail closed). On another platform there is no answer and the
//! caller keeps the bind check.

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
pub fn parse_fib_trie(text: &str) -> BTreeSet<Ipv4Addr> {
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
                out.insert(addr);
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
pub fn parse_if_inet6(text: &str) -> BTreeSet<Ipv6Addr> {
    text.lines()
        .filter_map(|line| {
            let hex = line.split_whitespace().next()?;
            if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let value = u128::from_str_radix(hex, 16).ok()?;
            Some(Ipv6Addr::from(value))
        })
        .collect()
}

/// The addresses that an interface of this machine has, per address family.
/// A family is `None` when its table cannot be read, and both are `None` on
/// another platform than Linux.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalAddrs {
    /// The IPv4 addresses of `/proc/net/fib_trie`.
    pub v4: Option<BTreeSet<Ipv4Addr>>,
    /// The IPv6 addresses of `/proc/net/if_inet6`.
    pub v6: Option<BTreeSet<Ipv6Addr>>,
}

impl LocalAddrs {
    /// The addresses of the text of each table (`None`: the table could not
    /// be read).
    #[must_use]
    pub fn from_tables(fib_trie: Option<&str>, if_inet6: Option<&str>) -> Self {
        Self {
            v4: fib_trie.map(parse_fib_trie),
            v6: if_inet6.map(parse_if_inet6),
        }
    }

    /// True when the table of the family of `ip` is known and holds it. An
    /// unreadable table is false (fail closed).
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ip) => self.v4.as_ref().is_some_and(|set| set.contains(&ip)),
            IpAddr::V6(ip) => self.v6.as_ref().is_some_and(|set| set.contains(&ip)),
        }
    }
}

/// The addresses that an interface of this machine has. On another platform
/// than Linux both families are `None`.
#[must_use]
pub fn local_addresses() -> LocalAddrs {
    if !cfg!(target_os = "linux") {
        return LocalAddrs::default();
    }
    LocalAddrs::from_tables(read(FIB_TRIE).as_deref(), read(IF_INET6).as_deref())
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

    fn v4(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    #[test]
    fn parse_fib_trie_takes_only_the_slash_32_host_local_entries() {
        let found = parse_fib_trie(FIB_TRIE_TEXT);
        let expected: BTreeSet<Ipv4Addr> =
            ["192.168.8.223", "10.0.0.9", "100.89.30.8", "127.0.0.1"]
                .into_iter()
                .map(v4)
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
            assert!(!found.contains(&v4(other)), "{other}");
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
        let expected: BTreeSet<Ipv6Addr> = [
            "fe80::ad95:8cc1:72af:378c",
            "::1",
            "fd7a:115c:a1e0::3d38:1e09",
        ]
        .into_iter()
        .map(|text| text.parse().unwrap())
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
    fn an_address_is_local_when_the_table_of_its_family_holds_it() {
        let both = LocalAddrs::from_tables(Some(FIB_TRIE_TEXT), Some(IF_INET6_TEXT));
        assert!(both.contains(ip("100.89.30.8")));
        assert!(both.contains(ip("fd7a:115c:a1e0::3d38:1e09")));
        assert!(!both.contains(ip("100.127.254.254")));
        assert!(!both.contains(ip("fd7a:115c:a1e0::1")));
    }

    #[test]
    fn an_unreadable_ipv6_table_does_not_make_an_address_local() {
        // IPv4 `ip_nonlocal_bind` on: a bind would work for any address, so
        // only the table counts.
        let local = LocalAddrs::from_tables(Some(FIB_TRIE_TEXT), None);
        assert!(local.contains(ip("100.89.30.8")));
        assert!(!local.contains(ip("100.127.254.254")));
        // An IPv6 address is never local without its table.
        assert!(!local.contains(ip("::1")));
        assert!(!local.contains(ip("fd7a:115c:a1e0::3d38:1e09")));
    }

    #[test]
    fn an_unreadable_ipv4_table_does_not_make_an_address_local() {
        let local = LocalAddrs::from_tables(None, Some(IF_INET6_TEXT));
        assert!(!local.contains(ip("127.0.0.1")));
        assert!(local.contains(ip("::1")));
        assert!(!LocalAddrs::default().contains(ip("127.0.0.1")));
    }

    #[test]
    fn the_local_addresses_of_this_machine_have_loopback_on_linux() {
        // A family that is `None` is a platform without the table, or a
        // Linux kernel without IPv6.
        if let Some(found) = local_addresses().v4 {
            assert!(found.contains(&v4("127.0.0.1")));
        }
    }
}
