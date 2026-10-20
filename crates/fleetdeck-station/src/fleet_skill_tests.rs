//! Tests 12 and 12a of `docs/v2/design/mcp-p3.md` §6: the publication of the
//! fleet skill tree, and the conditions of the tools that `SKILL.md` names.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use super::*;

fn station() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn trees(dir: &Path) -> PathBuf {
    dir.join(SHARE).join(TREES)
}

fn bad_copies(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(trees(dir))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.contains(BAD_INFIX))
        .collect();
    out.sort();
    out
}

fn tmp_dirs(dir: &Path) -> Vec<String> {
    std::fs::read_dir(trees(dir))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(TEMP_PREFIX))
        .collect()
}

/// A published tree equals the embedded content, with the design modes.
fn assert_valid(tree: &Path) {
    for (path, content) in FILES {
        let file = tree.join(path);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), content, "{path}");
        let meta = std::fs::symlink_metadata(&file).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o644, "{path}");
    }
    for dir in std::iter::once("").chain(DIRS) {
        let mode = std::fs::metadata(tree.join(dir))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "{dir}");
    }
    let mut top: Vec<String> = std::fs::read_dir(tree)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    top.sort();
    assert_eq!(top, [".claude-plugin", "skills"]);
}

#[test]
fn the_fleet_skill_is_published_content_addressed_and_verified() {
    let dir = station();
    let tree = publish(dir.path()).expect("published");
    assert!(tree.is_absolute());
    assert_eq!(tree.file_name().unwrap().to_str().unwrap(), tree_hash());
    assert_eq!(
        tree,
        std::path::absolute(trees(dir.path()))
            .unwrap()
            .join(tree_hash())
    );
    assert_valid(&tree);
    // The share folders are private, the lock file exists.
    for private in [dir.path().join(SHARE), trees(dir.path())] {
        assert_eq!(
            std::fs::metadata(private).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(trees(dir.path()).join(LOCK).is_file());
    // A second publication keeps the same tree (the same inode).
    let inode = std::fs::metadata(&tree).unwrap().ino();
    assert_eq!(publish(dir.path()).unwrap(), tree);
    assert_eq!(std::fs::metadata(&tree).unwrap().ino(), inode);
    assert!(bad_copies(dir.path()).is_empty());
    // The plugin manifest is the design's manifest.
    let manifest: serde_json::Value = serde_json::from_str(PLUGIN_JSON).unwrap();
    assert_eq!(manifest["name"], "fleetdeck");
    assert_eq!(manifest["version"], "2");
    assert!(SKILL_MD.starts_with("---\nname: fleet\n"));
}

#[test]
fn a_link_at_the_station_directory_share_or_fleet_skill_means_no_injection() {
    // The station directory itself.
    let real = station();
    let holder = station();
    let link = holder.path().join("station");
    symlink(real.path(), &link).unwrap();
    assert_eq!(publish(&link), None);
    assert!(!real.path().join(SHARE).exists());
    // `share`.
    let dir = station();
    let elsewhere = station();
    symlink(elsewhere.path(), dir.path().join(SHARE)).unwrap();
    assert_eq!(publish(dir.path()), None);
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    // `share/fleet-skill`.
    let dir = station();
    std::fs::create_dir(dir.path().join(SHARE)).unwrap();
    std::fs::set_permissions(
        dir.path().join(SHARE),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let elsewhere = station();
    symlink(elsewhere.path(), trees(dir.path())).unwrap();
    assert_eq!(publish(dir.path()), None);
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    // A group-writable `share` and a group-writable station directory.
    let dir = station();
    std::fs::create_dir(dir.path().join(SHARE)).unwrap();
    std::fs::set_permissions(
        dir.path().join(SHARE),
        std::fs::Permissions::from_mode(0o770),
    )
    .unwrap();
    assert_eq!(publish(dir.path()), None);
    let dir = station();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
    assert_eq!(publish(dir.path()), None);
    // A lock file that is a link is refused.
    let dir = station();
    assert!(publish(dir.path()).is_some());
    std::fs::remove_file(trees(dir.path()).join(LOCK)).unwrap();
    let outside = station();
    symlink(outside.path().join("lock"), trees(dir.path()).join(LOCK)).unwrap();
    assert_eq!(publish(dir.path()), None);
    assert!(!outside.path().join("lock").exists());
}

#[test]
fn a_linked_tree_or_a_linked_file_is_renamed_and_published_again() {
    let dir = station();
    let tree = publish(dir.path()).unwrap();
    // The tree directory becomes a link to a valid copy elsewhere.
    let copy_holder = station();
    let copy = copy_holder.path().join("copy");
    let again = station();
    let valid = publish(again.path()).unwrap();
    std::process::Command::new("cp")
        .arg("-r")
        .arg(&valid)
        .arg(&copy)
        .status()
        .unwrap();
    std::fs::rename(&tree, trees(dir.path()).join("moved")).unwrap();
    symlink(&copy, &tree).unwrap();
    let published = publish(dir.path()).unwrap();
    assert_eq!(published, tree);
    assert_valid(&tree);
    assert!(
        !std::fs::symlink_metadata(&tree)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(bad_copies(dir.path()).len(), 1);
    // The target of the old link is not touched.
    assert_valid(&copy);

    // A file becomes a link to a file outside.
    let outside = station();
    let target = outside.path().join("SKILL.md");
    std::fs::write(&target, SKILL_MD).unwrap();
    let file = tree.join("skills/fleet/SKILL.md");
    std::fs::remove_file(&file).unwrap();
    symlink(&target, &file).unwrap();
    assert_eq!(publish(dir.path()).unwrap(), tree);
    assert_valid(&tree);
    assert_eq!(bad_copies(dir.path()).len(), 2);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), SKILL_MD);

    // A directory inside becomes a link.
    let inner = tree.join("skills");
    std::fs::rename(&inner, tree.join("skills-old")).unwrap();
    symlink(copy.join("skills"), &inner).unwrap();
    assert_eq!(publish(dir.path()).unwrap(), tree);
    assert_valid(&tree);
}

#[test]
fn an_extra_hook_or_mcp_file_and_a_changed_byte_make_a_tree_bad() {
    let dir = station();
    let tree = publish(dir.path()).unwrap();
    // An extra `hooks/hooks.json`.
    std::fs::create_dir(tree.join("hooks")).unwrap();
    std::fs::write(tree.join("hooks/hooks.json"), "{}").unwrap();
    assert_eq!(publish(dir.path()).unwrap(), tree);
    assert_valid(&tree);
    assert_eq!(bad_copies(dir.path()).len(), 1);
    // An extra `.mcp.json`.
    std::fs::write(tree.join(".mcp.json"), "{}").unwrap();
    assert_eq!(publish(dir.path()).unwrap(), tree);
    assert_valid(&tree);
    assert_eq!(bad_copies(dir.path()).len(), 2);
    // A changed byte, a group-writable file and a truncated file.
    for change in 0..3 {
        let file = tree.join(".claude-plugin/plugin.json");
        match change {
            0 => std::fs::write(&file, PLUGIN_JSON.replace("fleetdeck", "fleetdecK")).unwrap(),
            1 => std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o664)).unwrap(),
            _ => std::fs::write(&file, "").unwrap(),
        }
        assert_eq!(publish(dir.path()).unwrap(), tree);
        assert_valid(&tree);
    }
    assert_eq!(bad_copies(dir.path()).len(), 5);
}

