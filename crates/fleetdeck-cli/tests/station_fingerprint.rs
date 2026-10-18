//! `fleetdeck station fingerprint` as a process (P8 T2): the refusals with no
//! station, the clap rule for `--probe-providers`, and the output of a real
//! station (one `fleetdeck station serve` process with its local socket).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use common::{Station, fleetdeck};
use fleetdeck_core::identity::Identity;
use fleetdeck_testkit::TempNode;

fn grouped(hex: &str) -> String {
    hex.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn fingerprint_with_no_station_is_unavailable_exit_1_and_empty_stdout() {
    let node = TempNode::new("idle").unwrap();
    for args in [
        &["station", "fingerprint"][..],
        &["station", "fingerprint", "--json"][..],
        &["station", "fingerprint", "--json", "--probe-providers"][..],
    ] {
        let out = fleetdeck(&node).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8(out.stderr).unwrap(),
            "unavailable: The local station is not running.\n",
            "{args:?}"
        );
    }
}

#[test]
fn fingerprint_probe_providers_without_json_is_a_clap_error() {
    let node = TempNode::new("idle").unwrap();
    let out = fleetdeck(&node)
        .args(["station", "fingerprint", "--probe-providers"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn fingerprint_of_a_running_station_is_grouped_and_json_has_the_design_fields() {
    let mut station = Station::start();
    let identity = Identity::load(&station.node.dirs().station()).unwrap();
    let hex = identity.fingerprint().to_string();

    // The default output is exactly the grouped fingerprint and a newline.
    let text = station.ok(&["station", "fingerprint"]);
    assert_eq!(text, format!("{}\n", grouped(&hex)));
    assert_eq!(text.trim_end().split(' ').count(), 16);

    // `--json`: one compact line with the fields of the design.
    let json = station.ok(&["station", "fingerprint", "--json"]);
    assert!(
        json.ends_with('\n') && json.matches('\n').count() == 1,
        "{json}"
    );
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["status"], "fingerprint");
    assert_eq!(
        value["features"],
        serde_json::json!(["station_provision.v1"])
    );
    assert_eq!(value["fingerprint"], hex.as_str());
    // An unpaired station.
    assert!(value["paired_deck"].is_null());
    assert_eq!(value["link_up"], false);
    assert!(value["providers"].is_null());
    assert!(value["build"]["version"].is_string());
    assert!(value["build"]["commit"].is_string());
    assert!(value["build"]["release"].is_boolean());
    let sha = value["binary_sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64);
    assert!(
        sha.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );

    // `--probe-providers`: the fake Claude of the harness, probed again.
    let probed = station.ok(&["station", "fingerprint", "--json", "--probe-providers"]);
    let value: serde_json::Value = serde_json::from_str(&probed).unwrap();
    let providers = value["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "{probed}");
    assert!(providers[0]["version"].is_string(), "{probed}");
    station.stop();
}
