//! The coordinator desktop methods of the station link (R13 P6 §5.5, §5.7,
//! §7.3): the claimed run of a mutation, `desktop.policy_now` and the
//! protected reads. The deck answer is a fake; the desktop is the fake
//! desktop, the store and the artifact tree are real.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_proto::artifacts::{ArtifactDeleteParams, ArtifactFile};
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance, CaptureStatus};
use fleetdeck_proto::coordinator_desktop::CoordinatorDesktopInfo;
use fleetdeck_proto::coordinator_desktop_text::limits;
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, CoordinatorContext, CoordinatorRead, ProtectedRead,
};
use fleetdeck_proto::desktop::{DesktopAction as A, Urgency};
use fleetdeck_proto::{SessionId, StationId};
use serde_json::{Value, json};

use super::*;
use crate::artifacts::capture_import::tests::{FP, Rig as StoreRig, rig as store_rig};
use crate::desktop::coordinator::{fleet_off, lost, unknown};
use crate::desktop::fake::{Opts, Rig};
use crate::store::capture_ops::NewCaptureArtifact;
use crate::store::desktop_ops::DesktopOpRow;

const NODE: &str = "aa11";
const OP: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const CAPTURE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const UNKNOWN_TEXT: &str =
    "Desktop authority could not be checked. No further desktop work was started.";

fn on() -> bool {
    true
}

fn ctx() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "grant-1".to_owned(),
        binding_id: "binding-1".to_owned(),
        generation: 3,
        caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZA"),
        caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZB"),
        caller_label: "bold-fox".to_owned(),
    }
}

/// A fake deck check: it answers a fixed result and can run a hook.
struct Check {
    result: Result<(), Fail>,
    seen: Mutex<Vec<(AuthorityPhase, Option<ProtectedRead>)>>,
    hook: Box<dyn Fn() + Send + Sync>,
}

impl Check {
    fn new(result: Result<(), Fail>, hook: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            result,
            seen: Mutex::new(Vec::new()),
            hook: Box::new(hook),
        })
    }

    fn ok() -> Arc<Self> {
        Self::new(Ok(()), || {})
    }

    fn failing(fail: Fail) -> Arc<Self> {
        Self::new(Err(fail), || {})
    }
}

#[async_trait]
impl DispatchCheck for Check {
    async fn check(&self, phase: AuthorityPhase, read: Option<&ProtectedRead>) -> Result<(), Fail> {
        (self.hook)();
        self.seen.lock().unwrap().push((phase, read.cloned()));
        self.result.clone()
    }
}

fn open_gates() -> Gates {
    Arc::new(|| Ok(()))
}

struct T {
    fake: Rig,
    s: StoreRig,
}

async fn setup() -> T {
    T {
        fake: Rig::new(Opts::default()),
        s: store_rig().await,
    }
}

fn notify() -> A {
    A::Notify {
        title: "Hello".into(),
        body: "Body".into(),
        urgency: Urgency::Normal,
    }
}

fn browser() -> A {
    A::BrowserOpen {
        url: "http://localhost:3000/".into(),
        monitor: None,
        session: None,
    }
}

fn pair(f: &Fail) -> (&'static str, &str) {
    (f.code, f.message.as_str())
}

type Acted = Result<fleetdeck_proto::desktop::DesktopActResult, Fail>;

impl T {
    /// One claimed run with the given hash, check and gates.
    async fn run(
        &self,
        action: A,
        hash: &str,
        check: Arc<dyn DispatchCheck>,
        gates: Gates,
    ) -> Acted {
        let kind = claim_kind(&action).unwrap();
        let extras = Extras {
            deck_fp: FP.to_owned(),
            tailnet_host: None,
        };
        let admitted =
            Admitted::open_coordinated(&action, NODE, ctx(), check, Arc::clone(&gates), extras);
        let run = Run {
            store: &self.s.store,
            desktop: &self.fake.desktop,
            deck_fp: FP,
            gate: &on,
        };
        let given = Given {
            action_id: OP.to_owned(),
            action_hash: hash.to_owned(),
            coordinator: Some(ctx()),
        };
        run_claimed(&run, &given, &ctx(), kind, &gates, action, admitted).await
    }

    async fn go(&self, action: A, hash: &str) -> Acted {
        self.run(action, hash, Check::ok(), open_gates()).await
    }

    async fn row(&self) -> DesktopOpRow {
        self.s.store.desktop_op_get(FP, OP).await.unwrap().unwrap()
    }

