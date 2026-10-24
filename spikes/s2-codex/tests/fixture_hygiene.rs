//! Recorded Codex fixtures must not leak local paths or account data (DL-014).

#![allow(clippy::unwrap_used)]

use std::path::Path;

const FORBIDDEN: &[&str] = &[
    "/home/",
    "/Users/",
    "/tmp/fd-s2-",
    "/tmp/.tmp",
    "@gmail.",
    "\"email\":\"",
];

#[test]
fn no_fixture_leaks_a_local_path_or_account() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.to_string_lossy().ends_with(".jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        // Deltas can split a path; join the delta text of each item too.
        let mut joined = String::new();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if let Some(d) = v.pointer("/frame/params/delta").and_then(|d| d.as_str()) {
                joined.push_str(d);
            }
        }
        for f in FORBIDDEN {
            assert!(!text.contains(f), "{} contains {f}", path.display());
            assert!(!joined.contains(f), "{} deltas contain {f}", path.display());
        }
        checked += 1;
    }
    assert!(checked >= 8, "only {checked} fixtures");
}
