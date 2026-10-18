//! The screenshot operation on a fake desktop (design §5.5, §6.5, §7.5): the
//! fake `grim` of the fake desktop, the `Direct` launch, the real store and
//! the real artifact tree.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_proto::capture::{code, text};
use fleetdeck_proto::desktop::DesktopActResult;

use super::{Allowed, Launch, Params, Request};
use crate::artifacts::ArtifactService;
use crate::artifacts::capture_import::tests::{FP, Recorder, Rig as StoreRig, rig as store_rig};
use crate::artifacts::capture_import::{DurableFs, SyncFs};
use crate::desktop::authority::Admitted;
use crate::desktop::deadline::Limits;
use crate::desktop::fake::{Opts, Rig as FakeRig};
use crate::desktop::{Fail, code as desktop_code};
use crate::store::Store;
use crate::store::capture_ops::{Begin, CaptureState};

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ID2: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

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

/// A fake `grim` in the bin folder of the fake desktop: it logs its argv,
/// prints `bytes` and then runs `extra`.
fn write_grim(dir: &Path, bytes: &[u8], extra: &str) {
    std::fs::write(dir.join("grim.out"), bytes).unwrap();
    let d = dir.display();
    let body = format!(
        "#!/bin/sh\n{{ for a in \"$@\"; do echo \"$a\"; done; }} >> {d}/grim.log\n\
         /bin/cat {d}/grim.out\n{extra}\n"
    );
    let path = dir.join("bin/grim");
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct T {
    fake: FakeRig,
    s: StoreRig,
    allowed: Arc<Mutex<Allowed>>,
}

fn open() -> Allowed {
    Allowed {
        desktop: true,
        capture: true,
        monitors: Vec::new(),
    }
}

async fn setup_with(fake: FakeRig) -> T {
    write_grim(fake.dir.path(), &png(), "");
    T {
        fake,
        s: store_rig().await,
        allowed: Arc::new(Mutex::new(open())),
    }
}

async fn setup() -> T {
    setup_with(FakeRig::new(Opts::default())).await
}

impl T {
    fn dir(&self) -> &Path {
        self.fake.dir.path()
    }

    fn service(&self) -> &ArtifactService {
        &self.s.service
    }

    fn store(&self) -> &Store {
        &self.s.store
    }

    fn request(&self, id: &str, monitor: &str, fs: Arc<dyn DurableFs>) -> Request {
        let allowed = Arc::clone(&self.allowed);
        Request {
            service: self.s.service.clone(),
            allowed: Arc::new(move || allowed.lock().unwrap().clone()),
            admitted: Admitted::open(),
            deck_fp: FP.to_owned(),
            capture_id: id.to_owned(),
            action_hash: format!("hash-{monitor}"),
            monitor: monitor.to_owned(),
            station_id: "srv".to_owned(),
            station_name: "srv".to_owned(),
            params: Params {
                launch: Launch::Direct,
                fs,
                admission: Duration::from_secs(5),
            },
        }
    }

    async fn run(&self, id: &str, monitor: &str) -> Result<DesktopActResult, Fail> {
        self.run_fs(id, monitor, Arc::new(SyncFs)).await
    }

    async fn run_fs(
        &self,
        id: &str,
        monitor: &str,
        fs: Arc<dyn DurableFs>,
    ) -> Result<DesktopActResult, Fail> {
        self.fake
            .desktop
            .screen_capture(self.request(id, monitor, fs))
            .await
    }

    /// How many times the fake `grim` ran.
    fn grim_runs(&self) -> usize {
        std::fs::read_to_string(self.dir().join("grim.log"))
            .unwrap_or_default()
            .lines()
            .count()
            / 9
    }

    async fn artifact_count(&self) -> i64 {
        self.store()
            .db()
            .call(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))?))
            .await
            .unwrap()
    }

    /// The staging folders and the artifact folders that exist.
    fn leftovers(&self) -> (Vec<String>, Vec<String>) {
        let names = |p: &Path| {
            std::fs::read_dir(p)
                .map(|d| {
                    d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        (names(&self.s.captures), names(&self.s.root))
    }

    fn set_monitors(&self, json: &str) {
        std::fs::write(self.dir().join("monitors.json"), json).unwrap();
    }
}

fn err(r: Result<DesktopActResult, Fail>) -> (&'static str, String) {
    let e = r.unwrap_err();
    (e.code, e.message)
}

/// Nothing ran: no grim, no row, no artifact, no folder.
async fn assert_untouched(t: &T, id: &str) {
    assert_eq!(t.grim_runs(), 0);
    assert!(t.store().capture_get(FP, id).await.unwrap().is_none());
    assert_eq!(t.artifact_count().await, 0);
    assert_eq!(t.leftovers(), (Vec::new(), Vec::new()));
}

/// A failed capture left no artifact, no folder and a `failed` row.
async fn assert_failed(t: &T, id: &str, want: &str) {
    let row = t.store().capture_get(FP, id).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Failed);
    assert_eq!(row.code.as_deref(), Some(want));
    assert_eq!(row.reserved_bytes, 0);
    assert_eq!(t.artifact_count().await, 0);
    assert_eq!(t.leftovers(), (Vec::new(), Vec::new()));
}

#[tokio::test]
async fn a_screenshot_becomes_one_artifact_with_its_provenance() {
    let t = setup().await;
    let done = t.run(ID, "DP-1").await.unwrap();
    assert_eq!(done.address, None);
    assert_eq!(done.info_fresh, None);
    assert!(done.info.monitors.iter().any(|m| m.name == "DP-1"));
    let result = done.capture.unwrap();
    assert_eq!(result.capture_id, ID);
    let published = result.published;
    assert!(published.created);
    assert_eq!(published.artifact.title, "Desktop capture \u{b7} srv/DP-1");
    assert_eq!(published.artifact.author, "owner");
    assert_eq!(published.artifact.session_id, None);
    assert_eq!(published.version.version, 1);
    assert_eq!(published.version.files, ["capture.png"]);
    let id = published.artifact.artifact_id.clone();
    let p = published.version.capture.clone().unwrap();
    assert_eq!((p.capture_id.as_str(), p.monitor.as_str()), (ID, "DP-1"));
    assert_eq!(p.station_id, "srv");
    assert!(p.at > 0);
    // The version file is the exact PNG.
    let bytes = std::fs::read(t.s.root.join(&id).join("1/capture.png")).unwrap();
    assert_eq!(bytes, png());
    assert_eq!(published.version.size, bytes.len() as u64);
    // The row, the staging, the argv.
    let row = t.store().capture_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Committed);
    assert_eq!(row.artifact_id, id);
    assert_eq!(
        row.sha256.as_deref(),
        Some(published.version.sha256.as_str())
    );
    assert_eq!(t.leftovers(), (Vec::new(), vec![id.clone()]));
    assert_eq!(t.artifact_count().await, 1);
    let log = std::fs::read_to_string(t.dir().join("grim.log")).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["-t", "png", "-s", "1", "-l", "6", "-o", "DP-1", "-"]
    );
    // The artifact reads back through the normal service.
    let detail = t.store().artifact_detail(id).await.unwrap().unwrap();
    assert_eq!(detail.1[0].capture, published.version.capture);
}