    fn count(&self, program: &str) -> usize {
        self.fake.calls_of(program).len()
    }
}

// --------------------------------------------------------- the claimed run

#[tokio::test]
async fn a_notification_runs_once_and_a_repeat_answers_the_stored_result() {
    let t = setup().await;
    let check = Check::ok();
    let first = t
        .run(notify(), "h1", check.clone(), open_gates())
        .await
        .unwrap();
    assert_eq!((first.address, first.window_id), (None, None));
    assert_eq!(t.count("notify-send"), 1);
    let row = t.row().await;
    assert_eq!((row.state.as_str(), row.kind.as_str()), ("done", "notify"));
    assert_eq!(row.result, Some(json!({})));
    // The same operation again: the stored result, a fresh read-only info,
    // no second effect and no second authority check.
    let again = t
        .run(notify(), "h1", check.clone(), open_gates())
        .await
        .unwrap();
    assert_eq!((again.address, again.window_id), (None, None));
    assert!(again.info.monitors.iter().any(|m| m.name == "DP-1"));
    assert_eq!(t.count("notify-send"), 1);
    assert_eq!(check.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_preview_replay_returns_the_same_handle_without_a_second_launch() {
    let t = setup().await;
    let first = t.go(browser(), "h1").await.unwrap();
    let handle = first.window_id.clone().unwrap();
    assert_eq!(first.address, None);
    assert_eq!(t.row().await.result, Some(json!({ "window_id": handle })));
    let again = t.go(browser(), "h1").await.unwrap();
    assert_eq!((again.window_id, again.address), (Some(handle), None));
    assert_eq!(t.count("chromium"), 1);
}

#[tokio::test]
async fn a_claim_that_never_finished_is_unknown_and_runs_no_effect() {
    let t = setup().await;
    let store = &t.s.store;
    store
        .desktop_op_claim(FP, OP, "h1", "notify", &ctx(), 1)
        .await
        .unwrap();
    let e = t.go(notify(), "h1").await.unwrap_err();
    assert_eq!(pair(&e), ("authority_unknown", UNKNOWN_TEXT));
    assert!(t.fake.calls().is_empty());
    assert_eq!(t.row().await.state, "claimed");
}

#[tokio::test]
async fn another_hash_or_kind_with_the_same_operation_id_is_unknown() {
    let t = setup().await;
    t.go(notify(), "h1").await.unwrap();
    for (action, hash) in [(notify(), "h2"), (browser(), "h1")] {
        let e = t.go(action, hash).await.unwrap_err();
        assert_eq!(e.code, "authority_unknown");
    }
    assert_eq!((t.count("notify-send"), t.count("chromium")), (1, 0));
}

#[tokio::test]
async fn a_gate_that_is_off_after_the_claim_stores_the_failure_and_runs_nothing() {
    let t = setup().await;
    let gates: Gates = Arc::new(|| Err(fleet_off()));
    let e = t.run(notify(), "h1", Check::ok(), gates).await.unwrap_err();
    assert_eq!(
        pair(&e),
        ("forbidden", "Fleet control is off on this station.")
    );
    assert!(t.fake.calls().is_empty());
    let row = t.row().await;
    assert_eq!(
        (row.state.as_str(), row.code.as_deref()),
        ("failed", Some("forbidden"))
    );
    // A repeat never runs it.
    let e = t.go(notify(), "h1").await.unwrap_err();
    assert_eq!(e.code, "authority_unknown");
    assert!(t.fake.calls().is_empty());
}

#[tokio::test]
async fn a_failed_effect_and_a_withdrawn_authority_store_their_codes() {
    let t = setup().await;
    t.fake.fail("notify-send");
    assert_eq!(t.go(notify(), "h1").await.unwrap_err().code, "failed");
    let row = t.row().await;
    assert_eq!(
        (row.state.as_str(), row.code.as_deref()),
        ("failed", Some("failed"))
    );
    assert_eq!(
        t.go(notify(), "h1").await.unwrap_err().code,
        "authority_unknown"
    );
    assert_eq!(t.count("notify-send"), 1);
    // A withdrawn authority: the operation is failed with that code.
    let t = setup().await;
    let e = t
        .run(notify(), "h1", Check::failing(lost()), open_gates())
        .await
        .unwrap_err();
    assert_eq!(e.code, "authority_lost");
    assert!(t.fake.calls().is_empty());
    assert_eq!(t.row().await.code.as_deref(), Some("authority_lost"));
}

#[tokio::test]
async fn a_failed_replay_info_returns_its_failure_and_leaves_the_operation() {
    let t = setup().await;
    t.go(notify(), "h1").await.unwrap();
    let monitors = t.fake.dir.path().join("monitors.json");
    let good = std::fs::read_to_string(&monitors).unwrap();
    std::fs::write(&monitors, "not json").unwrap();
    assert_eq!(t.go(notify(), "h1").await.unwrap_err().code, "failed");
    let row = t.row().await;
    assert_eq!((row.state.as_str(), row.result), ("done", Some(json!({}))));
    // It still answers once the desktop reads again.
    std::fs::write(&monitors, good).unwrap();
    t.go(notify(), "h1").await.unwrap();
    assert_eq!(t.count("notify-send"), 1);
}

// -------------------------------------------------------------- policy_now

fn live(monitors: &[&str]) -> Live {
    Live {
        fleet_control: true,
        desktop: true,
        capture: true,
        monitors: monitors.iter().map(|m| (*m).to_owned()).collect(),
    }
}

#[test]
fn the_policy_answer_follows_the_live_settings() {
    let a = policy_answer(
        &Live {
            desktop: false,
            ..live(&[])
        },
        None,
    );
    assert_eq!(
        (
            a.allow_fleet_control,
            a.allow_desktop,
            a.allow_desktop_capture,
            a.monitor_allowed
        ),
        (true, false, true, true)
    );
    // A null monitor is allowed; an empty list allows every monitor.
    assert!(policy_answer(&live(&["DP-1"]), None).monitor_allowed);
    assert!(policy_answer(&live(&[]), Some("DP-9")).monitor_allowed);
    // A list follows the current allowlist.
    assert!(policy_answer(&live(&["DP-1"]), Some("DP-1")).monitor_allowed);
    assert!(!policy_answer(&live(&["DP-1"]), Some("DP-2")).monitor_allowed);
    let off = Live {
        fleet_control: false,
        ..live(&[])
    };
    assert!(!policy_answer(&off, None).allow_fleet_control);
}

// ------------------------------------------------------------------ reads

fn request(read: ProtectedRead) -> CoordinatorRead {
    CoordinatorRead {
        read_id: "ab".repeat(16),
        read,
        read_hash: "cd".repeat(32),
    }
}

fn status_read(op: &str) -> ProtectedRead {
    ProtectedRead::ActionStatus {
        operation_id: op.to_owned(),
    }
}

fn capture_read(id: &str) -> ProtectedRead {
    ProtectedRead::CaptureStatus {
        capture_id: id.to_owned(),
    }
}

fn file_read(artifact_id: &str, version: u32, offset: u64, length: u64) -> ProtectedRead {
    ProtectedRead::CaptureFile {
        artifact_id: artifact_id.to_owned(),
        version,
        offset,
        length,
    }
}

#[test]
fn the_shapes_of_a_read_are_checked_first() {
    let info = request(ProtectedRead::Info);
    assert!(validate_read(&info).is_ok());
    let hashed = |read_id: String, read_hash: String| CoordinatorRead {
        read_id,
        read_hash,
        ..info.clone()
    };
    let (id, hash) = ("ab".repeat(16), "cd".repeat(32));
    let bad_reads = [
        hashed("AB".repeat(16), hash.clone()),
        hashed("ab".repeat(15), hash.clone()),
        hashed(id.clone(), "cd".repeat(31)),
        hashed(id, "CD".repeat(32)),
        request(status_read("short")),
        request(status_read(&format!("{}-", "A".repeat(25)))),
        request(capture_read("01arz3ndektsv4rrffq69g5fav")),
        request(file_read(&"x".repeat(25), 1, 0, 10)),
        request(file_read(OP, 1, 1, 10)),
    ];
    let text = "The desktop tool arguments are not valid.";
    for bad in bad_reads {
        let e = validate_read(&bad).unwrap_err();
        assert_eq!(pair(&e), ("invalid_params", text), "{bad:?}");
    }
    for ok in [
        status_read(OP),
        capture_read(CAPTURE),
        file_read(OP, 1, 0, 10),
    ] {
        assert!(validate_read(&request(ok)).is_ok());
    }
}

/// The live settings that a test moves.
struct Settings(Arc<Mutex<Live>>);

impl Settings {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(live(&[]))))
    }

    fn set(&self, f: impl FnOnce(&mut Live)) {
        f(&mut self.0.lock().unwrap());
    }
}

