//! The private paths of the station (DL-173 section 2.1): the set, its
//! comparison rules and the git excludes. The routes are tested in
//! `manager/files_private_tests.rs`, `session_diff_tests.rs` and
//! `artifacts/private_tests.rs`.

use std::path::{Path, PathBuf};

use fleetdeck_core::paths::NodeDirs;

use super::{PrivateEnv, PrivatePaths, canonical_of};

/// A text that every file of the layout holds, so that a search finds it.
pub(crate) const NEEDLE: &str = "needle-text";

/// The paths below HOME that are private in the default split layout.
pub(crate) const PRIVATE: &[&str] = &[
    ".config/fleetdeck/station.toml",
    ".config/fleetdeck/station.toml.tmp-x",
    ".local/share/fleetdeck/station",
    ".local/share/fleetdeck/station/station.db",
    ".local/share/fleetdeck/station/secrets",
    ".local/share/fleetdeck/station/secrets/jira_token",
    ".ssh/id_ed25519",
];

/// The private files among them (the ones that a test can read or change).
pub(crate) const PRIVATE_FILES: &[&str] = &[
    ".config/fleetdeck/station.toml",
    ".config/fleetdeck/station.toml.tmp-x",
    ".local/share/fleetdeck/station/station.db",
    ".local/share/fleetdeck/station/secrets/jira_token",
    ".ssh/id_ed25519",
    ".config/gh/hosts.yml",
    ".claude/.credentials.json",
];

/// The normal files next to them.
pub(crate) const NORMAL: &[&str] = &[
    "notes.txt",
    ".config/fleetdeck/readme.txt",
    ".local/share/fleetdeck/other.txt",
];

/// The default split layout below the canonical directory `home`: the
/// config directory and the data directory are different directories. The
/// private files and the normal files all hold [`NEEDLE`].
pub(crate) fn layout(home: &Path) -> NodeDirs {
    let dirs = NodeDirs {
        config: home.join(".config/fleetdeck"),
        data: home.join(".local/share/fleetdeck"),
    };
    for file in PRIVATE_FILES.iter().chain(NORMAL) {
        let path = home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{NEEDLE} {file}\n")).unwrap();
    }
    dirs
}

/// The set of a station that runs in `home` (no deck in the process).
pub(crate) fn private_of(home: &Path) -> PrivatePaths {
    PrivatePaths::new(&layout(home), Some(home), None, &PrivateEnv::default())
}

fn home() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    (tmp, home)
}

#[test]
fn the_exact_files_the_dirs_and_the_temp_files_are_private() {
    let (_t, home) = home();
    let p = private_of(&home);
    for path in PRIVATE {
        assert!(p.is_private(&home.join(path)), "{path}");
        assert!(p.is_private_canonical(&home.join(path)), "{path}");
    }
    // A file of a private directory that does not exist yet.
    assert!(p.is_private(&home.join(".local/share/fleetdeck/station/new/file")));
    // The temp file of an atomic write, also a new one.
    assert!(p.is_private(&home.join(".config/fleetdeck/station.toml.tmp-9f3")));
    for path in NORMAL {
        assert!(!p.is_private(&home.join(path)), "{path}");
    }
    // Names that only look alike are not private.
    for path in [
        ".config/fleetdeck/station.toml2",
        ".config/fleetdeck/xstation.toml.tmp-x",
        ".local/share/fleetdeck/station2/file",
        ".local/share/fleetdeck/stationary",
        ".config/fleetdeck",
        ".local/share/fleetdeck",
    ] {
        assert!(!p.is_private(&home.join(path)), "{path}");
    }
}

#[test]
fn the_home_credential_files_and_directories_are_private() {
    let (_t, home) = home();
    let p = private_of(&home);
    for path in [
        ".config/gh/hosts.yml",
        ".config/glab-cli/config.yml",
        ".ssh/id_rsa",
        ".claude/.credentials.json",
        ".codex/auth.json",
    ] {
        assert!(p.is_private(&home.join(path)), "{path}");
    }
    assert!(!p.is_private(&home.join(".claude/settings.json")));
    assert!(!p.is_private(&home.join(".codex/config.toml")));
}

#[test]
fn xdg_and_gh_config_dir_add_credential_directories() {
    let (_t, home) = home();
    let other = home.join("xdg");
    let gh = home.join("custom-gh");
    let p = PrivatePaths::new(
        &layout(&home),
        Some(&home),
        None,
        &PrivateEnv {
            xdg_config_home: Some(other.clone()),
            gh_config_dir: Some(gh.clone()),
        },
    );
    for path in [
        other.join("gh/hosts.yml"),
        other.join("glab-cli/config.yml"),
        gh.join("hosts.yml"),
        home.join(".config/gh/hosts.yml"),
    ] {
        assert!(p.is_private(&path), "{}", path.display());
    }
    assert!(!p.is_private(&other.join("other/file")));
}

