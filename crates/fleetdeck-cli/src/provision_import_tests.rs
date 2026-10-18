//! Tests of `provision-import` (P8 T6).
//!
//! The write checks every ancestor of the destination from `/` (no group or
//! other write bit), so a directory below `/tmp` fails it. The tests that
//! write use a private place below the source tree or `$HOME`
//! (`private_base`); when there is none they print a note and stop. The
//! source can be anywhere: a fake file system type stands for the medium.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;

use super::*;

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const EXT4: i64 = 0xEF53;

fn good() -> String {
    format!(
        "version = 1\ndeck = \"deck.example.ts.net:7421\"\ndeck_fingerprint = \"{FP}\"\nname = \"laptop\"\n"
    )
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
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
        chmod(dir.path(), 0o700);
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

/// A source file with `text`, in a temporary directory.
fn source(text: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("provision.toml");
    fs::write(&path, text).unwrap();
    (dir, path)
}

fn ext4(_: &File) -> io::Result<i64> {
    Ok(EXT4)
}

fn code(result: Result<Outcome, Refused>) -> (String, String) {
    let e = result.unwrap_err();
    (e.code, e.detail)
}

#[test]
fn the_media_are_iso_ext_xfs_btrfs_and_f2fs() {
    for t in [0x9660, 0xEF53, 0x5846_5342, 0x9123_683E, 0xF2F5_2010] {
        assert!(medium_ok(t), "{t:#x}");
    }
    // vfat, exFAT, fuseblk (NTFS), tmpfs, NFS, overlayfs, 0.
    for t in [
        0x4d44,
        0x2011_BAB0,
        0x6573_5546,
        0x0102_1994,
        0x6969,
        0x794c_7630,
        0,
        -1,
    ] {
        assert!(!medium_ok(t), "{t:#x}");
    }
}

#[test]
fn a_relative_source_is_invalid_request() {
    let (c, d) = code(import(Path::new("provision.toml"), Path::new("/x"), &ext4));
    assert_eq!(
        (c.as_str(), d.as_str()),
        ("invalid_request", "The import arguments are not valid.")
    );
}

#[test]
fn a_good_file_is_imported_privately() {
    let Some(base) = private_base() else { return };
    let (_keep, src) = source(good().as_bytes());
    let cfg = base.path().join("cfg");
    assert_eq!(import(&src, &cfg, &ext4).unwrap(), Outcome::Imported);
    let dest = cfg.join("fleetdeck/provision.toml");
    assert_eq!(fs::read_to_string(&dest).unwrap(), good());
    assert_eq!(fs::metadata(&dest).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn a_missing_config_folder_is_created_with_mode_0700() {
    let Some(base) = private_base() else { return };
    let (_keep, src) = source(good().as_bytes());
    let cfg = base.path().join("a/b");
    import(&src, &cfg, &ext4).unwrap();
    for dir in [cfg.join("fleetdeck"), cfg.clone()] {
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700, "{dir:?}");
    }
}

#[test]
fn an_owned_0755_config_folder_is_accepted() {
    let Some(base) = private_base() else { return };
    let (_keep, src) = source(good().as_bytes());
    let dir = base.path().join("fleetdeck");
    fs::create_dir(&dir).unwrap();
    chmod(&dir, 0o755);
    assert_eq!(import(&src, base.path(), &ext4).unwrap(), Outcome::Imported);
    assert!(dir.join("provision.toml").is_file());
}

#[test]
fn a_group_writable_config_folder_is_unsafe_output() {
    let Some(base) = private_base() else { return };
    let (_keep, src) = source(good().as_bytes());
    let dir = base.path().join("fleetdeck");
    fs::create_dir(&dir).unwrap();
    chmod(&dir, 0o770);
    let (c, d) = code(import(&src, base.path(), &ext4));
    assert_eq!(
        (c.as_str(), d.as_str()),
        (
            "unsafe_output",
            "The destination folder is not private or contains a link."
        )
    );
    assert!(!dir.join("provision.toml").exists());
}

#[test]
fn a_config_folder_that_is_a_link_is_unsafe_output() {
    let Some(base) = private_base() else { return };
    let (_keep, src) = source(good().as_bytes());
    let real = base.path().join("real");
    fs::create_dir(&real).unwrap();
    chmod(&real, 0o700);
    symlink(&real, base.path().join("fleetdeck")).unwrap();
    assert_eq!(code(import(&src, base.path(), &ext4)).0, "unsafe_output");
}

#[test]
fn the_identical_file_is_a_success_and_is_not_written_again() {
    let Some(base) = private_base() else { return };
    let (_keep, src) = source(good().as_bytes());
    assert_eq!(import(&src, base.path(), &ext4).unwrap(), Outcome::Imported);
    let dest = base.path().join("fleetdeck/provision.toml");
    let before = fs::metadata(&dest).unwrap();
    assert_eq!(
        import(&src, base.path(), &ext4).unwrap(),
        Outcome::AlreadyImported
    );
    let after = fs::metadata(&dest).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.mtime_nsec(), after.mtime_nsec());
}

#[test]
fn a_different_file_is_refused_and_left_unchanged() {
    let Some(base) = private_base() else { return };
    let dir = base.path().join("fleetdeck");
    fs::create_dir(&dir).unwrap();
    chmod(&dir, 0o700);
    let dest = dir.join("provision.toml");
    fs::write(&dest, "other\n").unwrap();
    let (_keep, src) = source(good().as_bytes());
    let (c, d) = code(import(&src, base.path(), &ext4));
    assert_eq!(
        (c.as_str(), d.as_str()),
        (
            "output_exists",
            "A different provisioning file already exists."
        )
    );
    assert_eq!(fs::read_to_string(&dest).unwrap(), "other\n");
}

#[test]
fn an_existing_file_over_16_kib_and_an_existing_link_count_as_different() {
    let Some(base) = private_base() else { return };
    let dir = base.path().join("fleetdeck");
    fs::create_dir(&dir).unwrap();
    chmod(&dir, 0o700);
    let dest = dir.join("provision.toml");
    let (_keep, src) = source(good().as_bytes());

    let mut big = good().into_bytes();
    big.resize(MAX_BYTES + 100, b' ');
    fs::write(&dest, &big).unwrap();
    assert_eq!(code(import(&src, base.path(), &ext4)).0, "output_exists");
    assert_eq!(fs::read(&dest).unwrap(), big);

    fs::remove_file(&dest).unwrap();
    let target = base.path().join("target.toml");
    fs::write(&target, good()).unwrap();
    symlink(&target, &dest).unwrap();
    assert_eq!(code(import(&src, base.path(), &ext4)).0, "output_exists");
}

#[test]
fn a_link_source_is_unsafe_source() {
    let (dir, src) = source(good().as_bytes());
    let link = dir.path().join("link.toml");
    symlink(&src, &link).unwrap();
    let (c, d) = code(import(&link, Path::new("/x"), &ext4));
    assert_eq!(
        (c.as_str(), d.as_str()),
        (
            "unsafe_source",
            "The source is a link, not a regular file, or larger than 16 KiB."
        )
    );
}

#[test]
fn a_directory_source_is_unsafe_source() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        code(import(dir.path(), Path::new("/x"), &ext4)).0,
        "unsafe_source"
    );
}