impl T {
    async fn read_with_check(
        &self,
        s: &Settings,
        check: &dyn DispatchCheck,
        read: ProtectedRead,
    ) -> Result<Value, Fail> {
        let shared = Arc::clone(&s.0);
        let live = move || shared.lock().unwrap().clone();
        let reader = Reader {
            deck_fp: FP,
            desktop: &self.fake.desktop,
            service: Some(&self.s.service),
            store: &self.s.store,
            live: &live,
        };
        read_with(&reader, check, &request(read)).await
    }

    /// A read that the fake deck allows.
    async fn rd(&self, s: &Settings, read: ProtectedRead) -> Result<Value, Fail> {
        let check = Check::ok();
        self.read_with_check(s, &*check, read).await
    }
}

#[tokio::test]
async fn the_info_read_maps_the_desktop_and_the_check_follows_the_work() {
    let t = setup().await;
    let dir = t.fake.dir.path().to_owned();
    let before = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&before);
    let check = Check::new(Ok(()), move || {
        let reads = std::fs::read_to_string(dir.join("reads.log")).unwrap_or_default();
        log.lock()
            .unwrap()
            .push(reads.lines().filter(|l| l.starts_with("== ")).count());
    });
    let value = t
        .read_with_check(&Settings::new(), &*check, ProtectedRead::Info)
        .await
        .unwrap();
    // The desktop was read before the authority check.
    assert!(before.lock().unwrap()[0] > 0);
    assert_eq!(
        *check.seen.lock().unwrap(),
        [(AuthorityPhase::Read, Some(ProtectedRead::Info))]
    );
    let info: CoordinatorDesktopInfo = serde_json::from_value(value).unwrap();
    assert_eq!((info.station.as_str(), info.locked), ("", Some(false)));
    let names: Vec<&str> = info.monitors.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["DP-1", "HDMI-A-1"]);
    assert!(info.monitors.iter().all(|m| m.capture_allowed));
    assert_eq!(
        (info.monitors[0].width, info.monitors[0].scale_pct),
        (3440, 125)
    );
    // The fake desktop has `notify-send` and `chromium`, and no `grim`.
    let c = &info.capabilities;
    assert_eq!((c.notify, c.preview, c.screenshot), (true, true, false));
    assert!(info.walls.is_empty());
}

