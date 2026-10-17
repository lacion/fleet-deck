//! The artifact share of a project (design §15.5, §15.6, FG-12, B6): the
//! authorization, the folder, the variables, the access and the refresh. The
//! remote path (an authorization through the deck) is covered by T2.5.

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::tailscale::TailscaleCli;
use fleetdeck_proto::fleet_git::{
    ArtifactAuthorization, ExposureState, GitSnapshot, SharePathState, ShareVerdict, StationHome,
    TaildriveReport, code,
};
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::git_state::tests::{new_git, new_session, store, verified, view};
use crate::store::{ReadResult, Store};
use crate::taildrive::TaildriveSource;

pub(crate) const SESSION: &str = "S1";

const STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "box", "DNSName": "box.example.ts.net.", "TailscaleIPs": ["100.64.0.1"]},
  "CurrentTailnet": {"Name": "example.com", "MagicDNSSuffix": "example.ts.net"}
}"#;

/// The table of `tailscale drive list` (one row per `(name, path)`; the
/// columns are as wide as the longest field).
pub(crate) fn table(rows: &[(&str, &str)]) -> String {
    let a = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max(4);
    let b = rows.iter().map(|r| r.1.len()).max().unwrap_or(0).max(4);
    let mut out = format!(
        "{:<a$}    {:<b$}    as\n{}    {}    --\n",
        "name",
        "path",
        "-".repeat(a),
        "-".repeat(b)
    );
    for (name, path) in rows {
        out.push_str(&format!("{name:<a$}    {path:<b$}    alice\n"));
    }
    out
}

/// A station store with a fake `tailscale`, a Fleet Git runtime, one project
/// `p1` with the artifact share `box/out`, and the session `S1`.
pub(crate) struct Rig {
    pub det: Det,
    pub store: Store,
    pub runtime: Arc<FleetGitRuntime>,
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    /// `root/exports/out`: the share folder.
    pub share: PathBuf,
    /// `root/home`: the `$HOME` of the source.
    pub home: PathBuf,
    drive_file: PathBuf,
    log: PathBuf,
    revision: std::sync::atomic::AtomicU64,
}

impl Rig {
    /// A rig with the share `out` on the machine `box`, no snapshot yet.
    pub(crate) async fn new() -> Self {
        let det = Det::new();
        let store = store(&det).await;
        Self::on(det, store)
    }

