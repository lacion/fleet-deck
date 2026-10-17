// Tests of the phase 2 wire types (FG-5, FG-6, FG-11, FG-12): the homes,
// traits, drift and Taildrive types, `FileRoot::Artifacts`, the new
// `StationInfo` fields and the placement fields of `StartSessions`.
use super::*;
use crate::ids::SessionId;
use serde_json::json;

#[test]
fn the_homes_feature_is_offered_and_not_required() {
    use crate::version::feature;
    assert_eq!(feature::FLEET_GIT_HOMES, "fleet_git.homes.v1");
    assert!(feature::ALL.contains(&"fleet_git.homes.v1"));
    assert!(!feature::REQUIRED.contains(&"fleet_git.homes.v1"));
}

#[test]
fn ensure_home_params_and_status_have_their_wire_shapes() {
    let params = EnsureHomeParams {
        project_id: "p1".into(),
        project_name: "App".into(),
        folder: "App".into(),
        url: "git@github.com:acme/app.git".into(),
        remote: RemoteIdentity {
            repo: "github.com/acme/app".into(),
            dest: "ssh://git@github.com:22/acme/app.git".into(),
        },
        remote_name: "origin".into(),
        generation: 2,
        attempt_id: "ha-1".into(),
        request_digest: "ab".repeat(32),
        retry_of: None,
    };
    let v = serde_json::to_value(&params).unwrap();
    assert!(!v.as_object().unwrap().contains_key("retry_of"));
    assert_eq!(v["generation"], 2);
    let back: EnsureHomeParams = serde_json::from_value(v).unwrap();
    assert_eq!(back, params);
    let retry = EnsureHomeParams {
        retry_of: Some("ha-0".into()),
        ..params
    };
    let back: EnsureHomeParams =
        serde_json::from_value(serde_json::to_value(&retry).unwrap()).unwrap();
    assert_eq!(back.retry_of.as_deref(), Some("ha-0"));
    let status = HomeStatus {
        attempt_id: "ha-1".into(),
        generation: 2,
        state: HomeAttemptState::Cloning,
        path: None,
        reserved_path: Some("/srv/clones/App".into()),
        code: None,
        message: None,
    };
    assert_eq!(
        serde_json::to_value(&status).unwrap(),
        json!({"attempt_id": "ha-1", "generation": 2, "state": "cloning", "reserved_path": "/srv/clones/App"})
    );
    assert_eq!(
        serde_json::to_value(HomeStatusParams {
            project_id: "p1".into(),
            attempt_id: "ha-1".into()
        })
        .unwrap(),
        json!({"project_id": "p1", "attempt_id": "ha-1"})
    );
}

