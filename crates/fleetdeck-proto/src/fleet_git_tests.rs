// Tests of the Fleet Git wire types and checks (design §3.1 to §3.3, §3.6).
use super::*;
use serde_json::json;

fn field(settings: &GitSettings) -> Option<String> {
    check_settings(settings).err().map(|e| e.field)
}

#[test]
fn an_empty_settings_object_has_the_documented_defaults() {
    let got: GitSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(got, GitSettings::default());
    assert!(!got.sync);
    assert_eq!(got.push, PushMode::Off);
    assert_eq!(got.push_remote, "origin");
    assert_eq!(got.cleanup, CleanupMode::Off);
    assert_eq!(got.cleanup_after_hours, 24);
    assert!(got.wip_remote.is_none() && got.land.is_none() && got.radar_home.is_none());
    assert_eq!(check_settings(&got), Ok(()));
}

#[test]
fn an_unknown_settings_field_is_refused() {
    let err = serde_json::from_value::<GitSettings>(json!({"sync": true, "nope": 1}));
    assert!(err.is_err());
}

#[test]
fn an_unknown_mode_decodes_as_unknown_and_is_refused() {
    let got: GitSettings = serde_json::from_value(
        json!({"sync": true, "push": "on_commit", "cleanup": "all", "land": "squash"}),
    )
    .unwrap();
    assert_eq!(got.push, PushMode::Unknown);
    assert_eq!(got.cleanup, CleanupMode::Unknown);
    assert_eq!(got.land, Some(LandMode::Unknown));
    for (settings, want) in [
        (
            GitSettings {
                sync: true,
                push: PushMode::Unknown,
                ..GitSettings::default()
            },
            "push",
        ),
        (
            GitSettings {
                sync: true,
                cleanup: CleanupMode::Unknown,
                ..GitSettings::default()
            },
            "cleanup",
        ),
        (
            GitSettings {
                land: Some(LandMode::Unknown),
                ..GitSettings::default()
            },
            "land",
        ),
    ] {
        let err = check_settings(&settings).unwrap_err();
        assert_eq!(err.field, want);
        assert_eq!(err.message, "This value is not known. Update Fleet Deck.");
    }
}

#[test]
fn push_and_cleanup_need_sync() {
    let push = GitSettings {
        push: PushMode::OnTurn,
        ..GitSettings::default()
    };
    let err = check_settings(&push).unwrap_err();
    assert_eq!(
        (err.field.as_str(), err.message.as_str()),
        ("push", "Turn on sync first.")
    );
    let cleanup = GitSettings {
        cleanup: CleanupMode::Merged,
        ..GitSettings::default()
    };
    assert_eq!(field(&cleanup).as_deref(), Some("cleanup"));
    let both = GitSettings {
        sync: true,
        push: PushMode::OnExit,
        cleanup: CleanupMode::Merged,
        ..GitSettings::default()
    };
    assert_eq!(check_settings(&both), Ok(()));
}

#[test]
fn the_remote_fields_are_checked() {
    for bad in ["", "-x", ".x", "a b", "a/b", &"a".repeat(65)] {
        let s = GitSettings {
            push_remote: bad.to_owned(),
            ..GitSettings::default()
        };
        let err = check_settings(&s).unwrap_err();
        assert_eq!(err.field, "push_remote", "{bad}");
        assert_eq!(
            err.message,
            "The push remote must be a remote name, for example origin."
        );
    }
    for ok in ["origin", "up.stream", "a_b-c", &"a".repeat(64)] {
        let s = GitSettings {
            push_remote: ok.to_owned(),
            ..GitSettings::default()
        };
        assert_eq!(check_settings(&s), Ok(()), "{ok}");
    }
    let wip = |url: &str| GitSettings {
        wip_remote: Some(url.to_owned()),
        ..GitSettings::default()
    };
    assert_eq!(check_settings(&wip("ssh://git@nas/srv/wip.git")), Ok(()));
    assert_eq!(check_settings(&wip("git@nas:wip.git")), Ok(()));
    assert_eq!(check_settings(&wip("/srv/wip.git")), Ok(()));
    let err = check_settings(&wip("https://user:tok@h/a")).unwrap_err();
    assert_eq!(err.field, "wip_remote");
    assert_eq!(
        err.message,
        "The WIP remote must be an https, ssh or user@host:path URL without a password."
    );
    let err = check_settings(&wip("alice@h:repo%2Fother")).unwrap_err();
    assert_eq!(
        err.message,
        "This remote URL has an escaped path. Use a plain path."
    );
}