    /// A rig on an existing store (the store of a manager).
    pub(crate) fn on(det: Det, store: Store) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let share = root.join("exports/out");
        for dir in ["exports/out", "home", "data", "work"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let (status, drive_file, log) = (
            root.join("status.json"),
            root.join("drive.txt"),
            root.join("argv.log"),
        );
        std::fs::write(&status, STATUS).unwrap();
        let bin = root.join("tailscale");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1\" in\n  status) cat '{}' ;;\n  drive) cat '{}' ;;\nesac\n",
            log.display(),
            status.display(),
            drive_file.display()
        );
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rig = Self {
            det,
            store,
            runtime: Arc::new(FleetGitRuntime::new(Arc::new(
                crate::workspace::GitOptions::isolated,
            ))),
            home: root.join("home"),
            drive_file,
            log,
            share,
            root,
            _tmp: tmp,
            revision: std::sync::atomic::AtomicU64::new(1),
        };
        rig.drive(&[("out", &rig.share.to_string_lossy())]);
        rig.set_mounts(BTreeMap::new());
        rig
    }

    /// Writes the output of the fake `tailscale drive list`.
    pub(crate) fn drive(&self, rows: &[(&str, &str)]) {
        std::fs::write(&self.drive_file, table(rows)).unwrap();
    }

    /// Sets the source of the report with these `[taildrive_mounts]`.
    pub(crate) fn set_mounts(&self, mounts: BTreeMap<String, PathBuf>) {
        self.runtime.set_taildrive_source(TaildriveSource {
            cli: TailscaleCli::new(self.root.join("tailscale")),
            data_dir: self.root.join("data"),
            home: Some(self.home.clone()),
            member_deny: MemberDeny::default(),
            instance_homes: Vec::new(),
            allowed_roots: vec![self.root.join("work")],
            mounts,
        });
    }

    /// A snapshot of `p1` with `artifact_share` whose home check names
    /// `home` as the verified path (a manager start needs it).
    pub(crate) async fn snapshot_at(&self, share: &str, home: &Path) {
        let revision = self
            .revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut project = view("p1");
        project.settings.artifact_share = Some(share.to_owned());
        project.home_path = home.to_string_lossy().into_owned();
        let mut check = verified("p1");
        check.canonical_path = Some(project.home_path.clone());
        let snapshot = GitSnapshot {
            revision,
            projects: vec![project],
            homes: Vec::new(),
        };
        let digest = format!("digest-{revision}");
        let applied = self.store.git_apply_snapshot(snapshot, digest, vec![check]);
        applied.await.unwrap();
    }

    /// A snapshot of `p1` with `artifact_share` and one home (sync off).
    pub(crate) async fn snapshot(&self, share: Option<&str>, home: Option<&Path>) {
        let revision = self
            .revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut project = view("p1");
        project.settings.artifact_share = share.map(str::to_owned);
        let snapshot = GitSnapshot {
            revision,
            projects: vec![project],
            homes: home
                .map(|path| StationHome {
                    project_id: "p1".to_owned(),
                    generation: 1,
                    path: path.to_string_lossy().into_owned(),
                    sync: false,
                })
                .into_iter()
                .collect(),
        };
        self.store
            .git_apply_snapshot(snapshot, format!("digest-{revision}"), vec![verified("p1")])
            .await
            .unwrap();
    }

    /// The session `S1` of `p1`.
    pub(crate) async fn session(&self) {
        self.store
            .create_session(new_session(SESSION))
            .await
            .unwrap();
        self.store
            .git_record_session(new_git(SESSION, "p1"), None)
            .await
            .unwrap();
    }

    /// A rig with the snapshot, the session and a first report.
    pub(crate) async fn ready() -> Self {
        let rig = Self::new().await;
        rig.snapshot(Some("box/out"), None).await;
        rig.session().await;
        rig.refresh().await;
        rig
    }

    pub(crate) async fn refresh(&self) -> TaildriveReport {
        taildrive::refresh(&self.runtime, &self.store)
            .await
            .unwrap()
    }

    /// How many times the fake `tailscale` ran with these first arguments.
    pub(crate) fn runs(&self, first: &str) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with(first))
            .count()
    }

    pub(crate) fn input<'a>(&self, session: &'a SessionId, share: &'a str) -> StepInput<'a> {
        StepInput {
            session_id: session,
            project_id: "p1",
            project_name: "My App",
            share,
            callsign: "ab12",
        }
    }

    /// The events of a session, in order.
    pub(crate) async fn events(&self) -> Vec<Event> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 1000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        events
            .into_iter()
            .filter(|e| e.session_id.as_str() == SESSION)
            .filter_map(|e| match e.event {
                EventBody::Known(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    pub(crate) async fn start(&self) -> BTreeMap<String, String> {
        let session = SessionId::from(SESSION);
        start_step(&self.runtime, &self.store, &self.input(&session, "box/out")).await
    }
}

fn unavailable(events: &[Event]) -> Vec<(&str, &str)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::ArtifactsUnavailable { code, text } => Some((code.as_str(), text.as_str())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_local_share_sets_the_variable_and_makes_the_folder_with_mode_0700() {
    let rig = Rig::ready().await;
    let vars = rig.start().await;
    let folder = rig.share.join("My-App/ab12");
    let expected = format!("{}/", folder.display());
    assert_eq!(vars.get(ENV_PATH), Some(&expected));
    assert_eq!(vars.len(), 1);
    for level in [rig.share.join("My-App"), folder.clone()] {
        let mode = std::fs::metadata(&level).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", level.display());
    }
    let events = rig.events().await;
    assert!(events.contains(&Event::ArtifactsReady {
        path: Some(expected.clone()),
        url: None
    }));
    let row = rig
        .store
        .artifacts_row(SESSION.into())
        .await
        .unwrap()
        .unwrap();
    let meta = std::fs::metadata(&folder).unwrap();
    assert_eq!(row.path.as_deref(), Some(expected.as_str()));
    assert_eq!((row.dev, row.ino), (Some(meta.dev()), Some(meta.ino())));
    let auth = row.auth.unwrap();
    assert_eq!(
        (auth.share.as_str(), auth.verdict),
        ("box/out", ShareVerdict::Ok)
    );
    assert_eq!(auth.exporting_station, None);
    assert_eq!(auth.expires_at - auth.issued_at, AUTH_TTL_MS);
    let summary = rig.store.summary(SESSION.into()).await.unwrap().unwrap();
    assert_eq!(
        summary.artifacts_dir.unwrap().path.as_deref(),
        Some(expected.as_str())
    );
}

#[tokio::test]
async fn an_exposed_share_gives_no_variable_and_artifacts_unavailable() {
    let rig = Rig::new().await;
    // The share holds a home of a project, sync off.
    let home = rig.share.join("proj");
    std::fs::create_dir(&home).unwrap();
    rig.snapshot(Some("box/out"), Some(&home)).await;
    rig.session().await;
    rig.refresh().await;
    assert!(rig.start().await.is_empty());
    let events = rig.events().await;
    let got = unavailable(&events);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, code::SHARE_EXPOSED);
    assert!(
        got[0]
            .1
            .starts_with("The artifact share exposes protected folders")
    );
    assert!(!rig.share.join("My-App").exists(), "no folder is made");
}

#[tokio::test]
async fn a_share_that_is_not_listed_or_before_the_first_snapshot_is_unverified() {
    let rig = Rig::new().await;
    rig.session().await;
    // No snapshot yet: every share is unknown (design §15.4).
    let report = rig.refresh().await;
    assert_eq!(report.shares[0].exposure_state, ExposureState::Unknown);
    assert_eq!(
        report.shares[0].unknown_reason.as_deref(),
        Some(taildrive::NO_HOMES_YET)
    );
    assert!(rig.start().await.is_empty());
    rig.snapshot(Some("box/other"), None).await;
    rig.refresh().await;
    let session = SessionId::from(SESSION);
    let unlisted = start_step(&rig.runtime, &rig.store, &rig.input(&session, "box/other")).await;
    assert!(unlisted.is_empty());
    let events = rig.events().await;
    let got = unavailable(&events);
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|(c, _)| *c == code::SHARE_UNVERIFIED));
    assert!(got[0].1.contains("cannot check the artifact share now"));
}