#[test]
fn an_unknown_home_makes_every_credential_location_private() {
    // NV-31: fail closed. The station secrets stay private as before.
    let (_t, home) = home();
    let p = PrivatePaths::new(&layout(&home), None, None, &PrivateEnv::default());
    assert!(p.is_private(&home.join(".config/fleetdeck/station.toml")));
    assert!(p.is_private(&home.join(".local/share/fleetdeck/station/secrets/x")));
    for path in [
        ".claude/.credentials.json",
        "x/.codex/auth.json",
        ".config/gh/hosts.yml",
        ".config/gh",
        "x/.config/glab-cli/config.yml",
        ".ssh/id_ed25519",
    ] {
        assert!(p.is_private(&home.join(path)), "{path}");
        assert!(p.is_private_canonical(&home.join(path)), "{path}");
    }
    for path in [
        "src/auth.json",
        ".codex/config.toml",
        ".claude/settings.json",
        ".config/ghost/x",
        ".config/other/gh",
    ] {
        assert!(!p.is_private(&home.join(path)), "{path}");
    }
}

#[test]
fn an_unknown_home_adds_five_glob_excludes_and_a_known_home_does_not() {
    let (_t, home) = home();
    let top = home.join("repo");
    std::fs::create_dir_all(&top).unwrap();
    let unknown = PrivatePaths::new(&layout(&home), None, None, &PrivateEnv::default());
    let magic = if cfg!(target_os = "macos") {
        "exclude,glob,icase"
    } else {
        "exclude,glob"
    };
    let got = unknown.excludes_for(&top);
    for pattern in [
        "**/.claude/.credentials.json",
        "**/.codex/auth.json",
        "**/.config/gh/**",
        "**/.config/glab-cli/**",
        "**/.ssh/**",
    ] {
        assert!(got.contains(&format!(":({magic}){pattern}")), "{pattern}");
    }
    let known = private_of(&home).excludes_for(&top);
    assert!(!known.iter().any(|e| e.contains("**/.ssh/**")));
    assert!(!known.iter().any(|e| e.contains("**/.codex/auth.json")));
}

#[test]
fn the_deck_paths_are_private_when_the_deck_runs_in_the_process() {
    let (_t, home) = home();
    let dirs = layout(&home);
    let (config, data) = (dirs.deck_config(), dirs.deck());
    let without = PrivatePaths::new(&dirs, Some(&home), None, &PrivateEnv::default());
    assert!(!without.is_private(&config));
    assert!(!without.is_private(&data.join("deck.db")));
    let with = PrivatePaths::new(
        &dirs,
        Some(&home),
        Some((&config, &data)),
        &PrivateEnv::default(),
    );
    assert!(with.is_private(&config));
    assert!(with.is_private(&dirs.config.join("deck.toml.tmp-1")));
    assert!(with.is_private(&data.join("deck.db")));
}

#[test]
fn a_symlink_to_a_private_path_is_private_after_canonicalization() {
    let (_t, home) = home();
    let p = private_of(&home);
    std::os::unix::fs::symlink(
        home.join(".local/share/fleetdeck/station"),
        home.join("shortcut"),
    )
    .unwrap();
    assert!(p.is_private(&home.join("shortcut/secrets/jira_token")));
    assert!(p.is_private(&home.join("shortcut/not-yet")));
    // The lexical comparison does not follow it: callers pass canonical
    // paths there. (A file that exists is private by its inode, whatever
    // its name: use a path that has no file.)
    assert!(!p.is_private_canonical(&home.join("shortcut/not-yet")));
}

#[test]
fn a_non_canonical_form_fails_closed() {
    let (_t, home) = home();
    let p = private_of(&home);
    assert!(p.is_private_canonical(&home.join("notes.txt/../notes.txt")));
    assert!(p.is_private_canonical(Path::new("./notes.txt")));
}

#[test]
fn a_hard_link_to_a_private_file_is_private() {
    let (_t, home) = home();
    let dirs = layout(&home);
    let link = home.join("innocent.txt");
    std::fs::hard_link(dirs.station_config(), &link).unwrap();
    let p = PrivatePaths::new(&dirs, Some(&home), None, &PrivateEnv::default());
    assert!(p.is_private(&link));
    assert!(p.is_private_file(&std::fs::metadata(&link).unwrap()));
    assert!(!p.is_private(&home.join("notes.txt")));
    // The inode check is for files: a directory entry is not matched by it.
    assert!(!p.is_private_file(&std::fs::metadata(&home).unwrap()));
}