#[test]
fn a_busy_lock_a_stopped_publisher_and_a_leftover_temp_directory_keep_the_tree_valid() {
    let dir = station();
    let root = dir.path().to_path_buf();
    // Publisher A stops after its verification (the lock is held); publisher
    // B finds the lock busy and publishes nothing.
    let (at_verify, verified_rx) = mpsc::channel::<()>();
    let (go, go_rx) = mpsc::channel::<()>();
    let a_root = root.clone();
    let a = std::thread::spawn(move || {
        let pause = || {
            at_verify.send(()).unwrap();
            go_rx.recv().unwrap();
        };
        publish_with(
            &a_root,
            &Options {
                after_verify: Some(&pause),
                ..Options::default()
            },
        )
    });
    verified_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let busy = publish_with(
        &root,
        &Options {
            lock_wait: Some(Duration::from_millis(150)),
            ..Options::default()
        },
    );
    assert_eq!(busy, None);
    assert!(!trees(&root).join(tree_hash()).exists());
    go.send(()).unwrap();
    let tree = a.join().unwrap().expect("A published after the pause");
    assert_valid(&tree);

    // Publisher C is stopped before its rename (no `<hash>` exists, as
    // after a crash): only a `.tmp-*` directory stays.
    std::fs::remove_dir_all(&tree).unwrap();
    let stop = || true;
    let stopped = publish_with(
        &root,
        &Options {
            stop_before_rename: Some(&stop),
            ..Options::default()
        },
    );
    assert_eq!(stopped, None);
    assert!(!tree.exists());
    assert_eq!(tmp_dirs(&root).len(), 1);
    // The next publisher removes the leftover and publishes.
    assert_eq!(publish(&root).unwrap(), tree);
    assert_valid(&tree);
    assert!(tmp_dirs(&root).is_empty());

    // A published tree stays valid and present when a later publisher stops
    // before a rename that it never needs: the tree exists, so there is no
    // write at all.
    let stop = || true;
    assert_eq!(
        publish_with(
            &root,
            &Options {
                stop_before_rename: Some(&stop),
                ..Options::default()
            }
        )
        .unwrap(),
        tree
    );
    assert_valid(&tree);
    assert!(tmp_dirs(&root).is_empty());
}