#[tokio::test]
async fn a_read_refusal_returns_no_data() {
    let t = setup().await;
    let s = Settings::new();
    for (fail, want) in [(lost(), "authority_lost"), (unknown(), "authority_unknown")] {
        let check = Check::failing(fail);
        let e = t.read_with_check(&s, &*check, ProtectedRead::Info).await;
        assert_eq!(e.unwrap_err().code, want);
    }
    // A gate that turns off during the check: refused after the await.
    let gate = Arc::clone(&s.0);
    let check = Check::new(Ok(()), move || gate.lock().unwrap().fleet_control = false);
    let e = t
        .read_with_check(&s, &*check, ProtectedRead::Info)
        .await
        .unwrap_err();
    assert_eq!(
        pair(&e),
        ("forbidden", "Fleet control is off on this station.")
    );
    // Every gate is read again for every kind of read.
    let s = Settings::new();
    s.set(|l| l.desktop = false);
    for r in [ProtectedRead::Info, status_read(OP), capture_read(CAPTURE)] {
        assert_eq!(t.rd(&s, r).await.unwrap_err().code, "forbidden");
    }
    // Capture reads also need `allow_desktop_capture`; the others do not.
    let s = Settings::new();
    s.set(|l| l.capture = false);
    assert!(t.rd(&s, status_read(OP)).await.is_ok());
    let e = t.rd(&s, capture_read(CAPTURE)).await.unwrap_err();
    assert_eq!(
        pair(&e),
        ("forbidden", "Desktop screenshots are off on this station.")
    );
}