#[tokio::test]
async fn an_ambiguous_share_needs_a_configured_mount_that_is_one_candidate() {
    let rig = Rig::new().await;
    rig.snapshot(Some("box/out"), None).await;
    rig.session().await;
    // The path field is padded: a longer path in the other row.
    let out = rig.share.to_string_lossy().into_owned();
    let long = format!("{out}/a/much/longer/path");
    rig.drive(&[("out", &out), ("zz", &long)]);
    let report = rig.refresh().await;
    assert_eq!(report.shares[0].path_state, SharePathState::Ambiguous);
    // Without a mount: share_unverified with the text of design §15.3.
    assert!(rig.start().await.is_empty());
    let events = rig.events().await;
    let got = unavailable(&events);
    assert_eq!(got[0].0, code::SHARE_UNVERIFIED);
    assert_eq!(
        got[0].1,
        "Fleet Deck cannot tell the exact path of the share out. Add it to [taildrive_mounts] in station.toml."
    );
    // A mount that is not a candidate: the same refusal.
    rig.set_mounts(BTreeMap::from([(
        "box/out".to_owned(),
        rig.root.join("work"),
    )]));
    rig.refresh().await;
    assert!(rig.start().await.is_empty());
    // A mount that is one candidate: the share is used through the mount.
    rig.set_mounts(BTreeMap::from([("box/out".to_owned(), rig.share.clone())]));
    rig.refresh().await;
    let vars = rig.start().await;
    assert_eq!(
        vars.get(ENV_PATH),
        Some(&format!("{out}/My-App/ab12/")),
        "{:?}",
        unavailable(&rig.events().await)
    );
}

#[test]
fn the_local_path_checks_give_the_three_mount_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("none");
    assert_eq!(check_local(&missing).unwrap_err().code, MOUNT_MISSING);
    let file = tmp.path().join("file");
    std::fs::write(&file, "x").unwrap();
    let denied = check_local(&file).unwrap_err();
    assert_eq!(denied.code, MOUNT_NOT_FOLDER);
    assert_eq!(
        denied.text,
        format!(
            "The artifact mount {} is not a folder; the session started without FLEETDECK_ARTIFACTS.",
            file.display()
        )
    );
    let ro = tmp.path().join("ro");
    std::fs::create_dir(&ro).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
    if !rustix::process::geteuid().is_root() {
        assert_eq!(check_local(&ro).unwrap_err().code, MOUNT_NOT_WRITABLE);
    }
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(check_local(&ro).is_ok());
    assert_eq!(
        unavailable_text(MOUNT_MISSING, "/m"),
        "The artifact mount /m does not exist; the session started without FLEETDECK_ARTIFACTS."
    );
    assert_eq!(
        unavailable_text(TAILNET_UNKNOWN, ""),
        "The tailnet name is not known; the session started without FLEETDECK_ARTIFACTS_URL."
    );
}

