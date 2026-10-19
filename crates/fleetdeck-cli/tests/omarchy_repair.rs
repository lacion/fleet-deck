//! The CLI side of the installer repair (`board-gaps.md` §3, BG-2):
//! `omarchy install-plan`, `install --replace-conflicts --plan-stdin`,
//! `install --resume`, `repair-status`, `restore-plan` and `restore
//! --plan-stdin`: the JSON shapes, the exit codes 0, 1, 2, 3 and 4, and the
//! files after each command. A fake `$HOME` and a fake `PATH` (the rig of the
//! installer tests).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
#![allow(dead_code, reason = "the rig of the installer tests is shared")]

#[path = "omarchy_install/rig.rs"]
mod rig;

#[path = "omarchy_repair/r1.rs"]
mod r1;

use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use rig::{Rig, err, mode, out};

const MANIFEST: &str = "plugins/fleetdeck.fleet/manifest.json";
const POST_BOOT: &str = "hooks/post-boot.d/fleetdeck";
const POST_UPDATE: &str = "hooks/post-update.d/fleetdeck";
const BUSY: &str = "Another Omarchy change is running.";

fn asset(path: &str) -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/omarchy")
            .join(path),
    )
    .unwrap()
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn manifest(rig: &Rig) -> PathBuf {
    rig.omarchy_dir().join(MANIFEST)
}

fn post_boot(rig: &Rig) -> PathBuf {
    rig.omarchy_dir().join(POST_BOOT)
}

fn post_update(rig: &Rig) -> PathBuf {
    rig.omarchy_dir().join(POST_UPDATE)
}

fn journal_dir(rig: &Rig) -> PathBuf {
    rig.fd_dir().join("omarchy-repair")
}

/// Runs the command with `stdin` piped in.
fn run_with(rig: &Rig, args: &[&str], stdin: &str) -> Output {
    let mut cmd = rig.command(args);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let data = stdin.to_owned();
    let writer = std::thread::spawn(move || {
        let _ = pipe.write_all(data.as_bytes());
    });
    let o = child.wait_with_output().unwrap();
    writer.join().unwrap();
    o
}

/// The stdout of a command that prints one JSON line and nothing else.
fn json_of(o: &Output) -> Value {
    let text = out(o);
    assert_eq!(text.trim().lines().count(), 1, "stdout: {text}");
    serde_json::from_str(text.trim()).unwrap()
}