#[tokio::test]
async fn each_gate_refuses_before_anything_runs() {
    let t = setup().await;
    let off = (
        desktop_code::FORBIDDEN,
        "Desktop screenshots are off on this station.".to_owned(),
    );
    t.allowed.lock().unwrap().desktop = false;
    assert_eq!(err(t.run(ID, "DP-1").await), off);
    t.allowed.lock().unwrap().desktop = true;
    t.allowed.lock().unwrap().capture = false;
    assert_eq!(err(t.run(ID, "DP-1").await), off);
    t.allowed.lock().unwrap().capture = true;
    // The allowlist: a monitor that is not in it, and one that is.
    t.allowed.lock().unwrap().monitors = vec!["HDMI-A-1".to_owned()];
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (
            desktop_code::FORBIDDEN,
            "This monitor is not allowed for screenshots.".to_owned()
        )
    );
    assert_untouched(&t, ID).await;
    t.run(ID2, "HDMI-A-1").await.unwrap();
    assert_eq!(t.grim_runs(), 1);
}

#[tokio::test]
async fn a_gate_that_turns_off_during_the_run_stops_before_grim() {
    let t = setup().await;
    // A request whose gate is on at step 3 and off right before `grim`.
    let calls = Arc::new(Mutex::new(0_usize));
    let counter = Arc::clone(&calls);
    let mut req = t.request(ID, "DP-1", Arc::new(SyncFs));
    req.allowed = Arc::new(move || {
        let mut n = counter.lock().unwrap();
        *n += 1;
        Allowed {
            capture: *n == 1,
            ..open()
        }
    });
    let e = t.fake.desktop.screen_capture(req).await.unwrap_err();
    assert_eq!(e.code, desktop_code::FORBIDDEN);
    assert_eq!(t.grim_runs(), 0);
    assert_failed(&t, ID, desktop_code::FORBIDDEN).await;
}