#[test]
fn old_trees_stay_while_a_session_is_live_and_go_at_a_quiet_start() {
    let dir = station();
    let tree = publish(dir.path()).unwrap();
    let old = trees(dir.path()).join("a".repeat(64));
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("file"), "x").unwrap();
    let old_bad = trees(dir.path()).join(format!("{}{BAD_INFIX}1", "b".repeat(64)));
    std::fs::create_dir(&old_bad).unwrap();
    let unrelated = trees(dir.path()).join("keep-me");
    std::fs::create_dir(&unrelated).unwrap();
    // A live session: nothing is removed.
    assert_eq!(publish_at_start(dir.path(), 1).unwrap(), tree);
    assert!(old.exists() && old_bad.exists());
    // No live session: the old trees go, the current tree and the other
    // names stay.
    assert_eq!(publish_at_start(dir.path(), 0).unwrap(), tree);
    assert!(!old.exists() && !old_bad.exists());
    assert!(unrelated.exists());
    assert_valid(&tree);
}

/// The names in backticks of a line.
fn ticked(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut rest = line;
    let mut offset = 0;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        out.push((offset + start + 1, &after[..end]));
        offset += start + end + 2;
        rest = &rest[start + end + 2..];
    }
    out
}

#[test]
fn every_tool_named_in_the_fleet_skill_is_conditional() {
    use crate::coordinator::{ToolClass, tools};
    // The tools that every binding has: the own tools of the catalog.
    let own: std::collections::BTreeSet<ToolClass> = [ToolClass::Own].into_iter().collect();
    let catalog = tools::catalog(&own);
    let always: Vec<&str> = catalog["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for name in [
        "fleet_view",
        "ask_coordinator",
        "publish_artifact",
        "set_focus",
    ] {
        assert!(always.contains(&name), "{name} is not always in a catalog");
    }
    let mut checked = 0;
    for line in SKILL_MD.lines() {
        for (at, name) in ticked(line) {
            if tools::tool(name).is_none() || always.contains(&name) {
                continue;
            }
            checked += 1;
            // An earlier (or this) `if your tool list has `<name>``.
            let clause = format!("if your tool list has `{name}`");
            let lower = line.to_lowercase();
            let first = lower
                .find(&clause)
                .unwrap_or_else(|| panic!("`{name}` is named without a condition: {line}"));
            assert!(first < at, "`{name}` comes before its condition: {line}");
        }
    }
    // `start_in_project`, `spawn`, `assign`, `wait_tasks`, `send_mail`,
    // `read_artifact` and `stop` are all checked, at least once each.
    assert!(checked >= 7, "{checked}");
}
