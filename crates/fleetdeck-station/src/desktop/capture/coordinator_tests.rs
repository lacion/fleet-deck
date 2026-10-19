//! The coordinator screenshot (R13 P6 §4.6, §4.7, §6.3): the fixed
//! disclosure before `grim`, the authority checks, the refusal before the
//! import, and the provenance. The deck answer is a fake; the desktop, `grim`
//! and `notify-send` are the fakes of the owner screenshot.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::CaptureCaller;
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, CoordinatorContext, ProtectedRead,
};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};

use super::{Allowed, Launch, Params, Request, coordinator};
use crate::artifacts::capture_import::SyncFs;
use crate::artifacts::capture_import::tests::{FP, Rig as StoreRig, rig as store_rig};
use crate::desktop::authority::Admitted;
use crate::desktop::coordinator::{DispatchCheck, Gates, fleet_off};
use crate::desktop::coordinator_tests::{FakeCheck, ctx, extras, open_gates};
use crate::desktop::fake::{Opts, Rig as FakeRig};
use crate::desktop::{Fail, code};
use crate::store::capture_ops::CaptureState;

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const NODE: &str = "aa11";
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// A valid 2x2 PNG.
fn png() -> Vec<u8> {
    let mut ihdr = 2_u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&2_u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = SIGNATURE.to_vec();
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &[1, 2, 3, 4, 5, 6, 7]));
    out.extend(chunk(b"IEND", &[]));
    out
}

/// A fake `grim` in the bin folder of the fake desktop.
fn write_grim(dir: &Path, bytes: &[u8]) {
    std::fs::write(dir.join("grim.out"), bytes).unwrap();
    let d = dir.display();
    let body = format!(
        "#!/bin/sh\n{{ for a in \"$@\"; do echo \"$a\"; done; }} >> {d}/grim.log\n\
         /bin/cat {d}/grim.out\n"
    );
    let path = dir.join("bin/grim");
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct T {
    fake: FakeRig,
    s: StoreRig,
}

async fn setup() -> T {
    let fake = FakeRig::new(Opts::default());
    write_grim(fake.dir.path(), &png());
    T {
        fake,
        s: store_rig().await,
    }
}

/// The count of the lines that start a program in a log of the fakes.
fn programs(dir: &Path, name: &str) -> usize {
    std::fs::read_to_string(dir.join(name))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("== "))
        .count()
}

impl T {
    fn dir(&self) -> &Path {
        self.fake.dir.path()
    }

    fn grim_runs(&self) -> usize {
        std::fs::read_to_string(self.dir().join("grim.log"))
            .unwrap_or_default()
            .lines()
            .count()
            / 9
    }

    fn request(&self, admitted: Admitted) -> Request {
        Request {
            service: self.s.service.clone(),
            allowed: Arc::new(|| Allowed {
                desktop: true,
                capture: true,
                monitors: Vec::new(),
                quota_bytes: super::CAPTURE_QUOTA,
                free_reserve_bytes: super::FREE_RESERVE,
            }),
            admitted,
            deck_fp: FP.to_owned(),
            capture_id: ID.to_owned(),
            action_hash: "hash".to_owned(),
            monitor: "DP-1".to_owned(),
            station_id: "srv".to_owned(),
            station_name: "srv".to_owned(),
            params: Params {
                launch: Launch::Direct,
                fs: Arc::new(SyncFs),
                admission: Duration::from_secs(5),
            },
        }
    }

    fn coordinated(
        &self,
        context: CoordinatorContext,
        remote: Arc<dyn DispatchCheck>,
        gates: Gates,
    ) -> Admitted {
        let action = DesktopAction::ScreenCapture {
            monitor: "DP-1".to_owned(),
        };
        Admitted::open_coordinated(&action, NODE, context, remote, gates, extras())
    }

    async fn run(&self, admitted: Admitted) -> Result<DesktopActResult, Fail> {
        self.fake
            .desktop
            .screen_capture(self.request(admitted))
            .await
    }