#[tokio::test]
async fn a_missing_monitor_and_a_monitor_over_the_limits_write_no_row() {
    let t = setup().await;
    let (c, m) = err(t.run(ID, "DP-9").await);
    assert_eq!(c, desktop_code::NOT_FOUND);
    assert_eq!(m, "`monitor` is not a monitor of this desktop");
    assert_untouched(&t, ID).await;
    t.set_monitors(
        r#"[{"id":0,"name":"DP-1","description":"","width":9000,"height":100,
             "refreshRate":60.0,"scale":1.0,"focused":true,"dpmsStatus":true}]"#,
    );
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (
            code::CAPTURE_DIMENSIONS,
            text::CAPTURE_DIMENSIONS.to_owned()
        )
    );
    assert_untouched(&t, ID).await;
}

#[tokio::test]
async fn a_locked_or_unknown_lock_state_before_the_capture_refuses_it() {
    let locked = (code::SCREEN_LOCKED, text::SCREEN_LOCKED.to_owned());
    let t = setup().await;
    t.fake.flag("hyprlock", true);
    assert_eq!(err(t.run(ID, "DP-1").await), locked);
    assert_untouched(&t, ID).await;
    // No `pgrep`: the lock state is unknown.
    let t = setup_with(FakeRig::without(Opts::default(), &["pgrep"])).await;
    assert_eq!(err(t.run(ID, "DP-1").await), locked);
    assert_untouched(&t, ID).await;
}

#[tokio::test]
async fn a_screen_that_locks_during_the_capture_discards_the_output() {
    let t = setup().await;
    write_grim(
        t.dir(),
        &png(),
        &format!("/bin/touch {}/hyprlock", t.dir().display()),
    );
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (code::SCREEN_LOCKED, text::SCREEN_LOCKED.to_owned())
    );
    assert_eq!(t.grim_runs(), 1);
    assert_failed(&t, ID, code::SCREEN_LOCKED).await;
}

#[tokio::test]
async fn a_lock_state_that_becomes_unknown_during_the_capture_discards_the_output() {
    let t = setup().await;
    write_grim(
        t.dir(),
        &png(),
        &format!("/bin/rm {}/bin/pgrep", t.dir().display()),
    );
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (code::SCREEN_LOCKED, text::SCREEN_LOCKED.to_owned())
    );
    assert_failed(&t, ID, code::SCREEN_LOCKED).await;
}