#[test]
fn a_file_with_more_than_one_link_is_refused_outside_git_only() {
    let (_t, home) = home();
    let p = PrivatePaths::default();
    let one = home.join("one.txt");
    std::fs::write(&one, "x").unwrap();
    let meta = |path: &Path| std::fs::metadata(path).unwrap();
    // A normal file (one link) is unchanged.
    assert!(!p.is_private_opened(&meta(&one), "one.txt"));
    let two = home.join("two.txt");
    std::fs::hard_link(&one, &two).unwrap();
    assert!(p.is_private_opened(&meta(&one), "one.txt"));
    assert!(p.is_private_opened(&meta(&two), "dir/two.txt"));
    // Git makes hard links itself: a path with a `.git` component stays
    // readable.
    assert!(!p.is_private_opened(&meta(&two), ".git/objects/ab/cdef"));
    assert!(!p.is_private_opened(&meta(&two), "sub/.git/objects/ab/cdef"));
    assert!(p.is_private_opened(&meta(&two), "my.git/objects"));
    // A directory is not a regular file.
    assert!(!p.is_private_opened(&meta(&home), ""));
}

#[test]
fn deny_all_refuses_every_path_and_default_refuses_none() {
    let all = PrivatePaths::deny_all();
    assert!(all.is_private(Path::new("/tmp")));
    assert!(all.is_private_canonical(Path::new("/does/not/matter")));
    let none = PrivatePaths::default();
    assert!(!none.is_private(Path::new("/tmp")));
    assert!(none.excludes_for(Path::new("/tmp")).is_empty());
}

#[test]
fn the_git_excludes_name_the_entries_below_the_top_and_the_temp_pattern() {
    let (_t, home) = home();
    let p = private_of(&home);
    let spec = p.excludes_for(&home);
    for expected in [
        ":(exclude,literal).config/fleetdeck/station.toml",
        ":(exclude,literal).local/share/fleetdeck/station",
        ":(exclude,literal).ssh",
        ":(exclude,literal).config/gh",
        ":(exclude,literal).claude/.credentials.json",
        ":(exclude,glob)**/station.toml.tmp-*",
    ] {
        let expected = if cfg!(target_os = "macos") {
            expected
                .replace("exclude,literal", "exclude,literal,icase")
                .replace("exclude,glob", "exclude,glob,icase")
        } else {
            expected.to_owned()
        };
        assert!(spec.contains(&expected), "{expected} in {spec:?}");
    }
    // A top that lies below an entry has no exclude for it; one that
    // holds no entry has none either.
    let below = p.excludes_for(&home.join(".local/share/fleetdeck/station/secrets"));
    assert!(below.iter().all(|s| s.contains("glob")), "{below:?}");
    let elsewhere = p.excludes_for(&home.join("project"));
    assert!(
        elsewhere.iter().all(|s| s.contains("glob")),
        "{elsewhere:?}"
    );
}

#[test]
fn canonical_of_joins_the_missing_tail_to_the_nearest_existing_ancestor() {
    let (_t, home) = home();
    std::fs::create_dir(home.join("real")).unwrap();
    std::os::unix::fs::symlink(home.join("real"), home.join("alias")).unwrap();
    assert_eq!(
        canonical_of(&home.join("alias/a/b.txt")),
        home.join("real/a/b.txt")
    );
    assert_eq!(canonical_of(&home.join("alias")), home.join("real"));
}