    async fn artifact_count(&self) -> i64 {
        self.s
            .store
            .db()
            .call(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))?))
            .await
            .unwrap()
    }

    async fn column(&self) -> Option<String> {
        self.s
            .store
            .db()
            .call(|conn| {
                Ok(conn.query_row(
                    "SELECT coordinator FROM capture_ops WHERE capture_id = ?1",
                    [ID],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    }

    /// Nothing is left of a failed capture and its row says why.
    async fn assert_failed(&self, want: &str) {
        let row = self.s.store.capture_get(FP, ID).await.unwrap().unwrap();
        assert_eq!(row.state, CaptureState::Failed);
        assert_eq!(row.code.as_deref(), Some(want));
        assert_eq!(row.reserved_bytes, 0);
        assert_eq!(self.artifact_count().await, 0);
        let names = |p: &Path| std::fs::read_dir(p).map_or(0, Iterator::count);
        assert_eq!((names(&self.s.captures), names(&self.s.root)), (0, 0));
    }
}

/// Answers `execute` and `import` apart.
struct PhaseCheck {
    import: Result<(), Fail>,
    seen: Mutex<Vec<AuthorityPhase>>,
}

#[async_trait]
impl DispatchCheck for PhaseCheck {
    async fn check(
        &self,
        phase: AuthorityPhase,
        _read: Option<&ProtectedRead>,
    ) -> Result<(), Fail> {
        self.seen.lock().unwrap().push(phase);
        match phase {
            AuthorityPhase::Import => self.import.clone(),
            _ => Ok(()),
        }
    }
}

#[test]
fn the_disclosure_names_the_monitor_and_the_label() {
    assert_eq!(
        coordinator::disclosure("DP-1", &ctx()),
        "Fleet Deck is taking a screenshot of DP-1 for bold-fox."
    );
}

#[tokio::test]
async fn the_disclosure_is_exact_and_comes_before_the_check_and_grim() {
    let t = setup().await;
    let dir = t.dir().to_owned();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let check = FakeCheck::with_hook(Ok(()), move || {
        log.lock().unwrap().push((
            programs(&dir, "calls.log"),
            std::fs::read_to_string(dir.join("grim.log")).map_or(0, |t| t.lines().count()),
        ));
    });
    let done = t
        .run(t.coordinated(ctx(), check.clone(), open_gates()))
        .await
        .unwrap();
    assert!(done.capture.is_some());
    assert_eq!(
        check.phases(),
        [AuthorityPhase::Execute, AuthorityPhase::Import]
    );
    // At the execute check the disclosure was sent and `grim` had not run.
    assert_eq!(seen.lock().unwrap()[0], (1, 0));
    let notes = t.fake.calls_of("notify-send");
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].args,
        [
            "--app-name=Fleet Deck",
            "--urgency=normal",
            "--",
            "Fleet Deck",
            "Fleet Deck is taking a screenshot of DP-1 for bold-fox."
        ]
    );
    assert_eq!(t.grim_runs(), 1);
}

#[tokio::test]
async fn a_failed_disclosure_starts_no_grim_and_fails_the_capture() {
    let t = setup().await;
    t.fake.fail("notify-send");
    let check = FakeCheck::ok();
    let e = t
        .run(t.coordinated(ctx(), check.clone(), open_gates()))
        .await
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("failed", "`notify-send` failed with exit code 2")
    );
    assert_eq!(t.grim_runs(), 0);
    // The authority check never ran: the disclosure comes first.
    assert!(check.phases().is_empty());
    t.assert_failed("failed").await;
}

#[tokio::test]
async fn a_refusal_before_the_import_discards_the_file_and_records_the_code() {
    for (fail, want) in [
        (crate::desktop::coordinator::lost(), "authority_lost"),
        (crate::desktop::coordinator::unknown(), "authority_unknown"),
    ] {
        let t = setup().await;
        let check = Arc::new(PhaseCheck {
            import: Err(fail),
            seen: Mutex::new(Vec::new()),
        });
        let e = t
            .run(t.coordinated(ctx(), check.clone(), open_gates()))
            .await
            .unwrap_err();
        assert_eq!(e.code, want);
        assert_eq!(
            *check.seen.lock().unwrap(),
            [AuthorityPhase::Execute, AuthorityPhase::Import]
        );
        // The pixels were taken, then discarded.
        assert_eq!(t.grim_runs(), 1);
        t.assert_failed(want).await;
        // The same capture ID again repeats the stored failure; no grim runs.
        let again = t
            .run(t.coordinated(ctx(), FakeCheck::ok(), open_gates()))
            .await
            .unwrap_err();
        assert_eq!(again.code, want);
        assert_eq!(t.grim_runs(), 1);
    }
}