#[test]
fn a_home_attempt_state_decodes_each_name_and_an_unknown_one() {
    for (name, want) in [
        ("cloning", HomeAttemptState::Cloning),
        ("ready", HomeAttemptState::Ready),
        ("adopted", HomeAttemptState::Adopted),
        ("failed", HomeAttemptState::Failed),
        ("interrupted", HomeAttemptState::Interrupted),
        ("cleared", HomeAttemptState::Cleared),
        ("paused", HomeAttemptState::Unknown),
    ] {
        let got: HomeAttemptState = serde_json::from_value(json!(name)).unwrap();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn traits_and_preferences_have_their_wire_names() {
    let traits = StationTraits {
        os: "linux".into(),
        arch: "x86_64".into(),
        on_battery: None,
        labels: vec!["gpu".into()],
    };
    assert_eq!(
        serde_json::to_value(&traits).unwrap(),
        json!({"os": "linux", "arch": "x86_64", "labels": ["gpu"]})
    );
    let bare: StationTraits =
        serde_json::from_value(json!({"os": "macos", "arch": "aarch64"})).unwrap();
    assert!(bare.labels.is_empty() && bare.on_battery.is_none());
    let got: Vec<Prefer> =
        serde_json::from_value(json!(["idle", "not_on_battery", "always_on", "x"])).unwrap();
    assert_eq!(
        got,
        [
            Prefer::Idle,
            Prefer::NotOnBattery,
            Prefer::AlwaysOn,
            Prefer::Unknown
        ]
    );
}

#[test]
fn the_config_digest_has_its_wire_names() {
    let digest = ConfigDigest {
        claude_version: Some("2.1.0".into()),
        codex_version: None,
        items: vec![
            DigestItem {
                name: DriftItem::ClaudeMd,
                hash: "absent".into(),
            },
            DigestItem {
                name: DriftItem::CodexAgentsMd,
                hash: "ab".repeat(32),
            },
        ],
        computed_at: 5,
    };
    let v = serde_json::to_value(&digest).unwrap();
    assert!(!v.as_object().unwrap().contains_key("codex_version"));
    assert_eq!(v["items"][0]["name"], "claude_md");
    assert_eq!(v["items"][1]["name"], "codex_agents_md");
    assert_eq!(serde_json::from_value::<ConfigDigest>(v).unwrap(), digest);
    for (name, want) in [
        ("claude_agents", DriftItem::ClaudeAgents),
        ("claude_skills", DriftItem::ClaudeSkills),
        ("cursor_rules", DriftItem::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<DriftItem>(json!(name)).unwrap(),
            want
        );
    }
}

#[test]
fn an_old_shape_share_decodes_as_unknown_exposure() {
    let share: TaildriveShare =
        serde_json::from_value(json!({"name": "art", "path": "/srv/art"})).unwrap();
    assert_eq!(share.exposure_state, ExposureState::Unknown);
    assert_eq!(share.path_state, SharePathState::Unknown);
    assert!(share.candidates.is_empty() && share.exposed.is_empty());
    assert!(share.as_user.is_none() && share.unknown_reason.is_none());
}

#[test]
fn taildrive_enums_decode_each_name_and_fail_closed_on_an_unknown_one() {
    for (name, want) in [
        ("clear", ExposureState::Clear),
        ("exposed", ExposureState::Exposed),
        ("unknown", ExposureState::Unknown),
        ("new", ExposureState::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<ExposureState>(json!(name)).unwrap(),
            want
        );
    }
    for (name, want) in [
        ("ok", SharePathState::Ok),
        ("missing", SharePathState::Missing),
        ("ambiguous", SharePathState::Ambiguous),
        ("new", SharePathState::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<SharePathState>(json!(name)).unwrap(),
            want
        );
    }
    for (name, want) in [
        ("ok", ShareVerdict::Ok),
        ("exposed", ShareVerdict::Exposed),
        ("not_a_station", ShareVerdict::NotAStation),
        ("unknown", ShareVerdict::Unknown),
        ("new", ShareVerdict::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<ShareVerdict>(json!(name)).unwrap(),
            want
        );
    }
    for (name, want) in [
        ("ok", TaildriveState::Ok),
        ("unavailable", TaildriveState::Unavailable),
        ("unknown", TaildriveState::Unknown),
        ("new", TaildriveState::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<TaildriveState>(json!(name)).unwrap(),
            want
        );
    }
    let kinds: Vec<Exposure> = serde_json::from_value(json!([
        "project_home",
        "worktree",
        "data_dir",
        "claude_home",
        "codex_home",
        "ssh",
        "gnupg",
        "aws",
        "member_deny",
        "new"
    ]))
    .unwrap();
    assert_eq!(kinds.len(), 10);
    assert_eq!(kinds[8], Exposure::MemberDeny);
    assert_eq!(kinds[9], Exposure::Unknown);
}

#[test]
fn a_taildrive_report_round_trips_with_its_optional_fields_hidden() {
    let report = TaildriveReport {
        state: TaildriveState::Ok,
        reason: None,
        machine: Some("nas".into()),
        shares: vec![TaildriveShare {
            name: "art".into(),
            path: "/srv/art".into(),
            candidates: vec!["/srv/art".into()],
            as_user: None,
            exposed: vec![Exposure::Ssh],
            exposure_state: ExposureState::Exposed,
            unknown_reason: None,
            path_state: SharePathState::Ok,
        }],
        checked_at: 9,
        inventory_revision: 9,
    };
    let v = serde_json::to_value(&report).unwrap();
    assert!(!v.as_object().unwrap().contains_key("reason"));
    assert!(!v["shares"][0].as_object().unwrap().contains_key("as_user"));
    assert_eq!(v["shares"][0]["exposed"], json!(["ssh"]));
    assert_eq!(
        serde_json::from_value::<TaildriveReport>(v).unwrap(),
        report
    );
    let old: TaildriveReport =
        serde_json::from_value(json!({"state": "unavailable", "checked_at": 1})).unwrap();
    assert!(old.shares.is_empty() && old.inventory_revision == 0);
}

#[test]
fn the_artifact_authorization_and_the_verdict_calls_have_their_wire_shapes() {
    let auth = ArtifactAuthorization {
        share: "nas/art".into(),
        exporting_station: None,
        inventory_revision: None,
        verdict: ShareVerdict::NotAStation,
        exposed: Vec::new(),
        checked_at: None,
        issued_at: 10,
        expires_at: 610_010,
    };
    assert_eq!(
        serde_json::to_value(&auth).unwrap(),
        json!({"share": "nas/art", "verdict": "not_a_station", "exposed": [], "issued_at": 10, "expires_at": 610_010})
    );
    assert_eq!(
        serde_json::to_value(ArtifactAuthorizeParams {
            project_id: "p1".into(),
            session_id: SessionId::from("s1"),
            share: "nas/art".into()
        })
        .unwrap(),
        json!({"project_id": "p1", "session_id": "s1", "share": "nas/art"})
    );
    assert_eq!(
        serde_json::to_value(ShareVerdictParams {
            share_name: "art".into()
        })
        .unwrap(),
        json!({"share_name": "art"})
    );
    // A verdict answer without the states fails closed.
    let answer: ShareVerdictAnswer =
        serde_json::from_value(json!({"inventory_revision": 3, "checked_at": 4})).unwrap();
    assert_eq!(answer.exposure_state, ExposureState::Unknown);
    assert_eq!(answer.path_state, SharePathState::Unknown);
}

#[test]
fn the_artifacts_root_serializes_as_the_session_form() {
    use crate::files::{FileListQuery, FileRoot};
    let root = FileRoot::Artifacts(SessionId::from("s1"));
    assert_eq!(
        serde_json::to_value(&root).unwrap(),
        json!({"artifacts": "s1"})
    );
    let q: FileListQuery =
        serde_json::from_value(json!({"root": {"artifacts": "s2"}, "path": "a"})).unwrap();
    assert_eq!(q.root, FileRoot::Artifacts(SessionId::from("s2")));
}

#[test]
fn an_old_shape_station_info_decodes_and_the_new_fields_hide_when_empty() {
    use crate::link::StationInfo;
    let old = json!({"name": "n", "incarnation": "i", "head": 1, "live_sessions": 0});
    let info: StationInfo = serde_json::from_value(old).unwrap();
    assert!(info.traits.is_none() && info.config.is_none());
    assert!(info.taildrive.is_none() && info.fleet_git.is_none());
    let v = serde_json::to_value(&info).unwrap();
    for key in ["traits", "config", "taildrive", "fleet_git"] {
        assert!(!v.as_object().unwrap().contains_key(key), "{key}");
    }
    let new = StationInfo {
        fleet_git: Some(FleetGitInfo {
            clone_root: "/srv/clones".into(),
            allow_check_cmd: false,
            git_version: Some("2.45.0".into()),
            allow_local_remotes: true,
        }),
        ..info
    };
    let back: StationInfo = serde_json::from_value(serde_json::to_value(&new).unwrap()).unwrap();
    assert_eq!(back, new);
    let older: FleetGitInfo =
        serde_json::from_value(json!({"clone_root": "/c", "allow_check_cmd": true})).unwrap();
    assert!(!older.allow_local_remotes && older.git_version.is_none());
}

fn placement(f: impl FnOnce(&mut crate::integrations::StartSessions)) -> Result<(), &'static str> {
    let mut req = crate::integrations::StartSessions {
        batch_id: "b-1".into(),
        issues: vec!["PROJ-1".into()],
        provider_instance: "claude".into(),
        mode: None,
        model: None,
        count_per_issue: 1,
        review_of_pr: None,
        home: None,
        requires: Vec::new(),
        prefer: Vec::new(),
        spread: false,
    };
    f(&mut req);
    req.check()
}

#[test]
fn a_start_without_placement_fields_decodes_and_passes() {
    let req: crate::integrations::StartSessions = serde_json::from_value(json!({
        "batch_id": "b-1",
        "issues": ["PROJ-1"],
        "provider_instance": "claude",
        "count_per_issue": 1
    }))
    .unwrap();
    assert!(req.home.is_none() && req.requires.is_empty() && req.prefer.is_empty());
    assert!(!req.spread);
    assert_eq!(req.check(), Ok(()));
}

#[test]
fn the_placement_fields_of_a_start_are_checked() {
    assert_eq!(
        placement(|r| {
            r.home = Some("auto".into());
            r.requires = vec![
                "gpu".into(),
                "os:linux".into(),
                "arch:aarch64".into(),
                "a".repeat(32),
                format!("os:{}", "b".repeat(32)),
            ];
            r.prefer = vec![Prefer::Idle, Prefer::NotOnBattery, Prefer::AlwaysOn];
            r.spread = true;
        }),
        Ok(())
    );
    let label = "A station label is 1 to 32 characters: a-z, 0-9, _ or -.";
    for bad in [
        "",
        "os:",
        "arch:",
        "foo:bar",
        "os:Linux",
        "os:linux:x",
        "GPU",
        "a b",
        "gpu!",
    ] {
        assert_eq!(
            placement(|r| r.requires = vec![bad.into()]),
            Err(label),
            "{bad:?}"
        );
    }
    assert_eq!(placement(|r| r.requires = vec!["a".repeat(33)]), Err(label));
    assert_eq!(
        placement(|r| r.requires = vec![format!("os:{}", "b".repeat(33))]),
        Err(label)
    );
    assert_eq!(
        placement(|r| r.requires = (0..9).map(|i| format!("l{i}")).collect()),
        Err("At most 8 required labels.")
    );
    assert_eq!(
        placement(|r| r.prefer = vec![Prefer::Idle, Prefer::Idle]),
        Err("Each preference at most once.")
    );
    assert_eq!(
        placement(|r| {
            r.prefer = vec![
                Prefer::Idle,
                Prefer::NotOnBattery,
                Prefer::AlwaysOn,
                Prefer::Unknown,
            ];
        }),
        Err("Each preference at most once.")
    );
    for bad in ["", &"s".repeat(65), "a\nb"] {
        assert_eq!(
            placement(|r| r.home = Some(bad.into())),
            Err("Home is a station id or auto."),
            "{bad:?}"
        );
    }
    assert_eq!(placement(|r| r.home = Some("s".repeat(64))), Ok(()));
}