#[test]
fn the_url_percent_encodes_every_segment() {
    assert_eq!(
        url_of("example.com", "nas", "my share (1)", "My App", "ab12"),
        "http://100.100.100.100:8080/example.com/nas/my%20share%20%281%29/My%20App/ab12/"
    );
    assert_eq!(encode_segment("a@b~c_d-e.f"), "a%40b~c_d-e.f");
    assert_eq!(encode_segment("é"), "%C3%A9");
}

fn auth(verdict: ShareVerdict, expires_at: i64) -> ArtifactAuthorization {
    ArtifactAuthorization {
        share: "nas/out".to_owned(),
        exporting_station: None,
        inventory_revision: None,
        verdict,
        exposed: Vec::new(),
        checked_at: None,
        issued_at: 0,
        expires_at,
    }
}

#[test]
fn only_a_current_ok_or_not_a_station_authorizes() {
    assert_eq!(refusal_of(&auth(ShareVerdict::Ok, 100), 99), None);
    assert_eq!(refusal_of(&auth(ShareVerdict::NotAStation, 100), 99), None);
    // An authorization whose expires_at passed is never used.
    for verdict in [ShareVerdict::Ok, ShareVerdict::NotAStation] {
        assert_eq!(
            refusal_of(&auth(verdict, 100), 100),
            Some(code::SHARE_UNVERIFIED)
        );
    }
    assert_eq!(
        refusal_of(&auth(ShareVerdict::Exposed, 100), 1),
        Some(code::SHARE_EXPOSED)
    );
    assert_eq!(
        refusal_of(&auth(ShareVerdict::Unknown, 100), 1),
        Some(code::SHARE_UNVERIFIED)
    );
}

#[tokio::test]
async fn a_share_on_another_machine_without_a_deck_link_is_unverified() {
    let rig = Rig::ready().await;
    let session = SessionId::from(SESSION);
    let ask = Ask {
        project_id: "p1",
        session_id: &session,
        share: "nas/out",
    };
    let denied = authorize(&rig.runtime, &rig.store, &ask).await.unwrap_err();
    assert_eq!(denied.code, code::SHARE_UNVERIFIED);
    let bad = Ask {
        share: "no-slash",
        ..ask
    };
    assert_eq!(
        authorize(&rig.runtime, &rig.store, &bad)
            .await
            .unwrap_err()
            .code,
        code::SHARE_UNVERIFIED
    );
}

#[tokio::test]
async fn the_verdict_answer_of_a_share_follows_decision_s5() {
    let rig = Rig::ready().await;
    let report = rig.runtime.taildrive_report().unwrap();
    let mounts = BTreeMap::new();
    let listed = verdict_answer(Some(&report), "out", &mounts, 5);
    assert_eq!(listed.path_state, SharePathState::Ok);
    assert_eq!(listed.exposure_state, ExposureState::Clear);
    assert_eq!(listed.inventory_revision, report.inventory_revision);
    // A share that is not listed: missing, unknown, no exposure.
    let missing = verdict_answer(Some(&report), "nope", &mounts, 5);
    assert_eq!(missing.path_state, SharePathState::Missing);
    assert_eq!(missing.exposure_state, ExposureState::Unknown);
    assert!(missing.exposed.is_empty());
    assert_eq!(missing.checked_at, report.checked_at);
    // No report: unknown, revision 0 and now.
    let none = verdict_answer(None, "out", &mounts, 5);
    assert_eq!(
        (
            none.path_state,
            none.exposure_state,
            none.inventory_revision,
            none.checked_at
        ),
        (SharePathState::Unknown, ExposureState::Unknown, 0, 5)
    );
    // An ambiguous share that the mounts resolve answers `ok`.
    let out = rig.share.to_string_lossy().into_owned();
    rig.drive(&[("out", &out), ("zz", &format!("{out}/a/longer/path"))]);
    let report = rig.refresh().await;
    assert_eq!(
        verdict_answer(Some(&report), "out", &mounts, 5).path_state,
        SharePathState::Ambiguous
    );
    let mounted = BTreeMap::from([("box/out".to_owned(), rig.share.clone())]);
    assert_eq!(
        verdict_answer(Some(&report), "out", &mounted, 5).path_state,
        SharePathState::Ok
    );
}