#[test]
fn the_start_registers_the_account_and_secret_files_and_a_restart_loads_them() {
    use std::os::unix::fs::MetadataExt;
    let (_t, home) = home();
    let dirs = layout(&home);
    let station = dirs.station();
    std::fs::create_dir_all(station.join("accounts")).unwrap();
    std::fs::write(station.join("accounts/acct-jira-0a1b2c3d.json"), "{}").unwrap();
    // A link and then the removal of the original: the link has one link.
    let link = home.join("alias-account.txt");
    let alias_secret = home.join("alias-secret.txt");
    let first = PrivatePaths::new(&dirs, Some(&home), None, &PrivateEnv::default());
    std::fs::hard_link(station.join("accounts/acct-jira-0a1b2c3d.json"), &link).unwrap();
    std::fs::hard_link(station.join("secrets/jira_token"), &alias_secret).unwrap();
    std::fs::remove_file(station.join("accounts/acct-jira-0a1b2c3d.json")).unwrap();
    std::fs::remove_file(station.join("secrets/jira_token")).unwrap();
    for path in [&link, &alias_secret] {
        let meta = std::fs::metadata(path).unwrap();
        assert_eq!(meta.nlink(), 1);
        assert!(first.is_private_file(&meta), "{path:?}");
        assert!(first.is_private(path), "{path:?}");
        assert!(first.is_private_opened(&meta, "x"), "{path:?}");
    }
    // The registry file is in the station directory, mode 0600.
    let file = station.join(super::REGISTRY_FILE);
    assert_eq!(
        std::fs::metadata(&file).unwrap().mode() & 0o777,
        0o600,
        "{file:?}"
    );
    // A restart: a new set loads the file.
    let again = PrivatePaths::new(&dirs, Some(&home), None, &PrivateEnv::default());
    for path in [&link, &alias_secret] {
        assert!(again.is_private(path), "{path:?}");
        assert!(again.is_private_opened(&std::fs::metadata(path).unwrap(), "x"));
    }
    // A normal file with one link is not refused.
    assert!(!again.is_private_opened(&std::fs::metadata(home.join("notes.txt")).unwrap(), "x"));
}

#[test]
fn register_adds_a_file_that_the_start_did_not_see() {
    let (_t, home) = home();
    let dirs = layout(&home);
    let p = PrivatePaths::new(&dirs, Some(&home), None, &PrivateEnv::default());
    let new = dirs.station().join("accounts/acct-linear-0a0a0a0a.json");
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::write(&new, "{}").unwrap();
    let link = home.join("late-link.txt");
    std::fs::hard_link(&new, &link).unwrap();
    std::fs::remove_file(&new).unwrap();
    assert!(!p.is_private_file(&std::fs::metadata(&link).unwrap()));
    // The same file again, registered by its identity through the link
    // (the call of the account store registers the temp file before the
    // rename).
    p.register(&link);
    assert!(p.is_private_file(&std::fs::metadata(&link).unwrap()));
    // The clones of a set share the registry.
    let clone = p.clone();
    let other = home.join("other-link.txt");
    std::fs::write(&other, "x").unwrap();
    clone.register(&other);
    assert!(p.is_private_file(&std::fs::metadata(&other).unwrap()));
}

#[test]
fn the_credential_directories_are_registered_at_start_and_by_a_rescan() {
    use std::os::unix::fs::MetadataExt;
    let (_t, home) = home();
    let dirs = layout(&home);
    let xdg = home.join("xdg");
    let ghdir = home.join("ghdir");
    for (dir, file) in [
        (home.join(".ssh/keys/deep"), "id_rsa"),
        (home.join(".config/glab-cli"), "config.yml"),
        (xdg.join("gh"), "hosts.yml"),
        (xdg.join("glab-cli"), "config.yml"),
        (ghdir.clone(), "hosts.yml"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), "secret").unwrap();
    }
    let env = PrivateEnv {
        xdg_config_home: Some(xdg.clone()),
        gh_config_dir: Some(ghdir.clone()),
    };
    let p = PrivatePaths::new(&dirs, Some(&home), None, &env);
    let files = [
        home.join(".ssh/id_ed25519"),
        home.join(".ssh/keys/deep/id_rsa"),
        home.join(".config/gh/hosts.yml"),
        home.join(".config/glab-cli/config.yml"),
        xdg.join("gh/hosts.yml"),
        xdg.join("glab-cli/config.yml"),
        ghdir.join("hosts.yml"),
        home.join(".claude/.credentials.json"),
    ];
    for file in &files {
        assert!(
            p.is_private_file(&std::fs::metadata(file).unwrap()),
            "{file:?}"
        );
    }
    // A file that a tool creates later is registered by the next rescan,
    // not before.
    let late = home.join(".ssh/id_late");
    std::fs::write(&late, "secret").unwrap();
    let meta = std::fs::metadata(&late).unwrap();
    assert!(!p.is_private_file(&meta));
    p.rescan();
    assert!(p.is_private_file(&meta));
    // A symlink in a credential directory is not followed.
    let outside = home.join("notes.txt");
    std::os::unix::fs::symlink(&outside, home.join(".ssh/pointer")).unwrap();
    p.rescan();
    assert!(!p.is_private_file(&std::fs::metadata(&outside).unwrap()));
    // The registry file keeps the new identity for a restart.
    let again = PrivatePaths::new(&dirs, Some(&home), None, &env);
    assert!(again.is_private_file(&std::fs::metadata(&late).unwrap()));
    assert_eq!(
        std::fs::metadata(dirs.station().join(super::REGISTRY_FILE))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
}
