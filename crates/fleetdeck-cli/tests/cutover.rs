//! M3 7-7 (v0 cutover, DL-132) and two PLAN §4.1 Keep rows that are rules
//! about the tree, not about behaviour (6-13 review): no model calls in the
//! core, and fake agents with opt-in live smoke tests.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_v0_code_and_plugin_files_are_gone_from_v2() {
    let root = root();
    let gone = [
        "src",
        "board",
        "hooks",
        "scripts",
        ".claude-plugin",
        "package.json",
        "bin",
        "commands",
        "skills",
        "contracts",
        "demo",
        "tests",
        "bun.lock",
        "tsconfig.json",
        "compatibility.json",
        ".github/workflows/ci.yml",
        ".github/workflows/publish.yml",
    ];
    let left: Vec<_> = gone.iter().filter(|p| root.join(p).exists()).collect();
    assert!(left.is_empty(), "v0 paths still present: {left:?}");
    // The v1 record and the v2 entry points stay.
    for keep in [
        "docs/v0/README.md",
        "README.md",
        "web/package.json",
        ".github/workflows/v2.yml",
    ] {
        assert!(root.join(keep).exists(), "{keep} is missing");
    }
}

/// PLAN §4.1 "No model calls in the core": no crate source names a model
/// API endpoint; every model call goes through a provider CLI process.
#[test]
fn no_crate_calls_a_model_api_directly() {
    let mut files = Vec::new();
    rust_files(&root().join("crates"), &mut files);
    assert!(files.len() > 50, "the walk found the crates");
    let endpoints = [
        concat!("api.", "anthropic.com"),
        concat!("api.", "openai.com"),
        concat!("generativelanguage.", "googleapis.com"),
        concat!("/v1/", "messages"),
        concat!("/v1/chat/", "completions"),
        concat!("/v1/", "responses"),
    ];
    let mut hits = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        for endpoint in endpoints {
            if text.contains(endpoint) {
                hits.push(format!("{}: {endpoint}", file.display()));
            }
        }
    }
    assert!(hits.is_empty(), "model API endpoints in the core: {hits:?}");
}

/// PLAN §4.1 "Fake agents and test seams": the fake agents exist, and every
/// live test is `#[ignore]`d behind `FD_LIVE_SMOKE=1` (`make smoke-live`).
#[test]
fn normal_tests_use_fake_agents_and_live_smoke_tests_are_opt_in() {
    let root = root();
    for bin in ["fake-claude.rs", "fake-codex.rs"] {
        assert!(
            root.join("crates/fleetdeck-testkit/src/bin")
                .join(bin)
                .exists(),
            "{bin} is missing"
        );
    }
    let live = std::fs::read_to_string(root.join("crates/fleetdeck-providers/tests/live_smoke.rs"))
        .expect("read live smoke");
    let tests = live.matches("#[tokio::test]").count() + live.matches("#[test]").count();
    let ignored = live.matches("#[ignore = \"live:").count();
    assert!(tests >= 2, "one live smoke test per provider");
    assert_eq!(
        tests, ignored,
        "every live smoke test is ignored by default"
    );
    assert!(live.contains("FD_LIVE_SMOKE"));
    let make = std::fs::read_to_string(root.join("Makefile")).expect("read Makefile");
    assert!(make.contains("smoke-live:") && make.contains("FD_LIVE_SMOKE=1"));
}
