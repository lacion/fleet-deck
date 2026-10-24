#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use serde_json::json;

use super::*;

const MAIN: &str = "@openai/codex";
const I_MAIN: &str = "sha512-MAIN";
const I_PLAT: &str = "sha512-PLAT";

fn set() -> Vec<VerifiedPackage> {
    vec![
        VerifiedPackage {
            name: MAIN.into(),
            version: "0.163.0".into(),
            integrity: I_MAIN.into(),
        },
        VerifiedPackage {
            name: MAIN.into(),
            version: "0.163.0-linux-x64".into(),
            integrity: I_PLAT.into(),
        },
    ]
}

fn bin() -> Value {
    json!({"codex": "bin/codex.js"})
}

fn pkg(dir: &Path, name: &str, version: &str, with_bin: bool) {
    fs::create_dir_all(dir).unwrap();
    let mut doc = json!({"name": name, "version": version});
    if with_bin {
        doc["bin"] = bin();
    }
    fs::write(dir.join("package.json"), doc.to_string()).unwrap();
}

/// An npm tree: the platform package is nested in the main package.
fn npm_tree(root: &Path) -> (PathBuf, Lockfile) {
    let sm = root.join("node_modules");
    pkg(&sm.join("@openai/codex"), MAIN, "0.163.0", true);
    pkg(
        &sm.join("@openai/codex/node_modules/@openai/codex-linux-x64"),
        MAIN,
        "0.163.0-linux-x64",
        false,
    );
    let lock = json!({"name": "lib", "lockfileVersion": 3, "packages": {
        "node_modules/@openai/codex": {"version": "0.163.0", "integrity": I_MAIN},
        "node_modules/@openai/codex/node_modules/@openai/codex-linux-x64": {
            "name": MAIN, "version": "0.163.0-linux-x64", "integrity": I_PLAT},
    }});
    let path = sm.join(".package-lock.json");
    fs::write(&path, lock.to_string()).unwrap();
    (sm, Lockfile::Npm(path))
}

const BUN_LOCK: &str = r#"{
  "lockfileVersion": 1,
  "workspaces": { "": { "dependencies": { "@openai/codex": "^0.163.0", }, }, },
  "packages": {
    "@openai/codex": ["@openai/codex@0.163.0", "", { "optionalDependencies": { "@openai/codex-linux-x64": "npm:@openai/codex@0.163.0-linux-x64", }, }, "sha512-MAIN"],
    "@openai/codex-linux-x64": ["@openai/codex@0.163.0-linux-x64", "", {}, "sha512-PLAT"],
  }
}
"#;

fn bun_tree(root: &Path, lock: &str) -> (PathBuf, Lockfile) {
    let sm = root.join("node_modules");
    pkg(&sm.join("@openai/codex"), MAIN, "0.163.0", true);
    pkg(
        &sm.join("@openai/codex-linux-x64"),
        MAIN,
        "0.163.0-linux-x64",
        false,
    );
    fs::create_dir_all(sm.join(".bin")).unwrap();
    let path = root.join("bun.lock");
    fs::write(&path, lock).unwrap();
    (sm, Lockfile::Bun(path))
}

#[test]
fn an_npm_tree_with_a_nested_platform_package_passes() {
    let d = tempfile::tempdir().unwrap();
    let (sm, lock) = npm_tree(d.path());
    let moved = verify_staged(&sm, lock, &set(), &bin()).unwrap();
    assert_eq!(moved, vec!["@openai/codex".to_string()]);
}

#[test]
fn a_bun_tree_with_trailing_commas_passes_and_moves_both_packages() {
    let d = tempfile::tempdir().unwrap();
    let (sm, lock) = bun_tree(d.path(), BUN_LOCK);
    let moved = verify_staged(&sm, lock, &set(), &bin()).unwrap();
    assert_eq!(
        moved,
        vec![
            "@openai/codex".to_string(),
            "@openai/codex-linux-x64".to_string()
        ]
    );
}

#[test]
fn a_comma_inside_a_string_stays() {
    let out = strip_trailing_commas("{\"a\": \"x,}\", \"b\": [1, 2,\n ], }");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["a"], "x,}");
    assert_eq!(v["b"], json!([1, 2]));
}

#[test]
fn one_changed_integrity_is_an_integrity_mismatch() {
    let d = tempfile::tempdir().unwrap();
    let (sm, lock) = npm_tree(d.path());
    let mut s = set();
    s[1].integrity = "sha512-OTHER".into();
    assert_eq!(
        verify_staged(&sm, lock, &s, &bin()),
        Err("integrity_mismatch")
    );
}

#[test]
fn an_extra_package_folder_is_unverified() {
    let d = tempfile::tempdir().unwrap();
    let (sm, lock) = npm_tree(d.path());
    pkg(&sm.join("evil"), "evil", "1.0.0", false);
    assert_eq!(
        verify_staged(&sm, lock, &set(), &bin()),
        Err("unverified_package")
    );
}

#[test]
fn a_lockfile_entry_outside_the_set_is_unverified() {
    let d = tempfile::tempdir().unwrap();
    let (sm, lock) = npm_tree(d.path());
    let Lockfile::Npm(path) = &lock else {
        unreachable!()
    };
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    doc["packages"]["node_modules/left-pad"] = json!({"version": "1.0.0", "integrity": "sha512-X"});
    fs::write(path, doc.to_string()).unwrap();
    assert_eq!(
        verify_staged(&sm, lock, &set(), &bin()),
        Err("unverified_package")
    );
}

#[test]
fn only_a_binary_bun_lockfile_is_tool_too_old() {
    let d = tempfile::tempdir().unwrap();
    let (sm, _) = bun_tree(d.path(), BUN_LOCK);
    fs::remove_file(d.path().join("bun.lock")).unwrap();
    fs::write(d.path().join("bun.lockb"), b"\0binary").unwrap();
    let lock = Lockfile::Bun(d.path().join("bun.lock"));
    assert_eq!(
        verify_staged(&sm, lock, &set(), &bin()),
        Err("tool_too_old")
    );
}

#[test]
fn a_changed_bin_map_is_an_unsupported_layout() {
    let d = tempfile::tempdir().unwrap();
    let (sm, lock) = npm_tree(d.path());
    let other = json!({"codex": "bin/other.js"});
    assert_eq!(
        verify_staged(&sm, lock, &set(), &other),
        Err("unsupported_layout")
    );
}

#[test]
fn a_folder_that_is_not_in_the_lockfile_is_unverified() {
    let d = tempfile::tempdir().unwrap();
    let (sm, lock) = npm_tree(d.path());
    // In the set, but not in the lockfile.
    let mut s = set();
    s.push(VerifiedPackage {
        name: "dep".into(),
        version: "1.0.0".into(),
        integrity: "x".into(),
    });
    pkg(&sm.join("dep"), "dep", "1.0.0", false);
    assert_eq!(
        verify_staged(&sm, lock, &s, &bin()),
        Err("unverified_package")
    );
}
