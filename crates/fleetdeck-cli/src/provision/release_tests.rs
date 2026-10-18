//! The release check (design `omarchy-p8.md` 5.2, D24): each rule.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io;

use super::effects::{Effects, Terminal};
use super::fake::{DESKTOP_BYTES, Fake, HOOK_BYTES, binary_sha, manifest_json, root_file};
use super::release::{self, DESKTOP_PATH, HOOK_PATH, MANIFEST_PATH};
use super::sha256_hex;

fn check(fake: &Fake) -> Result<release::Manifest, Terminal> {
    release::check(fake, &fake.build())
}

fn untrusted(fake: &Fake) {
    assert_eq!(check(fake).unwrap_err(), Terminal::RELEASE_UNTRUSTED);
}

fn mismatch(fake: &Fake) {
    assert_eq!(check(fake).unwrap_err(), Terminal::BUILD_MISMATCH);
}

#[test]
fn the_paths_are_the_contract_paths() {
    assert_eq!(MANIFEST_PATH, "/etc/fleetdeck-provision-release.json");
    assert_eq!(HOOK_PATH, "/usr/lib/fleetdeck/provision-first-login");
    assert_eq!(
        DESKTOP_PATH,
        "/etc/xdg/autostart/fleetdeck-provision.desktop"
    );
}

#[test]
fn a_good_release_passes_and_returns_the_manifest() {
    let fake = Fake::new();
    let manifest = check(&fake).unwrap();
    assert_eq!(manifest.release_version, "2.0.0");
    assert_eq!(manifest.commit, "abc1234");
    assert_eq!(manifest.binary_sha256, binary_sha());
    assert_eq!(
        manifest.hooks.provision_first_login_sha256,
        sha256_hex(HOOK_BYTES)
    );
    assert_eq!(
        manifest.hooks.autostart_desktop_sha256,
        sha256_hex(DESKTOP_BYTES)
    );
}

#[test]
fn the_manifest_owner_must_be_root() {
    let fake = Fake::new();
    fake.edit_file(MANIFEST_PATH, |f| f.uid = 1000);
    untrusted(&fake);
}

#[test]
fn the_manifest_mode_must_be_exactly_0444() {
    for mode in [0o644, 0o440, 0o400, 0o666, 0o4444] {
        let fake = Fake::new();
        fake.edit_file(MANIFEST_PATH, |f| f.mode = mode);
        untrusted(&fake);
    }
    // The file type bits do not count.
    let fake = Fake::new();
    fake.edit_file(MANIFEST_PATH, |f| f.mode = 0o100_444);
    check(&fake).unwrap();
}

#[test]
fn the_manifest_must_not_be_a_symlink() {
    let fake = Fake::new();
    fake.edit_file(MANIFEST_PATH, |f| f.symlink = true);
    untrusted(&fake);
}

#[test]
fn the_manifest_parent_must_be_root_owned_and_not_group_or_other_writable() {
    let fake = Fake::new();
    fake.edit_file(MANIFEST_PATH, |f| f.parent_uid = 1000);
    untrusted(&fake);
    for mode in [0o775, 0o757, 0o777, 0o770] {
        let fake = Fake::new();
        fake.edit_file(MANIFEST_PATH, |f| f.parent_mode = mode);
        untrusted(&fake);
    }
    let fake = Fake::new();
    fake.edit_file(MANIFEST_PATH, |f| f.parent_mode = 0o755);
    check(&fake).unwrap();
}

#[test]
fn the_manifest_is_at_most_64_kib() {
    let pad = |total: usize| {
        let mut text = manifest_json();
        text.push_str(&" ".repeat(total - text.len()));
        text
    };
    let fake = Fake::new();
    fake.set_file(
        MANIFEST_PATH,
        Ok(root_file(pad(64 * 1024).as_bytes(), 0o444)),
    );
    check(&fake).unwrap();
    fake.set_file(
        MANIFEST_PATH,
        Ok(root_file(pad(64 * 1024 + 1).as_bytes(), 0o444)),
    );
    untrusted(&fake);
}