#[tokio::test]
async fn the_folder_opens_while_authorized_and_a_replaced_folder_is_refused() {
    let rig = Rig::ready().await;
    rig.start().await;
    let session = SessionId::from(SESSION);
    let (_fd, path) = open_folder(&rig.runtime, &rig.store, &session)
        .await
        .unwrap();
    assert_eq!(path, rig.share.join("My-App/ab12"));
    // Another folder with the same name: other device or inode.
    let folder = rig.share.join("My-App/ab12");
    std::fs::rename(&folder, rig.share.join("My-App/old")).unwrap();
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(
        open_folder(&rig.runtime, &rig.store, &session)
            .await
            .unwrap_err(),
        AccessError::Unavailable
    );
    // A session without an artifact folder.
    rig.store.create_session(new_session("S2")).await.unwrap();
    rig.store
        .git_record_session(new_git("S2", "p1"), None)
        .await
        .unwrap();
    assert_eq!(
        open_folder(&rig.runtime, &rig.store, &SessionId::from("S2"))
            .await
            .unwrap_err(),
        AccessError::NoFolder
    );
}

#[tokio::test]
async fn a_share_that_becomes_exposed_is_refused_at_once_and_the_refresh_revokes_it() {
    let rig = Rig::ready().await;
    rig.start().await;
    let session = SessionId::from(SESSION);
    assert!(
        open_folder(&rig.runtime, &rig.store, &session)
            .await
            .is_ok()
    );
    // A refresh with a share that is still fine changes nothing.
    refresh_sessions(&rig.runtime, &rig.store).await;
    let count = |events: &[Event]| {
        events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Event::ArtifactsRevoked { .. } | Event::ArtifactsReady { .. }
                )
            })
            .count()
    };
    assert_eq!(count(&rig.events().await), 1);
    // A home of the project appears inside the share.
    let home = rig.share.join("My-App");
    rig.snapshot(Some("box/out"), Some(&home)).await;
    taildrive::recompute(&rig.runtime, &rig.store)
        .await
        .unwrap();
    assert_eq!(
        open_folder(&rig.runtime, &rig.store, &session)
            .await
            .unwrap_err(),
        AccessError::Unavailable
    );
    refresh_sessions(&rig.runtime, &rig.store).await;
    let events = rig.events().await;
    let revoked: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::ArtifactsRevoked { code, text } => Some((code.as_str(), text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(revoked, [(code::SHARE_EXPOSED, REVOKED_TEXT)]);
    let summary = rig.store.summary(SESSION.into()).await.unwrap().unwrap();
    assert!(summary.artifacts_dir.is_none());
    // A second refresh does not append again.
    refresh_sessions(&rig.runtime, &rig.store).await;
    assert_eq!(count(&rig.events().await), 2);
    // The home goes away: the later refresh that is ok shows the folder again.
    rig.snapshot(Some("box/out"), None).await;
    taildrive::recompute(&rig.runtime, &rig.store)
        .await
        .unwrap();
    refresh_sessions(&rig.runtime, &rig.store).await;
    let events = rig.events().await;
    assert!(matches!(
        events.last(),
        Some(Event::ArtifactsReady { path: Some(_), .. })
    ));
    let summary = rig.store.summary(SESSION.into()).await.unwrap().unwrap();
    assert!(summary.artifacts_dir.is_some());
}

#[tokio::test]
async fn a_changed_or_removed_project_share_refuses_the_access_and_revokes() {
    let rig = Rig::ready().await;
    rig.start().await;
    let session = SessionId::from(SESSION);
    rig.snapshot(None, None).await;
    assert_eq!(
        open_folder(&rig.runtime, &rig.store, &session)
            .await
            .unwrap_err(),
        AccessError::Unavailable
    );
    refresh_sessions(&rig.runtime, &rig.store).await;
    assert!(matches!(
        rig.events().await.last(),
        Some(Event::ArtifactsRevoked { code, .. }) if code == code::SHARE_UNVERIFIED
    ));
    rig.snapshot(Some("box/other"), None).await;
    assert_eq!(
        open_folder(&rig.runtime, &rig.store, &session)
            .await
            .unwrap_err(),
        AccessError::Unavailable
    );
}

#[tokio::test]
async fn the_step_without_a_deck_for_a_remote_share_gives_unavailable() {
    let rig = Rig::new().await;
    rig.snapshot(Some("nas/out"), None).await;
    rig.session().await;
    rig.refresh().await;
    let session = SessionId::from(SESSION);
    let vars = start_step(&rig.runtime, &rig.store, &rig.input(&session, "nas/out")).await;
    assert!(vars.is_empty());
    let events = rig.events().await;
    assert_eq!(unavailable(&events)[0].0, code::SHARE_UNVERIFIED);
}