fn plan(rig: &Rig) -> Value {
    let o = rig.run(&["omarchy", "install-plan", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    json_of(&o)
}

fn replace(rig: &Rig, facts: &Value, edits: &[&str]) -> Output {
    let input = json!({"facts": facts, "replace_recorded_edits": edits});
    run_with(
        rig,
        &[
            "omarchy",
            "install",
            "--yes",
            "--replace-conflicts",
            "--plan-stdin",
        ],
        &input.to_string(),
    )
}

fn restore_plan(rig: &Rig, id: &str) -> Value {
    let o = rig.run(&["omarchy", "restore-plan", "--repair", id, "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    json_of(&o)
}

fn restore(rig: &Rig, facts: &Value, ids: &[&str]) -> Output {
    let input = json!({"facts": facts, "asset_ids": ids});
    run_with(
        rig,
        &["omarchy", "restore", "--plan-stdin"],
        &input.to_string(),
    )
}

fn status(rig: &Rig) -> Value {
    let o = rig.run(&["omarchy", "repair-status", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    json_of(&o)
}

/// Two unrecorded conflicts (the manifest and a hook), then a replacement
/// that took them. Returns the repair ID.
fn replaced_two(rig: &mut Rig) -> String {
    rig.station(false);
    rig.hypr("-- user\n");
    put(&manifest(rig), "mine manifest\n");
    put(&post_boot(rig), "mine hook\n");
    let facts = plan(rig);
    let o = replace(rig, &facts, &[]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    json_of(&o)["repair_id"].as_str().unwrap().to_owned()
}

fn hold_lock(rig: &Rig) -> std::fs::File {
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(rig.fd_dir().join("omarchy.lock"))
        .unwrap();
    lock.lock().unwrap();
    lock
}

fn ids_of(value: &Value) -> Vec<(String, String)> {
    value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["asset_id"].as_str().unwrap().to_owned(),
                f["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

// ---- install-plan ----

#[test]
fn omarchy_repair_install_plan_prints_the_facts_of_a_conflict() {
    let rig = Rig::new();
    put(&manifest(&rig), "mine manifest\n");
    let o = rig.run(&["omarchy", "install-plan", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let facts = json_of(&o);
    assert_eq!(facts["build_hash"].as_str().unwrap().len(), 64);
    assert!(facts["record_sha256"].is_null());
    let files = facts["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    let f = &files[0];
    assert_eq!(f["asset_id"], "fleet_plugin_manifest");
    assert_eq!(f["classification"], "unrecorded_conflict");
    assert_eq!(f["path"], manifest(&rig).display().to_string());
    assert_eq!(f["base"], "omarchy");
    assert_eq!(f["rel"], MANIFEST);
    assert_eq!(f["size"], 14);
    assert_eq!(f["mode"], 0o644);
    assert!(f["dev"].is_u64() && f["ino"].is_u64());
    assert_eq!(f["replaceable"], true);
    assert!(f["blocked"].is_null());
    assert_eq!(f["sha256"].as_str().unwrap().len(), 64);
    assert!(!journal_dir(&rig).exists());
    assert_eq!(read(&manifest(&rig)), "mine manifest\n");
    assert_eq!(plan(&rig), facts);
}

#[test]
fn omarchy_repair_install_plan_marks_a_link_and_a_hard_link_as_not_replaceable() {
    let rig = Rig::new();
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    let victim = outside.path().join("victim");
    std::fs::write(&victim, "victim\n").unwrap();
    std::fs::create_dir_all(manifest(&rig).parent().unwrap()).unwrap();
    symlink(&victim, manifest(&rig)).unwrap();
    put(&post_boot(&rig), "mine hook\n");
    std::fs::hard_link(post_boot(&rig), rig.root().join("other-name")).unwrap();
    let facts = plan(&rig);
    let files = facts["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    for f in files {
        assert_eq!(f["replaceable"], false, "{f}");
        assert_eq!(f["blocked"], "unsafe_path", "{f}");
        assert!(f["sha256"].is_null(), "{f}");
    }
    assert!(
        files[0]["dev"].is_u64(),
        "the identity is known where it can be"
    );
}

// ---- install --replace-conflicts ----

#[test]
fn omarchy_repair_replaces_every_conflict_keeps_the_originals_and_runs_the_install() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    put(&manifest(&rig), "mine manifest\n");
    put(&post_boot(&rig), "mine hook\n");
    let facts = plan(&rig);
    let o = replace(&rig, &facts, &[]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    // Stdout holds only the JSON; the human lines are on stderr.
    let result = json_of(&o);
    assert!(
        err(&o).contains("Fleet Deck is installed for Omarchy."),
        "{}",
        err(&o)
    );
    assert!(result["code"].is_null());
    let id = result["repair_id"].as_str().unwrap();
    assert_eq!(id.len(), 32);
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    let files = result["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    let originals = [
        ("mine manifest\n", manifest(&rig)),
        ("mine hook\n", post_boot(&rig)),
    ];
    for (f, (text, path)) in files.iter().zip(&originals) {
        assert_eq!(f["state"], "replaced");
        assert_eq!(f["path"], path.display().to_string());
        let backup = PathBuf::from(f["backup_path"].as_str().unwrap());
        assert_eq!(read(&backup), *text, "the original is kept");
        assert_eq!(backup.parent(), path.parent());
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".fleetdeck-conflict-")
        );
    }
    assert_eq!(read(&manifest(&rig)), asset("plugin/manifest.json"));
    assert_eq!(read(&post_boot(&rig)), asset("hooks/post-boot"));
    assert_eq!(mode(&post_boot(&rig)), 0o755);
    let status = rig.run(&["omarchy", "status"]);
    assert_eq!(status.status.code(), Some(0), "{}", out(&status));
    let record = rig.record();
    assert_eq!(record["files"].as_array().unwrap().len(), 12);
    for path in [MANIFEST, POST_BOOT] {
        let entry = record["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == path)
            .unwrap();
        assert_eq!(entry["existed_before"], true, "{path}");
    }
    assert_eq!(rig.calls("omarchy-shell").len(), 1);
    assert_eq!(record["plugin_enabled"], true);
    assert!(rig.hypr_text().contains("fleetdeck: managed line"));
    let journal = journal_dir(&rig).join(format!("{id}.json"));
    assert_eq!(mode(&journal), 0o600);
    assert_eq!(mode(&journal_dir(&rig)), 0o700);
}

#[test]
fn omarchy_repair_keeps_a_recorded_edit_unless_it_is_selected() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    assert_eq!(rig.install().status.code(), Some(0));
    put(&manifest(&rig), "edited manifest\n");
    put(&post_update(&rig), "edited hook\n");
    let facts = plan(&rig);
    let files = facts["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f["classification"] == "recorded_edit"));
    // An unselected recorded edit is kept; a selected one is replaced.
    let o = replace(&rig, &facts, &["hook_post_update"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert_eq!(
        ids_of(&result),
        vec![("hook_post_update".to_owned(), "replaced".to_owned())]
    );
    assert_eq!(read(&manifest(&rig)), "edited manifest\n");
    assert_eq!(read(&post_update(&rig)), asset("hooks/post-update"));
    let backup = result["files"][0]["backup_path"].as_str().unwrap();
    assert_eq!(read(Path::new(backup)), "edited hook\n");
    assert!(err(&o).contains("kept (changed)"), "{}", err(&o));
    // With no selection nothing is replaced and no journal is made.
    let o = replace(&rig, &plan(&rig), &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let result = json_of(&o);
    assert!(result["repair_id"].is_null());
    assert!(result["files"].as_array().unwrap().is_empty());
    assert_eq!(read(&manifest(&rig)), "edited manifest\n");
}

#[test]
fn omarchy_repair_a_changed_premise_gives_plan_changed_and_writes_nothing() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    put(&manifest(&rig), "mine manifest\n");
    let facts = plan(&rig);
    let check = |o: &Output, rig: &Rig| {
        assert_eq!(o.status.code(), Some(4), "{}{}", out(o), err(o));
        let result = json_of(o);
        assert_eq!(result["code"], "plan_changed");
        assert!(result["repair_id"].is_null());
        assert!(!journal_dir(rig).exists());
        assert!(!rig.fd_dir().join("omarchy-install.json").exists());
        result
    };

    // The record changed (one appeared).
    put(
        &rig.fd_dir().join("omarchy-install.json"),
        &json!({
            "v": 2, "fleetdeck_version": "x", "files": [], "plugin_enabled": false,
            "fleet_summary_set_by_install": false, "menu_file_written": false,
            "screensaver": {"enabled": false, "had_original": false,
                            "backup_sha256": null, "last_written_sha256": null}
        })
        .to_string(),
    );
    let o = replace(&rig, &facts, &[]);
    assert_eq!(o.status.code(), Some(4));
    assert_eq!(json_of(&o)["code"], "plan_changed");
    std::fs::remove_file(rig.fd_dir().join("omarchy-install.json")).unwrap();

    // The file changed after the plan.
    std::fs::write(manifest(&rig), "changed after the plan\n").unwrap();
    let o = replace(&rig, &facts, &[]);
    let result = check(&o, &rig);
    assert_eq!(read(&manifest(&rig)), "changed after the plan\n");
    // The result lists the files of the reviewed plan, unchanged.
    assert_eq!(
        ids_of(&result),
        vec![("fleet_plugin_manifest".to_owned(), "unchanged".to_owned())]
    );
    assert!(result["files"][0]["backup_path"].is_null());
    std::fs::write(manifest(&rig), "mine manifest\n").unwrap();

    // A file was added after the plan.
    put(&post_boot(&rig), "added\n");
    let o = replace(&rig, &facts, &[]);
    check(&o, &rig);
    std::fs::remove_file(post_boot(&rig)).unwrap();

    // A selected recorded edit that the plan does not have.
    let o = replace(&rig, &facts, &["hook_theme_set"]);
    check(&o, &rig);
    assert_eq!(read(&manifest(&rig)), "mine manifest\n");
}

#[test]
fn omarchy_repair_a_link_or_a_hard_link_in_the_set_gives_unsafe_path() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    let victim = outside.path().join("victim");
    std::fs::write(&victim, "victim\n").unwrap();
    std::fs::create_dir_all(manifest(&rig).parent().unwrap()).unwrap();
    symlink(&victim, manifest(&rig)).unwrap();
    put(&post_boot(&rig), "mine hook\n");
    std::fs::hard_link(post_boot(&rig), rig.root().join("other-name")).unwrap();
    let facts = plan(&rig);
    let o = replace(&rig, &facts, &[]);
    assert_eq!(o.status.code(), Some(4), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert_eq!(result["code"], "unsafe_path");
    assert!(result["repair_id"].is_null());
    assert_eq!(read(&victim), "victim\n");
    assert!(
        std::fs::symlink_metadata(manifest(&rig))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(read(&post_boot(&rig)), "mine hook\n");
    assert!(!journal_dir(&rig).exists());
}

// ---- repair-status, restore ----

#[test]
fn omarchy_repair_status_reports_the_files_not_the_journal_alone() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    let s = status(&rig);
    let journals = s["journals"].as_array().unwrap();
    assert_eq!(journals.len(), 1);
    assert_eq!(journals[0]["repair_id"], id.as_str());
    assert_eq!(journals[0]["state"], "complete");
    assert!(journals[0]["created_at"].as_i64().unwrap() > 0);
    assert_eq!(
        ids_of(&journals[0]),
        vec![
            ("fleet_plugin_manifest".to_owned(), "published".to_owned()),
            ("hook_post_boot".to_owned(), "published".to_owned())
        ]
    );
    assert!(journals[0]["files"][0]["backup_path"].is_string());
    // The target is gone: the journal says complete, the files say backed up.
    std::fs::remove_file(manifest(&rig)).unwrap();
    let s = status(&rig);
    assert_eq!(s["journals"][0]["state"], "incomplete");
    assert_eq!(s["journals"][0]["files"][0]["state"], "backed_up");
    // A later edit is a conflict.
    put(&post_boot(&rig), "later edit\n");
    assert_eq!(status(&rig)["journals"][0]["files"][1]["state"], "conflict");
}

#[test]
fn omarchy_repair_status_lists_the_last_8_newest_first_and_skips_bad_journals() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    let original: Value =
        serde_json::from_str(&read(&journal_dir(&rig).join(format!("{id}.json")))).unwrap();
    for n in 0..10_u32 {
        let mut copy = original.clone();
        let new_id = format!("{n:032x}");
        copy["repair_id"] = json!(new_id);
        copy["created_at"] = json!(1_000 + i64::from(n));
        put(
            &journal_dir(&rig).join(format!("{new_id}.json")),
            &copy.to_string(),
        );
    }
    put(&journal_dir(&rig).join("notes.txt"), "x");
    let bad = format!("{:032x}", 0xbad_u32);
    put(&journal_dir(&rig).join(format!("{bad}.json")), "not json");
    let o = rig.run(&["omarchy", "repair-status", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let journals = json_of(&o)["journals"].as_array().unwrap().clone();
    assert_eq!(journals.len(), 8);
    // The real journal is the newest (a real time); then the copies, by
    // their time, newest first.
    assert_eq!(journals[0]["repair_id"], id.as_str());
    let times: Vec<i64> = journals
        .iter()
        .map(|j| j["created_at"].as_i64().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] > w[1]), "{times:?}");
    assert_eq!(journals[1]["repair_id"], format!("{:032x}", 9));
    assert!(err(&o).contains(&bad), "{}", err(&o));
    assert!(!err(&o).contains("notes.txt"), "{}", err(&o));
    assert_eq!(err(&o).matches("warning").count(), 1, "{}", err(&o));
}

#[test]
fn omarchy_repair_restore_brings_back_a_kept_copy_and_keeps_every_backup() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    let facts = restore_plan(&rig, &id);
    assert_eq!(facts["repair_id"], id.as_str());
    let files = facts["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    for f in files {
        assert_eq!(f["action"], "move_aside_then_copy_backup");
        assert_eq!(f["target_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(f["backup_sha256"].as_str().unwrap().len(), 64);
        assert!(Path::new(f["backup_path"].as_str().unwrap()).is_file());
    }
    let backup = files[0]["backup_path"].as_str().unwrap().to_owned();
    let o = restore(&rig, &facts, &["fleet_plugin_manifest"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert!(result["code"].is_null());
    assert_eq!(result["repair_id"], id.as_str());
    assert_eq!(
        ids_of(&result),
        vec![("fleet_plugin_manifest".to_owned(), "restored".to_owned())]
    );
    assert_eq!(read(&manifest(&rig)), "mine manifest\n");
    assert_eq!(
        read(Path::new(&backup)),
        "mine manifest\n",
        "the backup stays"
    );
    // The asset that was published is kept under a new name.
    let kept: Vec<_> = std::fs::read_dir(manifest(&rig).parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".fleetdeck-kept-"))
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(read(&kept[0].path()), asset("plugin/manifest.json"));
    // The file that was not selected is untouched.
    assert_eq!(read(&post_boot(&rig)), asset("hooks/post-boot"));
    // The status: one restored, one published.
    let s = status(&rig);
    assert_eq!(s["journals"][0]["state"], "incomplete");
    assert_eq!(s["journals"][0]["files"][0]["state"], "restored");
    // A second restore of the same file does nothing.
    let facts = restore_plan(&rig, &id);
    assert_eq!(facts["files"][0]["action"], "nothing");
    let o = restore(&rig, &facts, &["fleet_plugin_manifest"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        ids_of(&json_of(&o)),
        vec![("fleet_plugin_manifest".to_owned(), "restored".to_owned())]
    );
    // Restoring both: every file is back and the journal is restored.
    let facts = restore_plan(&rig, &id);
    let o = restore(&rig, &facts, &["fleet_plugin_manifest", "hook_post_boot"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(read(&post_boot(&rig)), "mine hook\n");
    assert_eq!(status(&rig)["journals"][0]["state"], "restored");
}

#[test]
fn omarchy_repair_restore_does_not_overwrite_a_later_owner_edit() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    put(&post_boot(&rig), "later owner edit\n");
    let facts = restore_plan(&rig, &id);
    assert_eq!(facts["files"][1]["action"], "stop_restore_conflict");
    let o = restore(&rig, &facts, &["hook_post_boot", "fleet_plugin_manifest"]);
    assert_eq!(o.status.code(), Some(4), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert_eq!(result["code"], "restore_conflict");
    assert_eq!(read(&post_boot(&rig)), "later owner edit\n");
    // The other file is restored: a stopped file stops only itself.
    assert_eq!(read(&manifest(&rig)), "mine manifest\n");
    let states = ids_of(&result);
    assert!(states.contains(&("hook_post_boot".to_owned(), "unchanged".to_owned())));
    assert!(states.contains(&("fleet_plugin_manifest".to_owned(), "restored".to_owned())));
    let backup = facts["files"][1]["backup_path"].as_str().unwrap();
    assert_eq!(read(Path::new(backup)), "mine hook\n");
}

#[test]
fn omarchy_repair_restore_with_changed_facts_gives_plan_changed() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    let facts = restore_plan(&rig, &id);
    put(&manifest(&rig), "changed after the plan\n");
    let o = restore(&rig, &facts, &["fleet_plugin_manifest"]);
    assert_eq!(o.status.code(), Some(4), "{}{}", out(&o), err(&o));
    assert_eq!(json_of(&o)["code"], "plan_changed");
    assert_eq!(read(&manifest(&rig)), "changed after the plan\n");
    // An asset that the plan does not have.
    let facts = restore_plan(&rig, &id);
    let o = restore(&rig, &facts, &["hook_theme_set"]);
    assert_eq!(o.status.code(), Some(4));
    assert_eq!(json_of(&o)["code"], "plan_changed");
}

// ---- resume ----

#[test]
fn omarchy_repair_resume_finishes_an_incomplete_repair_twice() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    // The manifest was backed up and not published.
    std::fs::remove_file(manifest(&rig)).unwrap();
    let o = run_with(&rig, &["omarchy", "install", "--resume", id.as_str()], "");
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert!(result["code"].is_null());
    assert_eq!(result["repair_id"], id.as_str());
    assert_eq!(
        ids_of(&result),
        vec![
            ("fleet_plugin_manifest".to_owned(), "replaced".to_owned()),
            ("hook_post_boot".to_owned(), "replaced".to_owned())
        ]
    );
    assert_eq!(read(&manifest(&rig)), asset("plugin/manifest.json"));
    assert_eq!(rig.run(&["omarchy", "status"]).status.code(), Some(0));
    // The same again: nothing changes.
    let o = run_with(&rig, &["omarchy", "install", "--resume", id.as_str()], "");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(read(&manifest(&rig)), asset("plugin/manifest.json"));
    assert_eq!(status(&rig)["journals"][0]["state"], "complete");
}

#[test]
fn omarchy_repair_resume_stops_at_a_later_edit_and_goes_on_with_the_other_file() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    std::fs::remove_file(manifest(&rig)).unwrap();
    put(&post_boot(&rig), "later owner edit\n");
    let o = run_with(&rig, &["omarchy", "install", "--resume", id.as_str()], "");
    assert_eq!(o.status.code(), Some(4), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert_eq!(result["code"], "restore_conflict");
    assert_eq!(read(&post_boot(&rig)), "later owner edit\n");
    assert_eq!(read(&manifest(&rig)), asset("plugin/manifest.json"));
}

#[test]
fn omarchy_repair_resume_with_another_build_hash_gives_build_mismatch() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    let path = journal_dir(&rig).join(format!("{id}.json"));
    let mut journal: Value = serde_json::from_str(&read(&path)).unwrap();
    journal["build_hash"] = json!("0".repeat(64));
    std::fs::write(&path, journal.to_string()).unwrap();
    std::fs::remove_file(manifest(&rig)).unwrap();
    let o = run_with(&rig, &["omarchy", "install", "--resume", id.as_str()], "");
    assert_eq!(o.status.code(), Some(4), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert_eq!(result["code"], "build_mismatch");
    assert_eq!(result["repair_id"], id.as_str());
    assert!(
        ids_of(&result)
            .iter()
            .all(|(_, state)| state == "unchanged")
    );
    assert!(!manifest(&rig).exists(), "nothing was changed");
    // A restore still works.
    let facts = restore_plan(&rig, &id);
    assert_eq!(facts["files"][0]["action"], "copy_backup");
    let o = restore(&rig, &facts, &["fleet_plugin_manifest"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(read(&manifest(&rig)), "mine manifest\n");
}

#[test]
fn omarchy_repair_a_bad_id_or_a_missing_journal_exits_1() {
    let rig = Rig::new();
    let bad_ids = [
        "short".to_owned(),
        "../../etc/passwd".to_owned(),
        "A".repeat(32),
        "g".repeat(32),
    ];
    for bad in bad_ids.iter().map(String::as_str) {
        let o = run_with(&rig, &["omarchy", "install", "--resume", bad], "");
        assert_eq!(o.status.code(), Some(1), "{bad}");
        assert!(
            err(&o).contains("the repair ID must be 32 lowercase hexadecimal characters."),
            "{}",
            err(&o)
        );
        assert_eq!(out(&o), "");
        let o = rig.run(&["omarchy", "restore-plan", "--repair", bad, "--json"]);
        assert_eq!(o.status.code(), Some(1), "{bad}");
    }
    let id = "a".repeat(32);
    let o = run_with(&rig, &["omarchy", "install", "--resume", id.as_str()], "");
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no repair journal"), "{}", err(&o));
    assert_eq!(out(&o), "");
}

// ---- arguments and stdin ----

#[test]
fn omarchy_repair_the_flags_require_and_exclude_each_other_with_exit_2() {
    let rig = Rig::new();
    let id = "a".repeat(32);
    let cases = [
        "install --replace-conflicts",
        "install --plan-stdin",
        "install --resume ID --plan-stdin",
        "install --resume ID --replace-conflicts --plan-stdin",
        "install --resume ID --screensaver",
        "install --screensaver --replace-conflicts --plan-stdin",
        "restore",
    ];
    for case in cases {
        let line = case.replace("ID", &id);
        let mut args = vec!["omarchy"];
        args.extend(line.split(' '));
        let o = run_with(&rig, &args, "");
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
}

#[test]
fn omarchy_repair_a_bad_stdin_exits_1_and_changes_nothing() {
    let rig = Rig::new();
    put(&manifest(&rig), "mine manifest\n");
    for (name, args) in [
        (
            "install",
            vec![
                "omarchy",
                "install",
                "--yes",
                "--replace-conflicts",
                "--plan-stdin",
            ],
        ),
        ("restore", vec!["omarchy", "restore", "--plan-stdin"]),
    ] {
        let o = run_with(&rig, &args, "not json");
        assert_eq!(o.status.code(), Some(1), "{name}: {}", err(&o));
        assert_eq!(out(&o), "");
        let o = run_with(&rig, &args, &"x".repeat(64 * 1024 + 1));
        assert_eq!(o.status.code(), Some(1), "{name}");
        assert!(
            err(&o).contains("larger than 64 KiB"),
            "{name}: {}",
            err(&o)
        );
        let o = run_with(&rig, &args, r#"{"facts": {}, "extra": 1}"#);
        assert_eq!(o.status.code(), Some(1), "{name}");
    }
    assert_eq!(read(&manifest(&rig)), "mine manifest\n");
    assert!(!journal_dir(&rig).exists());
}

// ---- the lock ----

#[test]
fn omarchy_repair_every_command_with_the_lock_held_waits_then_exits_3() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    let _lock = hold_lock(&rig);
    let id = "a".repeat(32);
    let replace_input = json!({
        "facts": {"build_hash": "0", "record_sha256": null, "files": []},
        "replace_recorded_edits": []
    })
    .to_string();
    let restore_input = json!({
        "facts": {"repair_id": id, "files": []},
        "asset_ids": []
    })
    .to_string();
    let calls: Vec<(Vec<&str>, &str)> = vec![
        (vec!["omarchy", "install-plan", "--json"], ""),
        (vec!["omarchy", "repair-status", "--json"], ""),
        (
            vec!["omarchy", "restore-plan", "--repair", id.as_str(), "--json"],
            "",
        ),
        (
            vec![
                "omarchy",
                "install",
                "--yes",
                "--replace-conflicts",
                "--plan-stdin",
            ],
            replace_input.as_str(),
        ),
        (vec!["omarchy", "install", "--resume", id.as_str()], ""),
        (
            vec!["omarchy", "restore", "--plan-stdin"],
            restore_input.as_str(),
        ),
    ];
    let started = Instant::now();
    // The six commands wait for the lock at the same time.
    std::thread::scope(|scope| {
        let rig = &rig;
        let handles: Vec<_> = calls
            .iter()
            .map(|(args, stdin)| scope.spawn(move || (args, run_with(rig, args, stdin))))
            .collect();
        for handle in handles {
            let (args, o) = handle.join().unwrap();
            assert_eq!(o.status.code(), Some(3), "{args:?}: {}", err(&o));
            assert_eq!(out(&o), "", "{args:?}");
            assert!(err(&o).contains(BUSY), "{args:?}: {}", err(&o));
        }
    });
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "{:?}",
        started.elapsed()
    );
}