#[test]
fn the_manifest_must_be_valid_json_of_version_1_without_unknown_keys() {
    let good = manifest_json();
    for text in [
        "not json".to_owned(),
        String::new(),
        good.replace("\"version\":1", "\"version\":2"),
        good.replacen('{', "{\"extra\":1,", 1),
        good.replace("\"hooks\"", "\"hooks2\""),
        good.replace("\"autostart_desktop_sha256\"", "\"x\""),
    ] {
        let fake = Fake::new();
        fake.set_file(MANIFEST_PATH, Ok(root_file(text.as_bytes(), 0o444)));
        untrusted(&fake);
    }
}

#[test]
fn an_unreadable_manifest_or_hook_is_untrusted() {
    for path in [MANIFEST_PATH, HOOK_PATH, DESKTOP_PATH] {
        let fake = Fake::new();
        fake.set_file(path, Err(io::ErrorKind::PermissionDenied.into()));
        untrusted(&fake);
        let fake = Fake::new();
        fake.files
            .lock()
            .unwrap()
            .remove(std::path::Path::new(path));
        untrusted(&fake);
    }
}

#[test]
fn the_running_build_must_be_a_release_build_equal_to_the_manifest() {
    let fake = Fake::new();
    let mut build = fake.build();
    build.release = false;
    assert_eq!(
        release::check(&fake, &build).unwrap_err(),
        Terminal::BUILD_MISMATCH
    );
    let mut build = fake.build();
    build.version = "2.0.1".to_owned();
    assert_eq!(
        release::check(&fake, &build).unwrap_err(),
        Terminal::BUILD_MISMATCH
    );
    let mut build = fake.build();
    build.commit = "def5678".to_owned();
    assert_eq!(
        release::check(&fake, &build).unwrap_err(),
        Terminal::BUILD_MISMATCH
    );
}

#[test]
fn the_binary_hash_must_equal_the_manifest() {
    let fake = Fake::new();
    *fake.own_sha256.lock().unwrap() = Some(sha256_hex(b"other binary"));
    mismatch(&fake);
    // An unreadable binary is a mismatch too.
    *fake.own_sha256.lock().unwrap() = None;
    mismatch(&fake);
}

#[test]
fn each_hook_hash_must_equal_the_manifest() {
    let fake = Fake::new();
    fake.edit_file(HOOK_PATH, |f| f.bytes.push(b'#'));
    untrusted(&fake);
    let fake = Fake::new();
    fake.edit_file(DESKTOP_PATH, |f| f.bytes.push(b'#'));
    untrusted(&fake);
}

#[test]
fn each_hook_must_be_root_owned_not_group_or_other_writable_and_not_a_link() {
    for path in [HOOK_PATH, DESKTOP_PATH] {
        let fake = Fake::new();
        fake.edit_file(path, |f| f.uid = 1000);
        untrusted(&fake);
        for mode in [0o666, 0o664, 0o775, 0o757, 0o777] {
            let fake = Fake::new();
            fake.edit_file(path, |f| f.mode = mode);
            untrusted(&fake);
        }
        let fake = Fake::new();
        fake.edit_file(path, |f| f.symlink = true);
        untrusted(&fake);
        // The parent of a hook is not checked.
        let fake = Fake::new();
        fake.edit_file(path, |f| {
            f.parent_uid = 1000;
            f.parent_mode = 0o777;
        });
        check(&fake).unwrap();
    }
}

#[test]
fn the_manifest_is_judged_before_the_build() {
    // A bad manifest wins over a bad build.
    let fake = Fake::new();
    fake.edit_file(MANIFEST_PATH, |f| f.uid = 1000);
    let mut build = fake.build();
    build.release = false;
    assert_eq!(
        release::check(&fake, &build).unwrap_err(),
        Terminal::RELEASE_UNTRUSTED
    );
}
