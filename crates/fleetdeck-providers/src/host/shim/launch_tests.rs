//! Tests of the launch types and the executable hash (DL-194).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;

use super::*;
use crate::host::shim::exe_hash::sha256_file;

fn spec_json() -> serde_json::Value {
    serde_json::json!({
        "shim_id": "s",
        "incarnation": "i",
        "shims_dir": "/x",
        "socket_path": "/y",
        "buffer_bytes": 1,
        "exit_publish_after_ms": 2,
        "exit_retry_ms": 3,
        "program": [],
        "args": [],
        "cwd": [],
        "env": [],
    })
}

#[test]
fn a_launch_spec_without_secrets_parses_with_secrets_false() {
    let spec: LaunchSpec = serde_json::from_value(spec_json()).unwrap();
    assert!(!spec.secrets);
    let mut with = spec_json();
    with["secrets"] = serde_json::json!(true);
    let spec: LaunchSpec = serde_json::from_value(with).unwrap();
    assert!(spec.secrets);
}

#[test]
fn the_hello_reply_round_trips_on_the_wire() {
    let reply = LaunchReply::Hello {
        exe_sha256: "ab12".to_owned(),
        protocol: LAUNCH_PROTOCOL,
    };
    let text = serde_json::to_string(&reply).unwrap();
    assert_eq!(
        text,
        r#"{"reply":"hello","exe_sha256":"ab12","protocol":2}"#
    );
    assert_eq!(serde_json::from_str::<LaunchReply>(&text).unwrap(), reply);
}

#[test]
fn the_secrets_frame_debug_hides_the_token() {
    let frame = SecretsFrame {
        token: "hunter2-token".to_owned(),
    };
    assert_eq!(
        format!("{frame:?}"),
        r#"SecretsFrame { token: "<redacted>" }"#
    );
    assert!(!format!("{frame:#?}").contains("hunter2"));
}

#[test]
fn exe_hash_of_a_known_file_equals_the_ring_digest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    // More than one 64 KiB chunk.
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    let want = ring::digest::digest(&ring::digest::SHA256, &bytes);
    assert_eq!(
        sha256_file(&path).unwrap(),
        crate::host::shim::wire::hex(want.as_ref())
    );
}

#[test]
fn exe_hash_refuses_a_file_over_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(512 * 1024 * 1024 + 1).unwrap();
    drop(file);
    let error = sha256_file(&path).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("too large to hash"), "{error}");
}
