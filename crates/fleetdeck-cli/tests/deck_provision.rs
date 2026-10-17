//! `fleetdeck deck provision-file` as a process, with a fresh data directory
//! and no deck (P8 T1): every refusal prints one stderr line `<code>:
//! <text>` with exit 1 and nothing on stdout; a missing `--deck` is a clap
//! error with exit 2.
//!
//! The output path rules refuse any path below a directory that other users
//! can write (`/tmp`), so the cases that must pass the path check use a
//! private directory below `CARGO_TARGET_TMPDIR`. When a directory above it
//! fails the rule (for example a link), `private_base` gives `None` and those
//! cases print a note and stop: the path rules have their own tests in
//! `fleetdeck-core` (`private_file`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Output};

const DECK: &str = "deck.example.ts.net:7421";

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A fresh data directory (its own HOME) that holds no deck.
struct Node {
    root: tempfile::TempDir,
}

impl Node {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        chmod(root.path(), 0o700);
        Self { root }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
            .arg("--data-dir")
            .arg(self.root.path().join("data"))
            .env("HOME", self.root.path().join("home"))
            .env_remove("FLEETDECK_DATA_DIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("RUST_LOG", "warn")
            .args(args)
            .output()
            .unwrap()
    }
}

/// A private directory (0700) whose ancestors pass the rule of the writer:
/// no link, owned by root or this user, not writable by group or others.
fn private_base() -> Option<tempfile::TempDir> {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(tmp).unwrap();
    let tmp = tmp.canonicalize().unwrap();
    let dir = tempfile::Builder::new()
        .prefix("provision-file-")
        .tempdir_in(&tmp)
        .unwrap();
    chmod(dir.path(), 0o700);
    let me = fs::metadata(dir.path()).unwrap().uid();
    for ancestor in dir.path().ancestors().skip(1) {
        let meta = fs::symlink_metadata(ancestor).unwrap();
        let ok = meta.is_dir() && (meta.uid() == 0 || meta.uid() == me) && meta.mode() & 0o022 == 0;
        if !ok {
            eprintln!("no private place: {} fails the rule", ancestor.display());
            return None;
        }
    }
    Some(dir)
}

/// Asserts a refusal: exit 1, empty stdout, exactly this stderr line.
#[track_caller]
fn refused(out: &Output, line: &str) {
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), format!("{line}\n"));
}

const INVALID: &str = "invalid_request: The provisioning arguments are not valid.";

#[test]
fn provision_file_without_a_deck_the_command_is_unavailable() {
    let Some(base) = private_base() else { return };
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "laptop",
        "--deck",
        DECK,
        "--out",
        base.path().join("laptop.toml").to_str().unwrap(),
    ]);
    refused(&out, "unavailable: The local deck is not running.");
    assert!(!base.path().join("laptop.toml").exists());
    let left: Vec<_> = fs::read_dir(base.path()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn provision_file_a_bad_name_is_an_invalid_request() {
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "",
        "--deck",
        DECK,
        "--out",
        "/nonexistent/laptop.toml",
    ]);
    refused(&out, INVALID);
    let long = "n".repeat(65);
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        &long,
        "--deck",
        DECK,
        "--out",
        "/nonexistent/laptop.toml",
    ]);
    refused(&out, INVALID);
}

#[test]
fn provision_file_a_bad_deck_address_is_an_invalid_request() {
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "laptop",
        "--deck",
        "deck.example.ts.net:0",
        "--out",
        "/nonexistent/laptop.toml",
    ]);
    refused(&out, INVALID);
}

#[test]
fn provision_file_a_relative_out_is_an_invalid_request() {
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "laptop",
        "--deck",
        DECK,
        "--out",
        "laptop.toml",
    ]);
    refused(&out, INVALID);
}

#[test]
fn provision_file_an_existing_output_file_is_not_replaced() {
    let Some(base) = private_base() else { return };
    let path = base.path().join("laptop.toml");
    fs::write(&path, "keep me\n").unwrap();
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "laptop",
        "--deck",
        DECK,
        "--out",
        path.to_str().unwrap(),
    ]);
    refused(&out, "output_exists: The output path already exists.");
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep me\n");
}

#[test]
fn provision_file_a_dangling_symlink_as_output_counts_as_existing() {
    let Some(base) = private_base() else { return };
    let path = base.path().join("laptop.toml");
    std::os::unix::fs::symlink(base.path().join("nowhere"), &path).unwrap();
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "laptop",
        "--deck",
        DECK,
        "--out",
        path.to_str().unwrap(),
    ]);
    refused(&out, "output_exists: The output path already exists.");
    assert!(!base.path().join("nowhere").exists());
}

#[test]
fn provision_file_a_parent_with_mode_0755_is_unsafe() {
    // Below the private base when there is one, else below /tmp: both are
    // refused, the first because of the mode of the parent.
    let dir = private_base().unwrap_or_else(|| tempfile::tempdir().unwrap());
    chmod(dir.path(), 0o755);
    let path = dir.path().join("laptop.toml");
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "laptop",
        "--deck",
        DECK,
        "--out",
        path.to_str().unwrap(),
    ]);
    refused(
        &out,
        "unsafe_output: The output path is not private or contains a link.",
    );
    assert!(!path.exists());
}

#[test]
fn provision_file_a_path_below_tmp_is_unsafe() {
    let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    chmod(dir.path(), 0o700);
    let path = dir.path().join("laptop.toml");
    let out = Node::new().run(&[
        "deck",
        "provision-file",
        "laptop",
        "--deck",
        DECK,
        "--out",
        path.to_str().unwrap(),
    ]);
    refused(
        &out,
        "unsafe_output: The output path is not private or contains a link.",
    );
    assert!(!path.exists());
}

#[test]
fn provision_file_a_missing_deck_option_is_a_usage_error() {
    let out = Node::new().run(&["deck", "provision-file", "laptop", "--out", "/x/y.toml"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--deck"), "{err}");
}
