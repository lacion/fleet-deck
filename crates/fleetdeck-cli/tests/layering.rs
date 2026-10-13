//! R8.3 (DL-162): the crate layering of PLAN §3 as a test.
//!
//! - `fleetdeck-proto` depends on no other Fleet Deck crate (any kind).
//! - `fleetdeck-providers` does not depend on `fleetdeck-deck` or
//!   `fleetdeck-station` (through normal dependencies, also indirectly).
//! - The deck never branches on a provider name: a source scan of
//!   `crates/fleetdeck-deck/src` finds no `match`, `matches!`, `if let` or
//!   `==` on `ProviderKind::Claude` or `ProviderKind::Codex` in non-test
//!   code. The deck routes by capability, never by provider (PLAN §6.2).

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test code")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The declared dependencies of each workspace crate:
/// crate name → (dependency name, kind) where kind is `normal`, `dev` or
/// `build`. `--no-deps` lists the declared edges of the workspace crates.
fn declared_dependencies() -> BTreeMap<String, Vec<(String, String)>> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .arg("--manifest-path")
        .arg(root().join("Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON");
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|package| {
            let deps = package["dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .map(|d| {
                    let kind = d["kind"].as_str().unwrap_or("normal").to_owned();
                    (d["name"].as_str().expect("name").to_owned(), kind)
                })
                .collect();
            (package["name"].as_str().expect("name").to_owned(), deps)
        })
        .collect()
}

/// Every crate that `krate` reaches through normal dependencies.
fn normal_closure(
    graph: &BTreeMap<String, Vec<(String, String)>>,
    krate: &str,
) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![krate.to_owned()];
    while let Some(name) = todo.pop() {
        for (dep, kind) in graph.get(&name).into_iter().flatten() {
            if kind == "normal" && seen.insert(dep.clone()) {
                todo.push(dep.clone());
            }
        }
    }
    seen
}

#[test]
fn proto_depends_on_no_fleetdeck_crate() {
    let graph = declared_dependencies();
    let own: Vec<&(String, String)> = graph["fleetdeck-proto"]
        .iter()
        .filter(|(name, _)| name.starts_with("fleetdeck-") || name.starts_with("spike-"))
        .collect();
    assert!(own.is_empty(), "fleetdeck-proto depends on {own:?}");
}

#[test]
fn providers_do_not_depend_on_the_deck_or_the_station() {
    let graph = declared_dependencies();
    let reached = normal_closure(&graph, "fleetdeck-providers");
    for forbidden in ["fleetdeck-deck", "fleetdeck-station"] {
        assert!(
            !reached.contains(forbidden),
            "fleetdeck-providers reaches {forbidden}: {reached:?}"
        );
    }
}

/// The non-test source files under `dir` (a file named `tests.rs`, ending in
/// `_tests.rs`, or under a `tests` directory is a test file).
fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                source_files(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

/// The lines of `text` (before an inline `#[cfg(test)]` module) that branch
/// on a provider name: a match arm, `matches!`, `if let` or a comparison.
fn provider_branches(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "#[cfg(test)]" {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        let names_a_provider =
            trimmed.contains("ProviderKind::Claude") || trimmed.contains("ProviderKind::Codex");
        let branches = trimmed.contains("=>")
            || trimmed.contains("matches!")
            || trimmed.contains("if let")
            || trimmed.contains("==")
            || trimmed.contains("!=")
            || trimmed.ends_with('|')
            || trimmed.starts_with('|');
        if names_a_provider && branches {
            found.push((index + 1, trimmed.to_owned()));
        }
    }
    found
}

/// Files of the deck that may branch on a provider name. Empty: the deck
/// has none today. Add a file here only with a DECISIONS-LOG entry.
const ALLOWED: &[&str] = &[];

#[test]
fn the_deck_never_branches_on_a_provider_name() {
    let mut files = Vec::new();
    source_files(&root().join("crates/fleetdeck-deck/src"), &mut files);
    assert!(
        files.len() > 10,
        "the scan found only {} files",
        files.len()
    );
    let mut offences = Vec::new();
    for file in files {
        let rel = file.strip_prefix(root()).unwrap().display().to_string();
        if ALLOWED.iter().any(|a| rel.ends_with(a)) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read source");
        for (line, code) in provider_branches(&text) {
            offences.push(format!("{rel}:{line}: {code}"));
        }
    }
    assert!(
        offences.is_empty(),
        "the deck branches on a provider name:\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_scan_finds_a_match_a_comparison_and_ignores_tests_and_comments() {
    let source = "\
fn a(k: ProviderKind) -> u8 {
    match k {
        ProviderKind::Claude => 1,
        ProviderKind::Codex | ProviderKind::Unknown => 2,
    }
}
fn b(k: ProviderKind) -> bool { k == ProviderKind::Codex }
fn c(k: ProviderKind) -> bool { matches!(k, ProviderKind::Claude) }
// ProviderKind::Claude => in a comment
fn d() -> ProviderKind { ProviderKind::Claude }
#[cfg(test)]
mod tests {
    fn e(k: ProviderKind) { if let ProviderKind::Claude = k {} }
}
";
    let lines: Vec<usize> = provider_branches(source).iter().map(|(n, _)| *n).collect();
    assert_eq!(lines, vec![3, 4, 7, 8]);
}