#[tokio::test]
async fn a_withdrawn_authority_at_the_execute_check_runs_no_grim() {
    let t = setup().await;
    let e = t
        .run(t.coordinated(
            ctx(),
            FakeCheck::answering(Err(crate::desktop::coordinator::lost())),
            open_gates(),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, "authority_lost");
    assert_eq!(t.grim_runs(), 0);
    t.assert_failed("authority_lost").await;
}

#[tokio::test]
async fn a_gate_that_is_off_runs_neither_the_disclosure_nor_grim() {
    let t = setup().await;
    let gates: Gates = Arc::new(|| Err(fleet_off()));
    let e = t
        .run(t.coordinated(ctx(), FakeCheck::ok(), gates))
        .await
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("forbidden", "Fleet control is off on this station.")
    );
    assert!(t.fake.calls().is_empty());
    assert_eq!(t.grim_runs(), 0);
    // Refused at the first read of the gates: no row exists.
    assert!(t.s.store.capture_get(FP, ID).await.unwrap().is_none());
    // A gate that turns off after the first read stops before the disclosure.
    let t = setup().await;
    let first = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&first);
    let gates: Gates = Arc::new(move || {
        if flag.swap(false, Ordering::SeqCst) {
            Ok(())
        } else {
            Err(fleet_off())
        }
    });
    let e = t
        .run(t.coordinated(ctx(), FakeCheck::ok(), gates))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::FORBIDDEN);
    assert!(t.fake.calls().is_empty());
    assert_eq!(t.grim_runs(), 0);
    t.assert_failed("forbidden").await;
}

#[tokio::test]
async fn the_coordinator_provenance_and_the_context_are_stored() {
    let t = setup().await;
    let done = t
        .run(t.coordinated(ctx(), FakeCheck::ok(), open_gates()))
        .await
        .unwrap();
    let published = done.capture.unwrap().published;
    let p = published.version.capture.clone().unwrap();
    assert_eq!(p.caller, CaptureCaller::Coordinator);
    assert_eq!(
        (
            p.caller_station.as_deref(),
            p.caller_session.as_deref(),
            p.caller_grant_id.as_deref()
        ),
        (
            Some("01HZZZZZZZZZZZZZZZZZZZZZZA"),
            Some("01HZZZZZZZZZZZZZZZZZZZZZZB"),
            Some("grant-1")
        )
    );
    assert_eq!((p.capture_id.as_str(), p.monitor.as_str()), (ID, "DP-1"));
    // The author has the first 16 characters of the station and the session.
    assert_eq!(
        published.artifact.author,
        "coordinator:01HZZZZZZZZZZZZZ/01HZZZZZZZZZZZZZZZZZZZZZZB"
    );
    assert_eq!(published.artifact.session_id, None);
    // The stored artifact has the same author and provenance.
    let detail =
        t.s.store
            .artifact_detail(published.artifact.artifact_id.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(detail.0.author, published.artifact.author);
    assert_eq!(detail.1[0].capture, Some(p));
    // The private operation record keeps the context with the generation.
    let stored: CoordinatorContext = serde_json::from_str(&t.column().await.unwrap()).unwrap();
    assert_eq!(stored, ctx());
    assert_eq!(stored.generation, 3);
}

#[tokio::test]
async fn a_long_author_is_cut_to_the_64_character_limit() {
    let t = setup().await;
    let context = CoordinatorContext {
        caller_session: fleetdeck_proto::SessionId::new("S".repeat(60)),
        caller_station: StationId::new("T".repeat(40)),
        ..ctx()
    };
    let done = t
        .run(t.coordinated(context.clone(), FakeCheck::ok(), open_gates()))
        .await
        .unwrap();
    let author = done.capture.unwrap().published.artifact.author;
    assert_eq!(author.chars().count(), 64);
    assert!(author.starts_with(&format!("coordinator:{}/", "T".repeat(16))));
    // The full IDs stay in the provenance.
    let version = t.s.store.capture_lookup(FP, ID).await.unwrap();
    let crate::store::capture_ops::Lookup::Committed { version, .. } = version else {
        panic!("a committed capture expected");
    };
    let p = version.capture.unwrap();
    assert_eq!(p.caller_station, Some("T".repeat(40)));
    assert_eq!(p.caller_session, Some("S".repeat(60)));
}

#[tokio::test]
async fn the_owner_screenshot_is_unchanged() {
    let t = setup().await;
    let done = t.run(Admitted::open()).await.unwrap();
    let published = done.capture.unwrap().published;
    assert_eq!(published.artifact.author, "owner");
    let p = published.version.capture.unwrap();
    assert_eq!(p.caller, CaptureCaller::Owner);
    assert_eq!(
        (p.caller_station, p.caller_session, p.caller_grant_id),
        (None, None, None)
    );
    assert_eq!(t.column().await, None);
    // No disclosure, one grim run.
    assert!(t.fake.calls_of("notify-send").is_empty());
    assert_eq!(t.grim_runs(), 1);
}