#[tokio::test]
async fn a_monitor_that_disappears_during_the_capture_discards_the_output() {
    let t = setup().await;
    let json = r#"[{"id":1,"name":"HDMI-A-1","description":"","width":1920,"height":1080,
                    "refreshRate":60.0,"scale":1.0,"focused":false,"dpmsStatus":true}]"#;
    std::fs::write(t.dir().join("other.json"), json).unwrap();
    write_grim(
        t.dir(),
        &png(),
        &format!(
            "/bin/cp {0}/other.json {0}/monitors.json",
            t.dir().display()
        ),
    );
    assert_eq!(err(t.run(ID, "DP-1").await).0, desktop_code::NOT_FOUND);
    assert_failed(&t, ID, desktop_code::NOT_FOUND).await;
}

#[tokio::test]
async fn a_quota_that_only_the_full_reservation_crosses_refuses_the_capture() {
    let t = setup().await;
    // The recovery runs first: it would fail the `running` row below.
    t.service().ensure_recovered().await.unwrap();
    // Another capture holds a `running` reservation; the committed bytes
    // plus both reservations cross the quota, the committed bytes alone not.
    let other = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    assert_eq!(
        t.store()
            .capture_begin(FP, other, "h", "DP-1", "ARTX", 1)
            .await
            .unwrap(),
        Begin::Started
    );
    let committed = super::CAPTURE_QUOTA - 2 * super::MAX_PNG_BYTES + 1;
    t.store()
        .db()
        .call(move |conn| {
            conn.execute_batch(&format!(
                "INSERT INTO artifacts VALUES ('ARTC', NULL, 't', 'image', 1, 'owner', 1, 1);
                 INSERT INTO artifact_versions VALUES ('ARTC', 1, {committed}, 's', 'm', '[]', 1, '{{}}');"
            ))?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (code::CAPTURE_QUOTA, text::CAPTURE_QUOTA.to_owned())
    );
    assert_eq!(t.grim_runs(), 0);
    assert!(t.store().capture_get(FP, ID).await.unwrap().is_none());
    assert_eq!(t.leftovers(), (Vec::new(), Vec::new()));
}

#[tokio::test]
async fn a_second_capture_is_busy_and_the_slot_frees_after_the_first() {
    let t = setup().await;
    let guard = t.fake.desktop.capture.try_take().unwrap();
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (code::BUSY, text::BUSY.to_owned())
    );
    assert_untouched(&t, ID).await;
    drop(guard);
    t.run(ID, "DP-1").await.unwrap();
    // The slot is free again after a success and after a failure.
    t.fake.flag("hyprlock", true);
    assert_eq!(err(t.run(ID2, "DP-1").await).0, code::SCREEN_LOCKED);
    t.fake.flag("hyprlock", false);
    assert!(t.fake.desktop.capture.try_take().is_ok());
}

#[tokio::test]
async fn the_same_id_for_another_request_is_a_conflict() {
    let t = setup().await;
    t.run(ID, "DP-1").await.unwrap();
    assert_eq!(
        err(t.run(ID, "HDMI-A-1").await),
        (
            code::CAPTURE_ID_CONFLICT,
            text::CAPTURE_ID_CONFLICT.to_owned()
        )
    );
    assert_eq!(t.grim_runs(), 1);
    assert_eq!(t.artifact_count().await, 1);
}