#[test]
fn the_numbers_and_names_are_checked() {
    let with = |f: fn(&mut GitSettings)| {
        let mut s = GitSettings::default();
        f(&mut s);
        s
    };
    assert_eq!(
        field(&with(|s| s.default_branch = Some("a..b".into()))).as_deref(),
        Some("default_branch")
    );
    assert_eq!(
        field(&with(|s| s.default_branch = Some("main".into()))),
        None
    );
    for hours in [0, 721] {
        let s = GitSettings {
            cleanup_after_hours: hours,
            ..GitSettings::default()
        };
        let err = check_settings(&s).unwrap_err();
        assert_eq!(err.field, "cleanup_after_hours");
        assert_eq!(err.message, "Clean up after 1 to 720 hours.");
    }
    for hours in [1, 720] {
        let s = GitSettings {
            cleanup_after_hours: hours,
            ..GitSettings::default()
        };
        assert_eq!(check_settings(&s), Ok(()));
    }
    assert_eq!(
        field(&with(|s| s.check_cmd = Some(String::new()))).as_deref(),
        Some("check_cmd")
    );
    assert_eq!(
        field(&with(|s| s.check_cmd = Some("a\0b".into()))).as_deref(),
        Some("check_cmd")
    );
    assert_eq!(
        field(&with(|s| s.check_cmd = Some("x".repeat(4097)))).as_deref(),
        Some("check_cmd")
    );
    assert_eq!(
        field(&with(|s| s.check_cmd = Some("make check".into()))),
        None
    );
}

#[test]
fn the_artifact_share_has_a_machine_and_a_share() {
    for ok in ["nas/artifacts", "n-1/my share(2)", "a/b"] {
        let s = GitSettings {
            artifact_share: Some(ok.to_owned()),
            ..GitSettings::default()
        };
        assert_eq!(check_settings(&s), Ok(()), "{ok}");
    }
    for bad in [
        "nas", "NAS/x", "nas/ x", "nas/x ", "nas/X", "/x", "nas/", "nas/a.b",
    ] {
        let s = GitSettings {
            artifact_share: Some(bad.to_owned()),
            ..GitSettings::default()
        };
        let err = check_settings(&s).unwrap_err();
        assert_eq!(err.field, "artifact_share", "{bad}");
        assert_eq!(
            err.message,
            "Use <machine>/<share>, for example nas/artifacts."
        );
    }
}

#[test]
fn worktree_include_rules() {
    let with = |items: &[&str]| GitSettings {
        worktree_include: items.iter().map(|s| (*s).to_owned()).collect(),
        ..GitSettings::default()
    };
    assert_eq!(
        check_settings(&with(&[".env", "config/*.local", "a?/b"])),
        Ok(())
    );
    for bad in [
        "../x", "a/../b", "/abs", "a//b", "a\\b", "**/x", "a[1]", "", "a\u{1}b",
    ] {
        let err = check_settings(&with(&[bad])).unwrap_err();
        assert_eq!(err.field, "worktree_include", "{bad:?}");
        assert_eq!(err.message, "Use relative paths without .. (at most 32).");
    }
    let many: Vec<String> = (0..33).map(|i| format!("f{i}")).collect();
    let s = GitSettings {
        worktree_include: many,
        ..GitSettings::default()
    };
    assert_eq!(field(&s).as_deref(), Some("worktree_include"));
    let long = "a".repeat(201);
    assert_eq!(field(&with(&[&long])).as_deref(), Some("worktree_include"));
}

