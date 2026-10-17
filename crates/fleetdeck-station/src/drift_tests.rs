use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use fleetdeck_proto::fleet_git::DriftItem;

use super::*;

fn tree(root: &Path, files: &[(&str, &str)]) {
    for (rel, body) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
}

fn is_hash(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn equal_trees_give_equal_hashes_in_any_place() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let files = [("x/one.md", "1"), ("two.md", "2"), ("x/y/three.md", "3")];
    tree(&a.path().join("skills"), &files);
    tree(&b.path().join("deep/er/skills"), &files);
    let ha = hash_item(&a.path().join("skills"));
    let hb = hash_item(&b.path().join("deep/er/skills"));
    assert!(is_hash(&ha), "{ha}");
    assert_eq!(ha, hb, "the hash holds no absolute path");
}

#[test]
fn a_changed_byte_a_rename_a_new_empty_folder_and_a_link_each_change_the_hash() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("skills");
    tree(&root, &[("a.md", "alpha"), ("b/c.md", "gamma")]);
    let base = hash_item(&root);

    fs::write(root.join("a.md"), "alphb").unwrap();
    let byte = hash_item(&root);
    assert_ne!(byte, base);
    fs::write(root.join("a.md"), "alpha").unwrap();
    assert_eq!(hash_item(&root), base);

    fs::rename(root.join("a.md"), root.join("z.md")).unwrap();
    let renamed = hash_item(&root);
    assert_ne!(renamed, base);
    fs::rename(root.join("z.md"), root.join("a.md")).unwrap();

    fs::create_dir(root.join("empty")).unwrap();
    let folder = hash_item(&root);
    assert_ne!(folder, base);
    fs::remove_dir(root.join("empty")).unwrap();

    symlink("a.md", root.join("link")).unwrap();
    let link = hash_item(&root);
    assert_ne!(link, base);
    // The target of the link counts too.
    fs::remove_file(root.join("link")).unwrap();
    symlink("b/c.md", root.join("link")).unwrap();
    assert_ne!(hash_item(&root), link);
    assert!(is_hash(&link));
}

#[test]
fn a_file_is_hashed_with_its_size() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("CLAUDE.md");
    fs::write(&file, "rules").unwrap();
    let first = hash_item(&file);
    assert!(is_hash(&first));
    fs::write(&file, "rulez").unwrap();
    assert_ne!(hash_item(&file), first);
    fs::write(&file, "rules").unwrap();
    assert_eq!(hash_item(&file), first);
}

#[test]
fn absent_unreadable_and_too_large() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(hash_item(&dir.path().join("none")), "absent");
    // A dangling link at the top is absent too.
    symlink(dir.path().join("none"), dir.path().join("dangling")).unwrap();
    assert_eq!(hash_item(&dir.path().join("dangling")), "absent");
    // A file over 4 MiB.
    let big = dir.path().join("big");
    fs::write(&big, vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
    assert_eq!(hash_item(&big), "too_large");
    // A folder with a file over 4 MiB.
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("big"), vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
    assert_eq!(hash_item(&folder), "too_large");
    // More than 10000 entries.
    let many = dir.path().join("many");
    fs::create_dir(&many).unwrap();
    for i in 0..=MAX_ENTRIES {
        fs::write(many.join(format!("f{i}")), "").unwrap();
    }
    assert_eq!(hash_item(&many), "too_large");
    // A socket at the top is neither a file nor a folder.
    let socket = dir.path().join("sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert_eq!(hash_item(&socket), "unreadable");
}

#[test]
fn a_folder_that_cannot_be_read_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("agents");
    tree(&folder, &[("sub/a.md", "a")]);
    fs::set_permissions(folder.join("sub"), fs::Permissions::from_mode(0o000)).unwrap();
    let got = hash_item(&folder);
    fs::set_permissions(folder.join("sub"), fs::Permissions::from_mode(0o755)).unwrap();
    // A process that ignores the mode (root) reads it: the test needs a mode check.
    if fs::read_dir(folder.join("sub")).is_ok() && got != "unreadable" {
        assert!(is_hash(&got));
    } else {
        assert_eq!(got, "unreadable");
    }
}

#[test]
fn a_top_level_link_is_followed_once_and_none_inside() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    tree(&real, &[("a.md", "a")]);
    symlink(&real, dir.path().join("skills")).unwrap();
    assert_eq!(hash_item(&dir.path().join("skills")), hash_item(&real));
    // A link inside is hashed as a link record, not followed.
    let outside = dir.path().join("outside");
    tree(&outside, &[("big.md", "one")]);
    symlink(&outside, real.join("l")).unwrap();
    let before = hash_item(&real);
    fs::write(outside.join("big.md"), "two").unwrap();
    assert_eq!(hash_item(&real), before);
}

#[test]
fn compute_has_the_four_items_in_order() {
    let home = tempfile::tempdir().unwrap();
    tree(
        home.path(),
        &[
            (".claude/CLAUDE.md", "rules"),
            (".claude/skills/s/SKILL.md", "skill"),
        ],
    );
    let digest = compute(Some(home.path()), Some("2.1.0".into()), None, 42);
    let names: Vec<DriftItem> = digest.items.iter().map(|i| i.name).collect();
    assert_eq!(
        names,
        [
            DriftItem::ClaudeMd,
            DriftItem::ClaudeAgents,
            DriftItem::ClaudeSkills,
            DriftItem::CodexAgentsMd
        ]
    );
    let hashes: Vec<&str> = digest.items.iter().map(|i| i.hash.as_str()).collect();
    assert!(is_hash(hashes[0]) && is_hash(hashes[2]));
    assert_eq!((hashes[1], hashes[3]), ("absent", "absent"));
    assert_eq!(digest.claude_version.as_deref(), Some("2.1.0"));
    assert_eq!(digest.codex_version, None);
    assert_eq!(digest.computed_at, 42);
    // Without a home every item is absent.
    let none = compute(None, None, None, 1);
    assert!(none.items.iter().all(|i| i.hash == "absent"));
    assert_eq!(none.items.len(), 4);
}

#[test]
fn two_homes_with_equal_files_have_equal_digests_and_a_new_file_changes_one_item() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for home in [a.path(), b.path()] {
        tree(
            home,
            &[(".claude/CLAUDE.md", "x"), (".claude/skills/s/a", "1")],
        );
    }
    let da = compute(Some(a.path()), None, None, 1);
    let db = compute(Some(b.path()), None, None, 2);
    assert_eq!(da.items, db.items);
    tree(b.path(), &[(".claude/skills/new.md", "n")]);
    let db = compute(Some(b.path()), None, None, 3);
    let differing: Vec<DriftItem> = da
        .items
        .iter()
        .zip(&db.items)
        .filter(|(x, y)| x != y)
        .map(|(x, _)| x.name)
        .collect();
    assert_eq!(differing, [DriftItem::ClaudeSkills]);
}