#[tokio::test]
async fn a_repeated_request_answers_from_the_row_and_never_runs_grim_again() {
    let t = setup().await;
    let first = t.run(ID, "DP-1").await.unwrap().capture.unwrap();
    let again = t.run(ID, "DP-1").await.unwrap().capture.unwrap();
    assert_eq!(again.capture_id, first.capture_id);
    assert_eq!(again.published.artifact, first.published.artifact);
    assert_eq!(again.published.version, first.published.version);
    assert_eq!(t.grim_runs(), 1);
    assert_eq!(t.artifact_count().await, 1);

    // A failed request repeats its stored failure.
    // (The screen locks during the capture, so the row exists and fails.)
    write_grim(
        t.dir(),
        &png(),
        &format!("/bin/touch {}/hyprlock", t.dir().display()),
    );
    let failed = err(t.run(ID2, "DP-1").await);
    assert_eq!(failed.0, code::SCREEN_LOCKED);
    t.fake.flag("hyprlock", false);
    write_grim(t.dir(), &png(), "");
    assert_eq!(err(t.run(ID2, "DP-1").await), failed);
    assert_eq!(
        t.grim_runs(),
        2,
        "the failed request did not run grim again"
    );

    // A `running` row (not ours) answers busy; no capture starts.
    let third = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    t.store()
        .capture_begin(FP, third, "hash-DP-1", "DP-1", "ARTX", 1)
        .await
        .unwrap();
    assert_eq!(err(t.run(third, "DP-1").await).0, code::BUSY);
    assert_eq!(t.grim_runs(), 2);

    // A deleted screenshot answers not_found.
    let id = first.published.artifact.artifact_id;
    t.store().artifact_delete_begin(id).await.unwrap();
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (
            desktop_code::NOT_FOUND,
            "The screenshot was deleted.".to_owned()
        )
    );
    assert_eq!(t.grim_runs(), 2);
}

#[tokio::test]
async fn an_interrupted_row_fails_at_the_recovery_and_no_second_capture_runs() {
    let t = setup().await;
    assert_eq!(
        t.store()
            .capture_begin(FP, ID, "hash-DP-1", "DP-1", "ART1", 1)
            .await
            .unwrap(),
        Begin::Started
    );
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (
            code::CAPTURE_INTERRUPTED,
            text::CAPTURE_INTERRUPTED.to_owned()
        )
    );
    assert_eq!(t.grim_runs(), 0);
    assert_eq!(t.artifact_count().await, 0);
}

#[tokio::test]
async fn a_failed_sync_is_capture_publish_failed_and_leaves_no_committed_row() {
    for at in 0..5 {
        let t = setup().await;
        let fs = Arc::new(Recorder::failing_at(at));
        assert_eq!(
            err(t.run_fs(ID, "DP-1", fs).await),
            (
                code::CAPTURE_PUBLISH_FAILED,
                text::CAPTURE_PUBLISH_FAILED.to_owned()
            ),
            "sync {at}"
        );
        assert_failed(&t, ID, code::CAPTURE_PUBLISH_FAILED).await;
    }
}

#[tokio::test]
async fn after_the_commit_no_unsynced_entry_can_take_the_png_away() {
    let t = setup().await;
    let rec = Arc::new(Recorder::default());
    let done = t
        .run_fs(ID, "DP-1", Arc::clone(&rec) as Arc<dyn DurableFs>)
        .await;
    let id = done
        .unwrap()
        .capture
        .unwrap()
        .published
        .artifact
        .artifact_id;
    assert_eq!(
        rec.count(),
        7,
        "the PNG, 1/, the id folder, artifacts/, data; then the durable staging \
         removal: the staging folder and captures/"
    );
    // The fault hook: drop everything that was not synced.
    rec.drop_unsynced(&t.s.data);
    assert_eq!(
        std::fs::read(t.s.root.join(&id).join("1/capture.png")).unwrap(),
        png()
    );
}

/// A `DurableFs` that breaks the fake `hyprctl` answer at the last sync.
struct Breaker {
    monitors: PathBuf,
}

impl DurableFs for Breaker {
    fn sync_file(&self, _: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }

    fn sync_dir(&self, _: &OwnedFd) -> std::io::Result<()> {
        std::fs::write(&self.monitors, "not json")
    }
}

#[tokio::test]
async fn a_failed_refresh_after_the_commit_keeps_the_artifact_and_marks_the_info_stale() {
    let t = setup().await;
    let fs = Arc::new(Breaker {
        monitors: t.dir().join("monitors.json"),
    });
    let done = t.run_fs(ID, "DP-1", fs).await.unwrap();
    assert_eq!(done.info_fresh, Some(false));
    assert!(done.info.monitors.iter().any(|m| m.name == "DP-1"));
    assert!(done.capture.is_some());
    assert_eq!(t.artifact_count().await, 1);
    let row = t.store().capture_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Committed);
}

