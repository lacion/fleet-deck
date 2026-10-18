#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::net::IpAddr;

use fleetdeck_core::local_addrs::LocalAddrs;

use super::*;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn local(v4: &[&str], v6: &[&str]) -> LocalAddrs {
    LocalAddrs {
        v4: Some(v4.iter().map(|s| s.parse().unwrap()).collect()),
        v6: Some(v6.iter().map(|s| s.parse().unwrap()).collect()),
    }
}

#[test]
fn fence_machine_addresses_are_the_sorted_union_without_duplicates() {
    let got = machine_addresses_with(
        Some(ip("100.64.0.9")),
        &strings(&[
            "192.168.1.5",
            "100.64.0.9",
            "192.168.1.5",
            "FD7A:115C:A1E0::A",
        ]),
        None,
    );
    assert_eq!(got, ["100.64.0.9", "192.168.1.5", "fd7a:115c:a1e0::a"]);
}

#[test]
fn fence_the_link_peer_is_canonical_and_kept_even_when_it_is_loopback() {
    let got = machine_addresses_with(Some(ip("::ffff:10.0.0.7")), &[], None);
    assert_eq!(got, ["10.0.0.7"]);
    let got = machine_addresses_with(Some(ip("127.0.0.1")), &[], None);
    assert_eq!(got, ["127.0.0.1"]);
    assert!(machine_addresses_with(None, &[], None).is_empty());
}

#[test]
fn fence_host_addresses_skip_invalid_loopback_and_link_local() {
    let got = machine_addresses_with(
        None,
        &strings(&[
            "not an address",
            "",
            "10.0.0.1:8080",
            "fe80::1%eth0",
            "127.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "169.254.3.3",
            "fe80::abcd",
            "10.0.0.2",
            "::ffff:10.0.0.3",
        ]),
        None,
    );
    assert_eq!(got, ["10.0.0.2", "10.0.0.3"]);
}

#[test]
fn fence_host_addresses_keep_at_most_sixteen() {
    let many: Vec<String> = (1..=30).map(|i| format!("10.1.0.{i}")).collect();
    let got = machine_addresses_with(None, &many, None);
    assert_eq!(got.len(), 16);
    // The first sixteen valid ones, sorted as text.
    let mut want: Vec<String> = (1..=16).map(|i| format!("10.1.0.{i}")).collect();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn fence_the_local_station_adds_loopback_and_every_interface_address() {
    let l = local(
        &["192.168.1.5", "127.0.0.1", "100.64.0.1"],
        &["fd7a::1", "::1"],
    );
    let got = machine_addresses_with(Some(ip("10.9.9.9")), &strings(&["10.0.0.2"]), Some(&l));
    assert_eq!(
        got,
        [
            "10.0.0.2",
            "10.9.9.9",
            "100.64.0.1",
            "127.0.0.1",
            "192.168.1.5",
            "::1",
            "fd7a::1"
        ]
    );
    // Unreadable tables still give the two loopback addresses.
    let none = LocalAddrs::default();
    assert_eq!(
        machine_addresses_with(None, &[], Some(&none)),
        ["127.0.0.1", "::1"]
    );
}

#[test]
fn fence_a_station_that_is_not_local_gets_no_loopback() {
    let got = machine_addresses_with(Some(ip("10.0.0.7")), &[], None);
    assert!(!got.contains(&"127.0.0.1".to_owned()));
    assert!(!got.contains(&"::1".to_owned()));
}

#[test]
fn fence_at_most_forty_addresses_and_the_interface_addresses_give_way() {
    let v4: BTreeSet<String> = (1..=60).map(|i| format!("172.16.0.{i}")).collect();
    let l = LocalAddrs {
        v4: Some(v4.iter().map(|s| s.parse().unwrap()).collect()),
        v6: Some(BTreeSet::new()),
    };
    let hosts: Vec<String> = (1..=16).map(|i| format!("10.2.0.{i}")).collect();
    let got = machine_addresses_with(Some(ip("10.9.9.9")), &hosts, Some(&l));
    assert_eq!(got.len(), 40);
    // The peer, the reported addresses and the loopback addresses stay.
    for must in hosts
        .iter()
        .map(String::as_str)
        .chain(["10.9.9.9", "127.0.0.1", "::1"])
    {
        assert!(got.contains(&must.to_owned()), "{must}");
    }
    let mut sorted = got.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(got, sorted);
}

#[test]
fn fence_the_public_function_with_a_local_station_holds_loopback() {
    // The real interface tables are read; only the loopback pair is certain.
    let det = fleetdeck_testkit::Det::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = rt
        .block_on(DeckStore::open_in_memory(det.clock(), det.ids()))
        .unwrap();
    let got = machine_addresses(&store, None, &[], true);
    assert!(got.contains(&"127.0.0.1".to_owned()));
    assert!(got.contains(&"::1".to_owned()));
    assert!(got.len() <= 40);
    assert!(machine_addresses(&store, None, &[], false).is_empty());
}