#[tokio::test]
async fn the_action_status_has_its_four_states() {
    let t = setup().await;
    let s = Settings::new();
    let store = &t.s.store;
    assert_eq!(
        t.rd(&s, status_read(OP)).await.unwrap(),
        json!({"state": "unknown"})
    );
    store
        .desktop_op_claim(FP, OP, "h", "notify", &ctx(), 1)
        .await
        .unwrap();
    assert_eq!(
        t.rd(&s, status_read(OP)).await.unwrap(),
        json!({"state": "claimed"})
    );
    let done = Ok(Some(json!({"window_id": "w"})));
    store.desktop_op_finish(FP, OP, done, 2).await.unwrap();
    assert_eq!(
        t.rd(&s, status_read(OP)).await.unwrap(),
        json!({"state": "done", "result": {"window_id": "w"}})
    );
    let other = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    store
        .desktop_op_claim(FP, other, "h", "notify", &ctx(), 1)
        .await
        .unwrap();
    store
        .desktop_op_finish(FP, other, Err("forbidden"), 2)
        .await
        .unwrap();
    assert_eq!(
        t.rd(&s, status_read(other)).await.unwrap(),
        json!({"state": "failed", "code": "forbidden"})
    );
}

impl T {
    /// Commits one capture artifact with the given bytes and provenance.
    async fn commit(&self, capture_id: &str, coordinator: bool, bytes: &[u8]) -> String {
        let store = &self.s.store;
        let artifact_id = store.capture_artifact_id();
        let c = ctx();
        let by = coordinator.then_some(&c);
        store
            .capture_begin_with(FP, capture_id, "h", "DP-1", &artifact_id, by, 10)
            .await
            .unwrap();
        let dir = self.s.root.join(&artifact_id).join("1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("capture.png"), bytes).unwrap();
        let caller = if coordinator {
            CaptureCaller::Coordinator
        } else {
            CaptureCaller::Owner
        };
        let new = NewCaptureArtifact {
            artifact_id: artifact_id.clone(),
            title: "t".to_owned(),
            size: bytes.len() as u64,
            sha256: "ab".repeat(32),
            provenance: CaptureProvenance {
                capture_id: capture_id.to_owned(),
                station_id: "srv".to_owned(),
                monitor: "DP-1".to_owned(),
                kind: CaptureKind::Screenshot,
                caller,
                caller_station: by.map(|c| c.caller_station.to_string()),
                caller_session: by.map(|c| c.caller_session.to_string()),
                caller_grant_id: by.map(|c| c.grant_id.clone()),
                at: 1,
                recording: None,
            },
        };
        store.capture_commit(FP, capture_id, new).await.unwrap();
        artifact_id
    }
}

fn is_unknown_artifact(e: &Fail) -> bool {
    pair(e)
        == (
            "unknown_artifact",
            "No such capture artifact is available to this grant.",
        )
}

#[tokio::test]
async fn the_capture_status_answers_in_the_existing_shape() {
    let t = setup().await;
    let s = Settings::new();
    let want = CaptureStatus::Unknown {
        capture_id: CAPTURE.to_owned(),
    };
    let got = t.rd(&s, capture_read(CAPTURE)).await.unwrap();
    assert_eq!(got, serde_json::to_value(want).unwrap());
    let artifact = t.commit(CAPTURE, true, b"png").await;
    let got = t.rd(&s, capture_read(CAPTURE)).await.unwrap();
    assert_eq!(
        (got["state"].as_str(), got["capture_id"].as_str()),
        (Some("committed"), Some(CAPTURE))
    );
    assert_eq!(got["published"]["artifact"]["artifact_id"], artifact);
}

#[tokio::test]
async fn a_capture_file_is_one_whole_chunk() {
    let t = setup().await;
    let bytes = b"\x89PNG-not-checked-here".to_vec();
    let artifact = t.commit(CAPTURE, true, &bytes).await;
    let len = bytes.len() as u64;
    let check = Check::ok();
    let value = t
        .read_with_check(&Settings::new(), &*check, file_read(&artifact, 1, 0, len))
        .await
        .unwrap();
    let file: ArtifactFile = serde_json::from_value(value).unwrap();
    assert_eq!(
        (
            file.path.as_str(),
            file.size,
            file.offset,
            file.length,
            file.eof
        ),
        ("capture.png", len, 0, len, true)
    );
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.decode(&file.data_base64);
    assert_eq!(data.unwrap(), bytes);
    // The check carried the read that ran.
    assert_eq!(
        check.seen.lock().unwrap()[0].1,
        Some(file_read(&artifact, 1, 0, len))
    );
}

#[tokio::test]
async fn the_capture_file_refusals_have_their_codes() {
    let t = setup().await;
    let s = Settings::new();
    let bytes = vec![7_u8; 100];
    let artifact = t.commit(CAPTURE, true, &bytes).await;
    // An offset other than 0, and a length that is not the committed size.
    for (offset, length) in [(1, 100), (0, 99), (0, 101), (0, 0)] {
        let e = t
            .rd(&s, file_read(&artifact, 1, offset, length))
            .await
            .unwrap_err();
        assert_eq!(e.code, "invalid_params", "{offset} {length}");
    }
    // Another version, an unknown artifact.
    for (id, version) in [(artifact.as_str(), 2), ("01ARZ3NDEKTSV4RRFFQ69G5FAZ", 1)] {
        let e = t.rd(&s, file_read(id, version, 0, 100)).await.unwrap_err();
        assert!(is_unknown_artifact(&e));
    }
    // An owner capture is not readable by a coordinator.
    let owner = t.commit("01ARZ3NDEKTSV4RRFFQ69G5FB0", false, &bytes).await;
    assert!(is_unknown_artifact(
        &t.rd(&s, file_read(&owner, 1, 0, 100)).await.unwrap_err()
    ));
    // Not an image.
    let html = t.commit("01ARZ3NDEKTSV4RRFFQ69G5FB1", true, &bytes).await;
    let id = html.clone();
    let update = move |conn: &mut rusqlite::Connection| {
        conn.execute(
            "UPDATE artifacts SET kind = 'html' WHERE artifact_id = ?1",
            [&id],
        )?;
        Ok(())
    };
    t.s.store.db().call(update).await.unwrap();
    assert!(is_unknown_artifact(
        &t.rd(&s, file_read(&html, 1, 0, 100)).await.unwrap_err()
    ));
    // A tombstone: the same answer, nothing is read.
    let gone = ArtifactDeleteParams {
        artifact_id: artifact.clone(),
    };
    t.s.service.delete(gone).await.unwrap();
    assert!(is_unknown_artifact(
        &t.rd(&s, file_read(&artifact, 1, 0, 100)).await.unwrap_err()
    ));
}

#[tokio::test]
async fn the_cap_is_716800_bytes_and_one_byte_more_is_too_large() {
    let t = setup().await;
    let s = Settings::new();
    let cap = limits::IMAGE_MAX_BYTES;
    assert_eq!(cap, 716_800);
    let exact = t.commit(CAPTURE, true, &vec![1_u8; 716_800]).await;
    let value = t.rd(&s, file_read(&exact, 1, 0, cap)).await.unwrap();
    let file: ArtifactFile = serde_json::from_value(value).unwrap();
    assert_eq!(
        (file.size, file.length, file.offset, file.eof),
        (cap, cap, 0, true)
    );
    let over = t
        .commit("01ARZ3NDEKTSV4RRFFQ69G5FB2", true, &vec![1_u8; 716_801])
        .await;
    let e = t.rd(&s, file_read(&over, 1, 0, cap + 1)).await.unwrap_err();
    assert_eq!(
        pair(&e),
        (
            "image_too_large",
            "The screenshot exceeds the 700 KiB MCP image limit. Open the artifact on the board."
        )
    );
    // A length of the cap for a file of one byte more is not the size.
    let e = t.rd(&s, file_read(&over, 1, 0, cap)).await.unwrap_err();
    assert_eq!(e.code, "invalid_params");
}

#[tokio::test]
async fn a_monitor_that_leaves_the_allowlist_during_the_check_releases_no_data() {
    let t = setup().await;
    let bytes = vec![5_u8; 64];
    let artifact = t.commit(CAPTURE, true, &bytes).await;
    for read in [file_read(&artifact, 1, 0, 64), capture_read(CAPTURE)] {
        // The monitor is allowed when the work runs; the owner narrows the
        // list while the authority check is blocked.
        let s = Settings::new();
        s.set(|l| l.monitors = vec!["DP-1".to_owned()]);
        let shared = Arc::clone(&s.0);
        let check = Check::new(Ok(()), move || {
            shared.lock().unwrap().monitors = vec!["HDMI-A-1".to_owned()];
        });
        let e = t.read_with_check(&s, &*check, read).await.unwrap_err();
        assert_eq!(
            pair(&e),
            ("forbidden", "This monitor is not allowed for screenshots.")
        );
    }
    // An empty list allows every monitor; a list with the monitor too.
    for monitors in [vec![], vec!["DP-1".to_owned()]] {
        let s = Settings::new();
        s.set(|l| l.monitors = monitors);
        assert!(t.rd(&s, file_read(&artifact, 1, 0, 64)).await.is_ok());
        assert!(t.rd(&s, capture_read(CAPTURE)).await.is_ok());
    }
}
