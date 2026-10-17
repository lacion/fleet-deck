//! The export of a move (FG-9, design §12.3 step 2, ADR-010 §5.1): what the
//! source refuses, the cap, and the manifest.

use std::fs;
use std::os::unix::fs::FileExt;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MOVE_MAX_BYTES, code};
use ring::digest;

use super::super::move_export::{TOO_LARGE, hex};
use super::tests::{NATIVE, Rig, claude_line};

#[test]
fn a_link_a_folder_or_a_missing_file_in_place_of_the_transcript_is_refused() {
    let rig = Rig::new();
    let main = rig.claude_main();
    // Missing.
    let err = rig.export(ProviderKind::Claude).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (
            "invalid_state",
            "The conversation files of this session were not found."
        )
    );
    // A link to a good file.
    let real = rig.root.join("real.jsonl");
    rig.write(&real, claude_line(NATIVE, 1).as_bytes());
    fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &main).unwrap();
    assert_eq!(
        rig.export(ProviderKind::Claude).unwrap_err().code,
        "invalid_state"
    );
    // A folder.
    fs::remove_file(&main).unwrap();
    fs::create_dir(&main).unwrap();
    assert_eq!(
        rig.export(ProviderKind::Claude).unwrap_err().code,
        "invalid_state"
    );
}

#[test]
fn a_subagent_link_is_skipped_and_never_followed() {
    let rig = Rig::new();
    rig.write(&rig.claude_main(), claude_line(NATIVE, 1).as_bytes());
    let secret = rig.root.join("secret.txt");
    rig.write(&secret, b"secret\n");
    let link = rig.claude_sub("agent-a1.jsonl");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    let exported = rig.export(ProviderKind::Claude).unwrap();
    assert_eq!(exported.manifest.files.len(), 1);
}

#[test]
fn a_transfer_over_64_mib_is_refused_before_any_byte_is_sent() {
    let rig = Rig::new();
    let main = rig.claude_main();
    rig.write(&main, claude_line(NATIVE, 1).as_bytes());
    // A sparse file: one byte over the cap.
    fs::OpenOptions::new()
        .write(true)
        .open(&main)
        .unwrap()
        .set_len(MOVE_MAX_BYTES + 1)
        .unwrap();
    let err = rig.export(ProviderKind::Claude).unwrap_err();
    assert_eq!(err.code, code::MOVE_TOO_LARGE);
    assert_eq!(err.message, TOO_LARGE);
    // Exactly the cap is allowed.
    fs::OpenOptions::new()
        .write(true)
        .open(&main)
        .unwrap()
        .set_len(MOVE_MAX_BYTES)
        .unwrap();
    let exported = rig.export(ProviderKind::Claude).unwrap();
    assert_eq!(exported.manifest.total, MOVE_MAX_BYTES);
}

#[test]
fn the_manifest_has_the_size_and_sha256_of_each_file_read_from_its_descriptor() {
    let rig = Rig::new();
    let body = format!("{}{}", claude_line(NATIVE, 1), claude_line(NATIVE, 2));
    rig.write(&rig.claude_main(), body.as_bytes());
    let sub = b"{\"sub\":1}\n";
    rig.write(&rig.claude_sub("agent-a1.jsonl"), sub);
    let exported = rig.export(ProviderKind::Claude).unwrap();
    let manifest = &exported.manifest;
    assert_eq!(manifest.provider, ProviderKind::Claude);
    assert_eq!(manifest.native_session_id, NATIVE);
    assert_eq!(manifest.total, (body.len() + sub.len()) as u64);
    let sha = |bytes: &[u8]| hex(digest::digest(&digest::SHA256, bytes).as_ref());
    assert_eq!(manifest.files[0].key, "main");
    assert_eq!(manifest.files[0].size, body.len() as u64);
    assert_eq!(manifest.files[0].sha256, sha(body.as_bytes()));
    assert_eq!(manifest.files[1].key, "subagent:a1");
    assert_eq!(manifest.files[1].sha256, sha(sub));
    assert!(manifest.files[0].rel.ends_with(&format!("{NATIVE}.jsonl")));
    // The open file is the file that the manifest describes.
    let mut read = vec![0_u8; body.len()];
    exported.files[0].file.read_exact_at(&mut read, 0).unwrap();
    assert_eq!(read, body.as_bytes());
}

#[test]
fn a_transcript_that_names_another_conversation_is_not_exported() {
    let rig = Rig::new();
    rig.write(
        &rig.claude_main(),
        claude_line("someone-else", 1).as_bytes(),
    );
    assert_eq!(
        rig.export(ProviderKind::Claude).unwrap_err().code,
        "invalid_state"
    );
}
