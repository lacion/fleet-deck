//! `fleetdeck station join --tailnet` against a running station process
//! (DL-178, tailnet design 5.2 and the station CLI tests of 8): the refusals
//! print `<code>: <detail>` with exit code 1, a host that is not in
//! `tailscale status` is refused, the probe needs a terminal, and `--cancel`
//! removes the retry file only. The full approval flow with a deck is
//! tested in `fleetdeck-station` (a test host has no tailnet address to
//! dial).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::Station;
use fleetdeck_link::pair_request::CanonicalRequest;
use fleetdeck_link::retry_file::{self, RetryRecord};
use fleetdeck_proto::link::Scope;

const STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "box", "DNSName": "box.tail1234.ts.net.", "UserID": 1,
           "TailscaleIPs": ["100.64.0.30"]},
  "Peer": {
    "nodekey:d": {"HostName": "deck-box", "DNSName": "deck-box.tail1234.ts.net.", "UserID": 1,
                  "TailscaleIPs": ["100.64.0.20"], "Online": true}
  },
  "User": {"1": {"LoginName": "owner@example.com"}}
}"#;

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A station whose `tailscale_bin` is a fake program that prints `status`.
fn station(status: &str) -> Station {
    let station = Station::new();
    let dir = station.node.root().to_path_buf();
    std::fs::write(dir.join("status.json"), status).unwrap();
    let bin = dir.join("tailscale");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\ncat '{}/status.json'\n", dir.display()),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A top-level key goes before the provider tables.
    let text = std::fs::read_to_string(station.node.dirs().station_config()).unwrap();
    station
        .node
        .write_station_toml(&format!("tailscale_bin = \"{}\"\n{text}", bin.display()))
        .unwrap();
    let mut station = station;
    station.spawn(None);
    station
}

/// The stderr of a command that must fail with exit code 1. A refusal is
/// exactly `<code>: <detail>`, without a program prefix; the code is a
/// lower-case word with underscores.
fn stderr(station: &Station, args: &[&str]) -> String {
    let err = station.fails(args);
    let (code, _) = err.split_once(": ").unwrap_or(("", ""));
    assert!(
        !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
        "not a `<code>: <detail>` refusal: {err}"
    );
    err
}

fn retry_dir(station: &Station) -> std::path::PathBuf {
    station.node.dirs().station()
}

fn save(dir: &Path, host: &str) -> CanonicalRequest {
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    retry_file::write(dir, &RetryRecord::new(host, 7421, FP, &req)).unwrap();
    req
}

#[test]
fn a_host_that_is_not_in_tailscale_status_is_refused_with_its_code() {
    let station = station(STATUS);
    for host in ["example.com", "100.64.0.99", "box.tail1234.ts.net"] {
        let err = stderr(
            &station,
            &[
                "station",
                "join",
                "--tailnet",
                host,
                "--deck-fingerprint",
                FP,
            ],
        );
        assert!(err.contains("not_in_tailnet:"), "{host}: {err}");
        assert!(err.contains("tailscale status"), "{err}");
    }
    assert!(retry_file::read(&retry_dir(&station)).unwrap().is_none());
}

#[test]
fn a_station_without_tailscale_says_so() {
    let station = station(STATUS);
    // A status that is not running.
    std::fs::write(
        station.node.root().join("status.json"),
        r#"{"BackendState": "Stopped"}"#,
    )
    .unwrap();
    let err = stderr(
        &station,
        &[
            "station",
            "join",
            "--tailnet",
            "deck-box",
            "--deck-fingerprint",
            FP,
        ],
    );
    assert!(err.contains("tailscale_unavailable:"), "{err}");
}

#[test]
fn the_probe_needs_a_terminal_to_confirm_the_fingerprint() {
    let station = station(STATUS);
    let err = stderr(&station, &["station", "join", "--tailnet", "deck-box"]);
    assert!(
        err.contains(
            "fingerprint_required: give --deck-fingerprint (no terminal to confirm the fingerprint)"
        ),
        "{err}"
    );
    assert!(retry_file::read(&retry_dir(&station)).unwrap().is_none());
}

#[test]
fn bad_values_are_refused_before_anything_is_sent() {
    let station = station(STATUS);
    let cases: [(&[&str], &str); 6] = [
        (
            &["--tailnet", "deck-box", "--deck-fingerprint", "abcd"],
            "invalid_request:",
        ),
        (
            &[
                "--tailnet",
                "deck-box",
                "--deck-fingerprint",
                FP,
                "--scopes",
                "coordinate",
            ],
            "invalid_request:",
        ),
        (
            &[
                "--tailnet",
                "deck-box",
                "--deck-fingerprint",
                FP,
                "--scopes",
                "read,root",
            ],
            "invalid_request:",
        ),
        (
            &[
                "--tailnet",
                "deck-box",
                "--deck-fingerprint",
                FP,
                "--name",
                "",
            ],
            "invalid_request:",
        ),
        (
            &["--tailnet", "deck-box:0", "--deck-fingerprint", FP],
            "invalid_request:",
        ),
        (&["--tailnet", "--deck-fingerprint", FP], "invalid_request:"),
    ];
    for (flags, want) in cases {
        let mut args = vec!["station", "join"];
        args.extend(flags);
        let err = stderr(&station, &args);
        assert!(err.contains(want), "{flags:?}: {err}");
    }
    assert!(retry_file::read(&retry_dir(&station)).unwrap().is_none());
}

#[test]
fn a_pending_request_of_another_deck_blocks_a_join_until_cancel() {
    let station = station(STATUS);
    let dir = retry_dir(&station);
    let req = save(&dir, "other.tail1234.ts.net");
    let err = stderr(
        &station,
        &[
            "station",
            "join",
            "--tailnet",
            "deck-box",
            "--deck-fingerprint",
            FP,
        ],
    );
    assert!(err.contains("pending:"), "{err}");
    assert!(
        err.contains("fleetdeck station join --tailnet --cancel"),
        "{err}"
    );
    // The file is untouched until the person cancels.
    assert_eq!(
        retry_file::read(&dir).unwrap().unwrap().request_id,
        req.id_hex()
    );

    let out = station.ok(&["station", "join", "--tailnet", "--cancel"]);
    assert!(out.contains("cancelled"), "{out}");
    assert!(retry_file::read(&dir).unwrap().is_none());
    let out = station.ok(&["station", "join", "--tailnet", "--cancel"]);
    assert!(out.contains("no pairing request was pending"), "{out}");
}

#[test]
fn an_invitation_join_still_needs_an_invitation_source() {
    let station = station(STATUS);
    // No terminal and no --stdin: the old behaviour, unchanged.
    let err = station.fails(&["station", "join"]);
    assert!(err.contains("standard input is not a terminal"), "{err}");
}