#[test]
fn worktree_env_rules() {
    let with = |name: &str, value: &str| GitSettings {
        worktree_env: BTreeMap::from([(name.to_owned(), value.to_owned())]),
        ..GitSettings::default()
    };
    assert_eq!(check_settings(&with("RUSTC_WRAPPER", "sccache")), Ok(()));
    assert_eq!(check_settings(&with("_X1", "")), Ok(()));
    for bad in [
        "PATH",
        "HOME",
        "LD_PRELOAD",
        "DYLD_LIBRARY_PATH",
        "GIT_DIR",
        "ANTHROPIC_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "CLAUDECODE",
        "AI_AGENT",
        "OPENAI_API_KEY",
        "CODEX_HOME",
        "FLEETDECK_X",
        "lower",
        "1X",
        "A-B",
        "",
    ] {
        let err = check_settings(&with(bad, "v")).unwrap_err();
        assert_eq!(err.field, "worktree_env", "{bad}");
        assert_eq!(err.message, format!("The variable {bad} is not allowed."));
    }
    assert_eq!(
        check_settings(&with("X", "a\0b")).unwrap_err().field,
        "worktree_env"
    );
    assert_eq!(
        check_settings(&with("X", &"v".repeat(4097)))
            .unwrap_err()
            .field,
        "worktree_env"
    );
    let name64 = "A".repeat(64);
    assert_eq!(check_settings(&with(&name64, "v")), Ok(()));
    assert!(check_settings(&with(&"A".repeat(65), "v")).is_err());
    let many: BTreeMap<String, String> = (0..33).map(|i| (format!("V{i}"), "x".into())).collect();
    let s = GitSettings {
        worktree_env: many,
        ..GitSettings::default()
    };
    assert_eq!(field(&s).as_deref(), Some("worktree_env"));
}

#[test]
fn the_slug_follows_o6_with_no_truncation() {
    assert_eq!(station_slug("Alpha"), "alpha");
    assert_eq!(station_slug("my laptop_2"), "my-laptop-2");
    assert_eq!(station_slug("a--b"), "a--b");
    assert_eq!(station_slug("caf\u{e9}"), "caf-");
    let long = "A".repeat(63);
    assert_eq!(station_slug(&long), "a".repeat(63));
}

fn start() -> FleetGitStart {
    FleetGitStart {
        kind: StartKind::Worktree,
        project_id: "p1".into(),
        project_name: "app".into(),
        settings: GitSettings {
            sync: true,
            push: PushMode::OnTurn,
            ..GitSettings::default()
        },
        settings_revision: 7,
        home_generation: 2,
        home_path: "/w/app".into(),
        remote: RemoteIdentity {
            repo: "github.com/acme/app".into(),
            dest: "https://github.com:443/acme/app.git".into(),
        },
        slug: "alpha".into(),
        forge: None,
        clone_url: "https://github.com/acme/app.git".into(),
    }
}

#[test]
fn a_start_snapshot_round_trips() {
    let value = serde_json::to_value(start()).unwrap();
    assert_eq!(value["kind"], "worktree");
    assert_eq!(value["settings"]["push"], "on_turn");
    let back: FleetGitStart = serde_json::from_value(value).unwrap();
    assert_eq!(back, start());
    let in_place = json!({"kind": "in_place"});
    assert_eq!(
        serde_json::from_value::<StartKind>(in_place["kind"].clone()).unwrap(),
        StartKind::InPlace
    );
    assert_eq!(
        serde_json::from_value::<StartKind>(json!("teleport")).unwrap(),
        StartKind::Unknown
    );
}

