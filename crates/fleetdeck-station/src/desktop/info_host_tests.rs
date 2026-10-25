//! R13 P7 §7.6: `DesktopInfo.host_addresses`: the filtering of loopback and
//! link-local addresses, the canonical form, the order and the cap.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};

use fleetdeck_core::local_addrs::LocalAddrs;

use super::{HOST_ADDRESSES_MAX, host_addresses};

fn v4(list: &[&str]) -> Option<BTreeSet<Ipv4Addr>> {
    Some(list.iter().map(|a| a.parse().unwrap()).collect())
}

fn v6(list: &[&str]) -> Option<BTreeSet<Ipv6Addr>> {
    Some(list.iter().map(|a| a.parse().unwrap()).collect())
}

#[test]
fn loopback_and_link_local_addresses_are_removed() {
    let addrs = LocalAddrs {
        v4: v4(&[
            "127.0.0.1",
            "127.1.2.3",
            "169.254.1.1",
            "169.254.255.254",
            "192.168.1.5",
            "100.64.0.9",
        ]),
        v6: v6(&[
            "::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "fd7a:115c:a1e0::9",
            "2001:db8::1",
        ]),
    };
    assert_eq!(
        host_addresses(&addrs),
        [
            "100.64.0.9",
            "192.168.1.5",
            "2001:db8::1",
            "fd7a:115c:a1e0::9",
            "fec0::1"
        ]
    );
}

#[test]
fn the_text_is_canonical() {
    let addrs = LocalAddrs {
        v4: v4(&["10.0.0.2"]),
        v6: v6(&[
            // An IPv4-mapped address is its IPv4 address (and a duplicate
            // of one that is already there collapses).
            "::ffff:10.0.0.2",
            "::ffff:192.0.2.9",
            // A mapped loopback or link-local address is removed too.
            "::ffff:127.0.0.1",
            "::ffff:169.254.0.7",
            // Lowercase, compressed.
            "2001:0DB8:0000:0000:0000:0000:0000:00AB",
        ]),
    };
    assert_eq!(
        host_addresses(&addrs),
        ["10.0.0.2", "192.0.2.9", "2001:db8::ab"]
    );
}

#[test]
fn the_list_is_sorted_by_text_and_capped() {
    let many: Vec<String> = (1..=30).map(|n| format!("10.0.0.{n}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let addrs = LocalAddrs {
        v4: v4(&refs),
        v6: v6(&["2001:db8::1"]),
    };
    let out = host_addresses(&addrs);
    assert_eq!(out.len(), HOST_ADDRESSES_MAX);
    let mut sorted = out.clone();
    sorted.sort();
    assert_eq!(out, sorted);
    // The first 16 of the text order: "10.0.0.1", "10.0.0.10" ...
    let mut expected: Vec<String> = many.clone();
    expected.push("2001:db8::1".to_owned());
    expected.sort();
    expected.truncate(HOST_ADDRESSES_MAX);
    assert_eq!(out, expected);
}

#[test]
fn an_unreadable_family_uses_the_other_and_both_unreadable_give_an_empty_list() {
    let only_v4 = LocalAddrs {
        v4: v4(&["192.0.2.1"]),
        v6: None,
    };
    assert_eq!(host_addresses(&only_v4), ["192.0.2.1"]);
    let only_v6 = LocalAddrs {
        v4: None,
        v6: v6(&["2001:db8::5"]),
    };
    assert_eq!(host_addresses(&only_v6), ["2001:db8::5"]);
    assert!(host_addresses(&LocalAddrs::default()).is_empty());
    let empty = LocalAddrs {
        v4: v4(&[]),
        v6: v6(&[]),
    };
    assert!(host_addresses(&empty).is_empty());
}