#[tokio::test]
async fn a_stalled_grim_fails_the_capture_at_the_deadline_and_cleans_up() {
    let t = setup().await;
    write_grim(t.dir(), &png(), "");
    // Replace the output with a stall.
    std::fs::write(
        t.dir().join("bin/grim"),
        format!(
            "#!/bin/sh\n{{ for a in \"$@\"; do echo \"$a\"; done; }} >> {}/grim.log\nexec /bin/sleep 30\n",
            t.dir().display()
        ),
    )
    .unwrap();
    t.fake.desktop.set_limits(Limits {
        long: Duration::from_secs(5),
        short: Duration::from_secs(3),
    });
    let e = t.run(ID, "DP-1").await.unwrap_err();
    assert_eq!(e.code, code::CAPTURE_FAILED);
    assert_failed(&t, ID, code::CAPTURE_FAILED).await;
}

#[tokio::test]
async fn a_missing_grim_is_unsupported_and_the_row_fails() {
    let t = setup().await;
    std::fs::remove_file(t.dir().join("bin/grim")).unwrap();
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (desktop_code::UNSUPPORTED, text::NO_GRIM.to_owned())
    );
    assert_failed(&t, ID, desktop_code::UNSUPPORTED).await;
}

#[tokio::test]
async fn bad_output_is_capture_failed_and_leaves_nothing() {
    let t = setup().await;
    write_grim(t.dir(), b"this is not a png", "");
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (code::CAPTURE_FAILED, text::CAPTURE_FAILED.to_owned())
    );
    assert_failed(&t, ID, code::CAPTURE_FAILED).await;
}

#[tokio::test]
async fn every_request_makes_at_most_one_artifact() {
    let t = setup().await;
    for _ in 0..3 {
        let _ = t.run(ID, "DP-1").await;
    }
    let _ = t.run(ID, "HDMI-A-1").await;
    assert_eq!(t.artifact_count().await, 1);
    assert_eq!(t.grim_runs(), 1);
}

#[tokio::test]
async fn a_failed_cleanup_keeps_the_reservation_and_the_quota_counts_it() {
    let t = setup().await;
    t.service().ensure_recovered().await.unwrap();
    // The artifacts root is a file: the folder removal refuses it.
    std::fs::write(&t.s.root, b"not a folder").unwrap();
    write_grim(t.dir(), b"this is not a png", "");
    assert_eq!(
        err(t.run(ID, "DP-1").await),
        (code::CAPTURE_FAILED, text::CAPTURE_FAILED.to_owned())
    );
    let row = t.store().capture_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Failed);
    assert_eq!(row.code.as_deref(), Some(code::CAPTURE_FAILED));
    assert_eq!(row.reserved_bytes, super::MAX_PNG_BYTES);
    assert_eq!(
        t.store().captures_to_recover().await.unwrap().len(),
        1,
        "the next start retries it"
    );
    // The committed bytes alone leave room for one more capture; with the
    // reservation of the failed row they cross the quota.
    let committed = super::CAPTURE_QUOTA - 2 * super::MAX_PNG_BYTES + 1;
    t.store()
        .db()
        .call(move |conn| {
            conn.execute_batch(&format!(
                "INSERT INTO artifacts VALUES ('ARTC', NULL, 't', 'image', 1, 'owner', 1, 1);
                 INSERT INTO artifact_versions VALUES ('ARTC', 1, {committed}, 's', 'm', '[]', 1, '{{}}');"
            ))?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        err(t.run(ID2, "DP-1").await),
        (code::CAPTURE_QUOTA, text::CAPTURE_QUOTA.to_owned())
    );
}