#[test]
fn a_fifo_source_is_unsafe_source_and_does_not_block() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        Mode::RUSR | Mode::WUSR,
        0,
    )
    .unwrap();
    assert_eq!(
        code(import(&fifo, Path::new("/x"), &ext4)).0,
        "unsafe_source"
    );
}

#[test]
fn a_source_over_16_kib_is_unsafe_source() {
    let mut text = good().into_bytes();
    text.resize(MAX_BYTES + 1, b' ');
    let (_keep, src) = source(&text);
    assert_eq!(
        code(import(&src, Path::new("/x"), &ext4)).0,
        "unsafe_source"
    );
    // The limit itself is read (the later rules decide).
    text.truncate(MAX_BYTES);
    let (_keep, src) = source(&text);
    assert_eq!(
        import(&src, Path::new("/x"), &|_| Ok(0)).unwrap_err().code,
        "unsupported_medium"
    );
}

#[test]
fn a_missing_source_is_source_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let (c, d) = code(import(&dir.path().join("none"), Path::new("/x"), &ext4));
    assert_eq!(
        (c.as_str(), d.as_str()),
        ("source_unreadable", "The source file could not be read.")
    );
}

#[test]
fn a_failed_type_check_is_source_unreadable() {
    let (_keep, src) = source(good().as_bytes());
    let fail = |_: &File| Err(io::Error::other("fstatfs"));
    assert_eq!(
        code(import(&src, Path::new("/x"), &fail)).0,
        "source_unreadable"
    );
}

#[test]
fn an_unsupported_medium_is_refused_and_nothing_is_written() {
    let Some(base) = private_base() else { return };
    let (_keep, src) = source(good().as_bytes());
    let vfat = |_: &File| Ok(0x4d44);
    let (c, d) = code(import(&src, base.path(), &vfat));
    assert_eq!(
        (c.as_str(), d.as_str()),
        (
            "unsupported_medium",
            "The source file system is not supported for provisioning media."
        )
    );
    assert!(!base.path().join("fleetdeck").exists());
}

#[test]
fn a_bad_file_is_file_invalid() {
    let bad_name = good().replace("laptop", "");
    let bad_deck = good().replace("deck.example.ts.net:7421", "deck.example.ts.net");
    let upper = good().replace(FP, &FP.to_uppercase());
    let cases = [
        "this is not toml".to_owned(),
        format!("{}extra = 1\n", good()),
        bad_name,
        // Not the canonical `host:port` form (no port).
        bad_deck,
        upper,
        good().replace("version = 1", "version = 2"),
    ];
    for text in cases {
        let (_keep, src) = source(text.as_bytes());
        let (c, d) = code(import(&src, Path::new("/x"), &ext4));
        assert_eq!(
            (c.as_str(), d.as_str()),
            ("file_invalid", "The provisioning file is not valid."),
            "{text}"
        );
    }
}
