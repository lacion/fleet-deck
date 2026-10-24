//! PRE-PR round 1 of BG-2, at the level of the commands: an unsafe
//! permission refuses the plan and the replacement, a kept copy ends in
//! `.sample`, a later edit that a restore meets is reported, and a repair
//! whose install did not finish stays incomplete until a resume finishes it.

use super::*;

fn chmod(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

fn journal_file(rig: &Rig, id: &str) -> PathBuf {
    journal_dir(rig).join(format!("{id}.json"))
}

fn journal_json(rig: &Rig, id: &str) -> Value {
    serde_json::from_str(&read(&journal_file(rig, id))).unwrap()
}

fn save_journal(rig: &Rig, id: &str, value: &Value) {
    std::fs::write(journal_file(rig, id), serde_json::to_vec(value).unwrap()).unwrap();
}

fn resume(rig: &Rig, id: &str) -> Output {
    run_with(rig, &["omarchy", "install", "--resume", id], "")
}

#[test]
fn omarchy_repair_a_group_writable_file_or_directory_is_not_replaceable() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    put(&manifest(&rig), "mine manifest\n");
    let targets: [(&str, PathBuf); 4] = [
        ("the file", manifest(&rig)),
        ("its folder", manifest(&rig).parent().unwrap().to_path_buf()),
        ("a folder above", rig.omarchy_dir().join("plugins")),
        ("the base folder", rig.omarchy_dir()),
    ];
    for (label, path) in targets {
        let before = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        chmod(&path, before | 0o020);
        let facts = plan(&rig);
        let f = &facts["files"][0];
        assert_eq!(f["replaceable"], false, "{label}");
        assert_eq!(f["blocked"], "unsafe_path", "{label}");
        let o = replace(&rig, &facts, &[]);
        assert_eq!(o.status.code(), Some(4), "{label}: {}{}", out(&o), err(&o));
        let result = json_of(&o);
        assert_eq!(result["code"], "unsafe_path", "{label}");
        assert!(result["repair_id"].is_null(), "{label}");
        assert_eq!(read(&manifest(&rig)), "mine manifest\n", "{label}");
        assert!(!journal_dir(&rig).exists(), "{label}");
        chmod(&path, before);
    }
    // Safe again: the same file is replaceable.
    assert_eq!(plan(&rig)["files"][0]["replaceable"], true);
}

#[test]
fn omarchy_repair_the_kept_copies_end_in_sample_so_no_hook_dispatcher_runs_them() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    let facts = restore_plan(&rig, &id);
    let o = restore(&rig, &facts, &["fleet_plugin_manifest", "hook_post_boot"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let hooks = post_boot(&rig).parent().unwrap().to_path_buf();
    let mut names: Vec<String> = std::fs::read_dir(&hooks)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    // The dispatcher runs every file but `*.sample`: only the hook itself.
    let runs: Vec<&String> = names.iter().filter(|n| !n.ends_with(".sample")).collect();
    assert_eq!(runs, vec!["fleetdeck"], "{names:?}");
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("fleetdeck.fleetdeck-conflict-"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("fleetdeck.fleetdeck-kept-"))
    );
    assert_eq!(read(&post_boot(&rig)), "mine hook\n");
}

#[test]
fn omarchy_repair_restore_reports_a_later_edit_that_is_kept_aside() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    // The state after a move aside that met a later edit that could not go
    // back: the edit under the kept name, the journal knows the name.
    let dir = manifest(&rig).parent().unwrap().to_path_buf();
    let name = "manifest.json.fleetdeck-kept-1-0123456789abcdef.sample";
    std::fs::rename(manifest(&rig), dir.join(name)).unwrap();
    std::fs::write(dir.join(name), "later edit\n").unwrap();
    let mut journal = journal_json(&rig, &id);
    journal["files"][0]["kept_name"] = name.into();
    save_journal(&rig, &id, &journal);
    let facts = restore_plan(&rig, &id);
    assert_eq!(facts["files"][0]["action"], "copy_backup");
    let o = restore(&rig, &facts, &["fleet_plugin_manifest", "hook_post_boot"]);
    assert_eq!(o.status.code(), Some(4), "{}{}", out(&o), err(&o));
    let result = json_of(&o);
    assert_eq!(result["code"], "restore_conflict");
    let file = &result["files"][0];
    assert_eq!(file["asset_id"], "fleet_plugin_manifest");
    assert_eq!(
        file["kept_path"].as_str().unwrap(),
        dir.join(name).display().to_string()
    );
    // The backup was not published; the edit is still there; the other file
    // was restored; a file with no edit has no `kept_path` key.
    assert!(!manifest(&rig).exists());
    assert_eq!(read(&dir.join(name)), "later edit\n");
    assert_eq!(read(&post_boot(&rig)), "mine hook\n");
    assert!(result["files"][1].get("kept_path").is_none());
}

#[test]
fn omarchy_repair_a_journal_that_is_not_finished_stays_incomplete_until_a_resume() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    // The command finished: the journal says so, and the status is complete.
    assert_eq!(journal_json(&rig, &id)["finished"], true);
    assert_eq!(status(&rig)["journals"][0]["state"], "complete");
    // A crash after the publication and before the finish.
    let mut journal = journal_json(&rig, &id);
    journal["finished"] = false.into();
    save_journal(&rig, &id, &journal);
    let s = status(&rig);
    assert_eq!(s["journals"][0]["state"], "incomplete");
    assert_eq!(
        ids_of(&s["journals"][0]),
        vec![
            ("fleet_plugin_manifest".to_owned(), "published".to_owned()),
            ("hook_post_boot".to_owned(), "published".to_owned())
        ]
    );
    // The resume runs the rest of the install and finishes the journal.
    let o = resume(&rig, &id);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(json_of(&o)["code"].is_null());
    assert!(err(&o).contains("Fleet Deck is installed for Omarchy."));
    assert_eq!(journal_json(&rig, &id)["finished"], true);
    assert_eq!(status(&rig)["journals"][0]["state"], "complete");
    // A second resume changes nothing.
    let o = resume(&rig, &id);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(status(&rig)["journals"][0]["state"], "complete");
}

#[test]
fn omarchy_repair_a_journal_without_finished_or_of_version_1_is_not_read() {
    let mut rig = Rig::new();
    let id = replaced_two(&mut rig);
    assert_eq!(journal_json(&rig, &id)["v"], 2);
    let journal = journal_json(&rig, &id);
    // No `finished` field: the journal is skipped, not read as complete.
    let mut without = journal.clone();
    without.as_object_mut().unwrap().remove("finished");
    save_journal(&rig, &id, &without);
    assert!(status(&rig)["journals"].as_array().unwrap().is_empty());
    // Version 1: the same.
    let mut old = journal.clone();
    old["v"] = 1.into();
    save_journal(&rig, &id, &old);
    assert!(status(&rig)["journals"].as_array().unwrap().is_empty());
    // The journal as it was written is read.
    save_journal(&rig, &id, &journal);
    assert_eq!(status(&rig)["journals"][0]["state"], "complete");
}
