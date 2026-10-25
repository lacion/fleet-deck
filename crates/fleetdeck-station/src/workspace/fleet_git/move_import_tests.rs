//! Tests of the manifest checks, the staging receiver and the delete by
//! identity (FG-9, design §12.3 step 3, T4.2c).

use std::fs;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::Path;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MOVE_MAX_BYTES, TranscriptManifest, code};

use super::{
    Receiver, Removal, check_manifest, delete_file_by_identity, delete_folder_by_identity,
};
use crate::workspace::fleet_git::move_layout::staged_name;
use crate::workspace::fleet_git::move_layout::tests::{NATIVE, Rig, claude_line};

/// A manifest of two Claude files (the main file and one subagent file) and
/// their bytes in manifest order.
fn manifest_and_bytes() -> (TranscriptManifest, Vec<Vec<u8>>) {
    let rig = Rig::new();
    let body = format!("{}{}", claude_line(NATIVE, 1), claude_line(NATIVE, 2));
    rig.write(&rig.claude_main(), body.as_bytes());
    rig.write(&rig.claude_sub("agent-a1.jsonl"), b"{\"x\":1}\n");
    let exported = rig.export(ProviderKind::Claude).unwrap();
    let mut bytes = Vec::new();
    for open in &exported.files {
        let mut buf = vec![0_u8; usize::try_from(open.entry.size).unwrap()];
        open.file.read_exact_at(&mut buf, 0).unwrap();
        bytes.push(buf);
    }
    (exported.manifest, bytes)
}

fn all(bytes: &[Vec<u8>]) -> Vec<u8> {
    bytes.concat()
}

#[test]
fn the_receiver_writes_staged_files_with_mode_0600_in_a_folder_with_mode_0700() {
    let (manifest, bytes) = manifest_and_bytes();
    let dir = tempfile::tempdir().unwrap();
    let moves = dir.path().join("moves");
    let mut receiver = Receiver::create(&moves, "m1", &manifest).unwrap();
    let stream = all(&bytes);
    // The bytes come in small odd pieces that cross the file boundary.
    for piece in stream.chunks(7) {
        receiver.feed(piece).unwrap();
    }
    let folder = receiver.finish().unwrap();
    let meta = fs::metadata(&folder.path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    assert_eq!((meta.dev(), meta.ino()), (folder.dev, folder.ino));
    for (index, want) in bytes.iter().enumerate() {
        let path = folder.path.join(staged_name(index));
        assert_eq!(&fs::read(&path).unwrap(), want);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn a_receiver_refuses_a_bad_hash_a_short_stream_and_too_many_bytes() {
    let (manifest, bytes) = manifest_and_bytes();
    let dir = tempfile::tempdir().unwrap();
    // A changed byte: the hash of the first file does not match.
    let mut bad = all(&bytes);
    bad[3] ^= 0xff;
    let mut receiver = Receiver::create(&dir.path().join("a"), "m1", &manifest).unwrap();
    let err = receiver.feed(&bad).unwrap_err();
    assert_eq!(err.code, code::MOVE_CHECKSUM);
    // A short stream.
    let stream = all(&bytes);
    let mut receiver = Receiver::create(&dir.path().join("b"), "m1", &manifest).unwrap();
    receiver.feed(&stream[..stream.len() - 1]).unwrap();
    let err = receiver.finish().err().unwrap();
    assert_eq!(err.code, code::MOVE_CHECKSUM);
    // One byte more than the total.
    let mut long = stream;
    long.push(b'x');
    let mut receiver = Receiver::create(&dir.path().join("c"), "m1", &manifest).unwrap();
    let err = receiver.feed(&long).unwrap_err();
    assert_eq!(err.code, code::MOVE_TOO_LARGE);
}

#[test]
fn the_staging_folder_is_new_and_a_second_one_for_the_move_is_refused() {
    let (manifest, _) = manifest_and_bytes();
    let dir = tempfile::tempdir().unwrap();
    let moves = dir.path().join("moves");
    assert!(Receiver::create(&moves, "m1", &manifest).is_ok());
    let err = Receiver::create(&moves, "m1", &manifest).err().unwrap();
    assert_eq!(err.code, "internal");
}

#[test]
fn a_bad_manifest_is_refused_at_prepare_with_the_right_code() {
    let (good, _) = manifest_and_bytes();
    assert!(check_manifest(&good).is_ok());
    type Case = (&'static str, fn(&mut TranscriptManifest), &'static str);
    let cases: [Case; 8] = [
        (
            "provider",
            |m| m.provider = ProviderKind::Unknown,
            "invalid_params",
        ),
        (
            "native id",
            |m| m.native_session_id = "a b".into(),
            "invalid_params",
        ),
        ("no files", |m| m.files.clear(), "invalid_params"),
        ("sum", |m| m.total += 1, "invalid_params"),
        (
            "dot dot",
            |m| m.files[0].rel = "a/../b".into(),
            "invalid_params",
        ),
        (
            "absolute",
            |m| m.files[0].rel = "/etc/x".into(),
            "invalid_params",
        ),
        (
            "sha",
            |m| m.files[0].sha256 = "ABC".into(),
            code::MOVE_CHECKSUM,
        ),
        (
            "cap",
            |m| {
                m.files[0].size = MOVE_MAX_BYTES;
                m.total = MOVE_MAX_BYTES + m.files[1].size;
            },
            code::MOVE_TOO_LARGE,
        ),
    ];
    for (name, change, want) in cases {
        let mut manifest = good.clone();
        change(&mut manifest);
        let err = check_manifest(&manifest).unwrap_err();
        assert_eq!(err.code, want, "{name}");
    }
    let mut many = good.clone();
    while many.files.len() <= 256 {
        many.files.push(good.files[1].clone());
    }
    many.total = many.files.iter().map(|f| f.size).sum();
    assert_eq!(check_manifest(&many).unwrap_err().code, "invalid_params");
}

#[test]
fn delete_by_identity_removes_the_same_file_and_keeps_a_changed_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    fs::write(&file, "a").unwrap();
    let meta = fs::metadata(&file).unwrap();
    let (dev, ino) = (meta.dev(), meta.ino());
    // Another file at the path: kept.
    fs::remove_file(&file).unwrap();
    fs::write(&file, "b").unwrap();
    let other = fs::metadata(&file).unwrap();
    if other.ino() != ino {
        assert_eq!(delete_file_by_identity(&file, dev, ino), Removal::Changed);
        assert!(file.exists());
    }
    let meta = fs::metadata(&file).unwrap();
    assert_eq!(
        delete_file_by_identity(&file, meta.dev(), meta.ino()),
        Removal::Removed
    );
    assert_eq!(
        delete_file_by_identity(&file, meta.dev(), meta.ino()),
        Removal::Missing
    );
    // A link is not a regular file.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(dir.path(), &link).unwrap();
    let meta = fs::symlink_metadata(&link).unwrap();
    assert_eq!(
        delete_file_by_identity(&link, meta.dev(), meta.ino()),
        Removal::Changed
    );
}

#[test]
fn delete_folder_by_identity_removes_the_folder_with_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("moves/m1");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("000"), "x").unwrap();
    let meta = fs::metadata(&folder).unwrap();
    assert_eq!(
        delete_folder_by_identity(&folder, meta.dev(), meta.ino() + 1),
        Removal::Changed
    );
    assert!(Path::new(&folder).exists());
    assert_eq!(
        delete_folder_by_identity(&folder, meta.dev(), meta.ino()),
        Removal::Removed
    );
    assert!(!folder.exists());
}
