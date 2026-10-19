//! The hidden command `fleetdeck provision-import` as a process (P8 T6):
//! the exit codes, the empty stdout and the fixed stderr lines.
//!
//! The real file system type check runs here. A temporary directory on an
//! allowed type (ext4, xfs, btrfs, f2fs, iso9660) gives a successful import;
//! on another type (tmpfs, ...) the command refuses with `unsupported_medium`.
//! The test checks the exact line of the case that it finds and prints which
//! case ran. The destination rule checks every ancestor from `/`, so the
//! success case needs a private place below the source tree or `$HOME`; when
//! there is none it prints a note and stops.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The allowed types of the command (ISO, ext, xfs, btrfs, f2fs).
const MEDIA: [u64; 5] = [0x9660, 0xEF53, 0x5846_5342, 0x9123_683E, 0xF2F5_2010];

fn provision_import(args: &[&str], xdg_config_home: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleetdeck"));
    cmd.arg("provision-import")
        .args(args)
        .env_remove("FLEETDECK_DATA_DIR")
        .env_remove("XDG_DATA_HOME")
        .env("RUST_LOG", "warn");
    match xdg_config_home {
        Some(dir) => cmd.env("XDG_CONFIG_HOME", dir),
        None => cmd.env_remove("XDG_CONFIG_HOME"),
    };
    cmd.output().unwrap()
}

#[track_caller]
fn ends(out: &Output, code: i32, stderr: &str) {
    assert_eq!(out.status.code(), Some(code), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{out:?}");
}

/// A directory (0700) whose ancestors pass the rule of the writer: no
/// link, owned by root or this user, no group or other write bit.
fn private_base() -> Option<tempfile::TempDir> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let places = [Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))), home];
    places.into_iter().flatten().find_map(|place| {
        let place = place.canonicalize().ok()?;
        let dir = tempfile::Builder::new()
            .prefix("provision-import-")
            .tempdir_in(&place)
            .ok()?;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let me = fs::metadata(dir.path()).unwrap().uid();
        for ancestor in dir.path().ancestors().skip(1) {
            let meta = fs::symlink_metadata(ancestor).unwrap();
            let ok =
                meta.is_dir() && (meta.uid() == 0 || meta.uid() == me) && meta.mode() & 0o022 == 0;
            if !ok {
                eprintln!("no private place: {} fails the rule", ancestor.display());
                return None;
            }
        }
        Some(dir)
    })
}

#[test]
fn a_relative_source_is_invalid_request_with_exit_1() {
    let out = provision_import(&["--source", "provision.toml"], None);
    ends(
        &out,
        1,
        "invalid_request: The import arguments are not valid.\n",
    );
}

#[test]
fn the_command_is_not_in_the_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(!help.contains("provision-import"), "{help}");
}

#[test]
fn a_missing_source_argument_is_a_clap_error_with_exit_2() {
    let out = provision_import(&[], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
}

#[test]
fn a_real_run_follows_the_file_system_type_of_the_temporary_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("provision.toml");
    let text = format!(
        "version = 1\ndeck = \"deck.example.ts.net:7421\"\ndeck_fingerprint = \"{FP}\"\nname = \"laptop\"\n"
    );
    fs::write(&source, &text).unwrap();
    #[allow(
        clippy::unnecessary_fallible_conversions,
        reason = "f_type is u32 on macOS and i64 on Linux"
    )]
    let f_type = {
        let file = fs::File::open(&source).unwrap();
        u64::try_from(rustix::fs::fstatfs(&file).unwrap().f_type).unwrap_or(0) & 0xFFFF_FFFF
    };
    let source = source.to_str().unwrap();
    if MEDIA.contains(&f_type) {
        let Some(base) = private_base() else {
            eprintln!("case: allowed type {f_type:#x}, no private place: the run is skipped");
            return;
        };
        eprintln!("case: allowed type {f_type:#x}: the import succeeds");
        let cfg = base.path().join("cfg");
        let out = provision_import(&["--source", source], Some(&cfg));
        ends(&out, 0, "Provisioning file imported.\n");
        let dest = cfg.join("fleetdeck/provision.toml");
        assert_eq!(fs::read_to_string(&dest).unwrap(), text);
        assert_eq!(fs::metadata(&dest).unwrap().mode() & 0o777, 0o600);
        let again = provision_import(&["--source", source], Some(&cfg));
        ends(&again, 0, "Provisioning file already imported.\n");
    } else {
        eprintln!("case: type {f_type:#x} is not allowed: the import is refused");
        let cfg = tmp.path().join("cfg");
        let out = provision_import(&["--source", source], Some(&cfg));
        ends(
            &out,
            1,
            "unsupported_medium: The source file system is not supported for provisioning media.\n",
        );
        assert!(!cfg.exists());
    }
}