#[test]
fn a_snapshot_and_its_ack_round_trip() {
    let snapshot = GitSnapshot {
        revision: 1_791_000_000_000,
        projects: vec![GitProjectView {
            project_id: "p1".into(),
            project_name: "app".into(),
            settings_revision: 3,
            settings: GitSettings::default(),
            remote: start().remote,
            forge: None,
            clone_url: "git@github.com:acme/app.git".into(),
            home_generation: 1,
            home_path: "/w/app".into(),
            radar_home: false,
        }],
        homes: vec![StationHome {
            project_id: "p1".into(),
            generation: 1,
            path: "/w/app".into(),
            sync: false,
        }],
    };
    let back: GitSnapshot =
        serde_json::from_value(serde_json::to_value(&snapshot).unwrap()).unwrap();
    assert_eq!(back, snapshot);
    let ack = SnapshotAck {
        stored: true,
        revision: snapshot.revision,
        checks: vec![
            HomeCheck {
                project_id: "p1".into(),
                generation: 1,
                state: HomeCheckState::Mismatch,
                canonical_path: Some("/w/app".into()),
                seen: Some(RemoteIdentity {
                    repo: "x/y".into(),
                    dest: "https://x:443/y".into(),
                }),
                code: Some(code::REMOTE_MISMATCH.into()),
            },
            HomeCheck {
                project_id: "p2".into(),
                generation: 4,
                state: HomeCheckState::Verified,
                canonical_path: None,
                seen: None,
                code: None,
            },
        ],
    };
    let value = serde_json::to_value(&ack).unwrap();
    assert!(
        value["checks"][1].get("seen").is_none(),
        "None is not serialized"
    );
    assert_eq!(serde_json::from_value::<SnapshotAck>(value).unwrap(), ack);
    assert_eq!(
        serde_json::from_value::<HomeCheckState>(json!("not_a_repo")).unwrap(),
        HomeCheckState::NotARepo
    );
    assert_eq!(
        serde_json::from_value::<HomeCheckState>(json!("future")).unwrap(),
        HomeCheckState::Unknown
    );
}

#[test]
fn the_state_enums_have_snake_case_names_and_an_unknown_value() {
    for (state, name) in [
        (PushState::Idle, "idle"),
        (PushState::Due, "due"),
        (PushState::Pushing, "pushing"),
        (PushState::Pushed, "pushed"),
        (PushState::Failing, "failing"),
        (PushState::Stopped, "stopped"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<PushState>(json!(name)).unwrap(),
            state
        );
    }
    assert_eq!(
        serde_json::from_value::<PushState>(json!("x")).unwrap(),
        PushState::Unknown
    );
    for (state, name) in [
        (CleanupState::Waiting, "waiting"),
        (CleanupState::Kept, "kept"),
        (CleanupState::Trashed, "trashed"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
    }
    assert_eq!(
        serde_json::from_value::<CleanupState>(json!("x")).unwrap(),
        CleanupState::Unknown
    );
}

#[test]
fn the_push_now_shapes_round_trip() {
    let params = PushNowParams {
        session_id: SessionId::from("01JABCDEFGHJKMNPQRSTVWXYZ0"),
    };
    assert_eq!(
        serde_json::to_value(&params).unwrap(),
        json!({"session_id": "01JABCDEFGHJKMNPQRSTVWXYZ0"})
    );
    assert_eq!(
        serde_json::to_value(PushNowAnswer {
            state: PushState::Due
        })
        .unwrap(),
        json!({"state": "due"})
    );
}

#[test]
fn the_codes_are_the_wire_strings_of_adr_009() {
    assert_eq!(code::NOT_SYNCED, "not_synced");
    assert_eq!(code::BRANCH_NAMESPACE_BLOCKED, "branch_namespace_blocked");
    assert_eq!(code::REMOTE_UNSUPPORTED, "remote_unsupported");
    assert_eq!(code::NO_DEFAULT_BRANCH, "no_default_branch");
    assert_eq!(code::INVALID_GIT_SETTINGS, "invalid_git_settings");
    assert_eq!(code::FLEET_GIT_UNSUPPORTED, "fleet_git_unsupported");
    assert_eq!(code::INTERNAL, "internal");
}

#[test]
fn the_feature_is_offered_and_not_required() {
    assert!(crate::version::feature::ALL.contains(&"fleet_git.v1"));
    assert!(!crate::version::feature::REQUIRED.contains(&"fleet_git.v1"));
    assert_eq!(crate::version::feature::FLEET_GIT, "fleet_git.v1");
}
