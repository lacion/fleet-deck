//! The canonical form of an address (R13 P7 §7.6, "Address form"). Every
//! session address and every machine address of the board fence is kept and
//! compared in this form.

use std::net::IpAddr;

/// The canonical text of `ip`: an IPv4-mapped IPv6 address becomes its IPv4
/// address; an IPv6 address is in its compressed lowercase form. The text
/// has no port and no zone (an [`IpAddr`] holds neither).
#[must_use]
pub fn canonical(ip: IpAddr) -> String {
    ip.to_canonical().to_string()
}

/// True for a loopback address or a link-local address, in canonical form.
#[must_use]
pub(crate) fn loopback_or_link_local(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unicast_link_local(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn text(s: &str) -> String {
        canonical(s.parse().unwrap())
    }

    #[test]
    fn fence_canonical_keeps_ipv4() {
        assert_eq!(text("192.168.1.5"), "192.168.1.5");
        assert_eq!(text("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn fence_canonical_turns_an_ipv4_mapped_ipv6_into_its_ipv4() {
        assert_eq!(text("::ffff:192.168.1.5"), "192.168.1.5");
        assert_eq!(text("::FFFF:C0A8:0105"), "192.168.1.5");
        assert_eq!(text("::ffff:127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn fence_canonical_writes_ipv6_compressed_and_lowercase() {
        assert_eq!(text("FD7A:115C:A1E0:0:0:0:0:A"), "fd7a:115c:a1e0::a");
        assert_eq!(
            text("2001:0db8:0000:0000:0000:0000:0000:0001"),
            "2001:db8::1"
        );
        assert_eq!(text("0:0:0:0:0:0:0:1"), "::1");
        // The two spellings of one address are equal.
        assert_eq!(text("2001:DB8::1"), text("2001:db8:0:0:0:0:0:1"));
    }

    #[test]
    fn fence_loopback_and_link_local_are_found_after_the_mapping() {
        for yes in [
            "127.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "169.254.1.1",
            "fe80::1",
        ] {
            assert!(loopback_or_link_local(yes.parse().unwrap()), "{yes}");
        }
        for no in ["10.0.0.1", "fd7a:115c:a1e0::a", "::ffff:10.0.0.1"] {
            assert!(!loopback_or_link_local(no.parse().unwrap()), "{no}");
        }
    }
}
