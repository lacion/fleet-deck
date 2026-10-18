//! The hidden command `fleetdeck provision-bootstrap` as a process (P8
//! T3b): the exit codes, the empty stdout and the fixed stderr lines.
//!
//! The bootstrap takes its lock and writes its state through rules that
//! check every directory from `/` (no group or other write bit), so a
//! directory below `/tmp` fails them. The cases use a private directory below
//! `CARGO_TARGET_TMPDIR` or `$HOME`; when there is none they print a note and
//! stop. The trusted manifest `/etc/fleetdeck-provision-release.json` is a
//! fixed path: a machine that has it skips the `release_untrusted` case.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fleetdeck_core::private_file::try_lock_private;

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const MANIFEST: &str = "/etc/fleetdeck-provision-release.json";

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A private directory (0700) whose ancestors pass the rule: no link, owned
/// by root or this user, not writable by group or others.
fn private_base() -> Option<tempfile::TempDir> {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(tmp).unwrap();
    // `target` can be a link to a shared disk whose folders fail the rule:
    // the home directory is the second place.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    [Some(tmp.to_path_buf()), home]
        .into_iter()
        .flatten()
        .find_map(|place| private_in(&place))
}

fn private_in(place: &Path) -> Option<tempfile::TempDir> {
    let place = place.canonicalize().ok()?;
    let dir = tempfile::Builder::new()
        .prefix("provision-bootstrap-")
        .tempdir_in(&place)
        .ok()?;
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

/// Runs `fleetdeck --data-dir <base>/data provision-bootstrap ...`.
fn bootstrap(base: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .arg("--data-dir")
        .arg(base.join("data"))
        .arg("provision-bootstrap")
        .args(args)
        .env("HOME", base.join("home"))
        .env_remove("FLEETDECK_DATA_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("RUST_LOG", "warn")
        .output()
        .unwrap()
}

/// A valid provisioning file in a private directory below `base`.
fn provision_file(base: &Path) -> PathBuf {
    let dir = base.join("provision");
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let path = dir.join("provision.toml");
    fs::write(
        &path,
        format!(
            "version = 1\ndeck = \"deck.example.ts.net:7421\"\ndeck_fingerprint = \"{FP}\"\nname = \"laptop\"\n"
        ),
    )
    .unwrap();
    chmod(&path, 0o600);
    path
}

#[track_caller]
fn ends(out: &Output, code: i32, stderr: &str) {
    assert_eq!(out.status.code(), Some(code), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{out:?}");
}

#[test]
fn a_relative_file_is_file_invalid_with_exit_2() {
    let Some(base) = private_base() else { return };
    let out = bootstrap(base.path(), &["--file", "provision.toml"]);
    ends(&out, 2, "provision: validated: file_invalid\n");
}

#[test]
fn without_a_trusted_manifest_the_release_is_untrusted_with_exit_2() {
    if Path::new(MANIFEST).exists() {
        eprintln!("note: {MANIFEST} exists on this machine; the case is skipped");
        return;
    }
    let Some(base) = private_base() else { return };
    let file = provision_file(base.path());
    let out = bootstrap(base.path(), &["--file", file.to_str().unwrap()]);
    ends(&out, 2, "provision: validated: release_untrusted\n");
    // The terminal condition is kept in the state file (0600).
    let state = base.path().join("data/station/provision-state.json");
    assert!(
        fs::read_to_string(&state)
            .unwrap()
            .contains("release_untrusted")
    );
    assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o600);
    // A second run shows the kept error and does the same.
    let again = bootstrap(base.path(), &["--file", file.to_str().unwrap()]);
    ends(&again, 2, "provision: validated: release_untrusted\n");
}

#[test]
fn a_second_instance_exits_0_without_output_while_the_lock_is_held() {
    let Some(base) = private_base() else { return };
    let station = base.path().join("data/station");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&station)
        .unwrap();
    let held = try_lock_private(&station.join("provision.lock"))
        .unwrap()
        .expect("the lock is free");
    let file = provision_file(base.path());
    let out = bootstrap(base.path(), &["--file", file.to_str().unwrap()]);
    ends(&out, 0, "");
    drop(held);
}

#[test]
fn the_command_is_hidden_from_the_help_but_runs() {
    let help = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success(), "{help:?}");
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(!text.contains("provision-bootstrap"), "{text}");
    let own = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .args(["provision-bootstrap", "--help"])
        .output()
        .unwrap();
    assert!(own.status.success(), "{own:?}");
    assert!(String::from_utf8_lossy(&own.stdout).contains("--file"));
}
