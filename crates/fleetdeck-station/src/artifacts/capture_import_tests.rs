//! The import of a screenshot and the startup recovery (R13 P5a task D2,
//! design §6.2, §6.5): the sync order, the fault hook that drops what was
//! not synced, a failed sync, the refusals, and the recovery of a crash
//! before and after the file move and the commit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance, code, text};
use fleetdeck_testkit::Det;

use super::{DurableFs, SyncFs, discard, import_png};
use crate::artifacts::ArtifactService;
use crate::desktop::capture::Staging;
use crate::manager::policy::Policy;
use crate::manager::policy_hub::PolicyHub;
use crate::store::capture_ops::{Begin, CaptureState, NewCaptureArtifact};
use crate::store::{OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::PrivatePaths;

pub(crate) const FP: &str = "ab00000000000000000000000000000000000000000000000000000000000001";
const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

/// A `DurableFs` that records every call (the inode of the synced file or
/// directory), can fail or skip one call, and can drop what was not synced
/// (the fault hook of design §6.2).
#[derive(Default)]
pub(crate) struct Recorder {
    calls: Mutex<Vec<(bool, u64)>>,
    fail_at: Option<usize>,
    skip_at: Option<usize>,
}

impl Recorder {
    pub(crate) fn failing_at(n: usize) -> Self {
        Self {
            fail_at: Some(n),
            ..Self::default()
        }
    }

    pub(crate) fn skipping(n: usize) -> Self {
        Self {
            skip_at: Some(n),
            ..Self::default()
        }
    }

    fn note(&self, is_dir: bool, ino: u64) -> std::io::Result<()> {
        let mut calls = self.calls.lock().unwrap();
        let n = calls.len();
        calls.push((is_dir, ino));
        if self.fail_at == Some(n) {
            return Err(std::io::Error::other("a sync failed"));
        }
        Ok(())
    }

    /// The number of sync calls.
    pub(crate) fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// The synced paths, in order, as the paths of `known` that have the
    /// same inode.
    pub(crate) fn order(&self, known: &[&Path]) -> Vec<PathBuf> {
        let calls = self.calls.lock().unwrap().clone();
        calls
            .iter()
            .map(|(_, ino)| {
                known
                    .iter()
                    .find(|p| std::fs::metadata(p).unwrap().ino() == *ino)
                    .map_or_else(|| PathBuf::from("?"), |p| (*p).to_path_buf())
            })
            .collect()
    }

    /// The fault hook: below `top`, drops every directory entry whose
    /// directory was not synced and every file that was not synced. `top`
    /// itself is taken as existing.
    pub(crate) fn drop_unsynced(&self, top: &Path) {
        let calls = self.calls.lock().unwrap().clone();
        let mut dirs = HashSet::new();
        let mut files = HashSet::new();
        for (n, (is_dir, ino)) in calls.iter().enumerate() {
            if self.skip_at == Some(n) {
                continue;
            }
            if *is_dir {
                dirs.insert(*ino);
            } else {
                files.insert(*ino);
            }
        }
        walk(top, &dirs, &files);
    }
}

fn walk(dir: &Path, dirs: &HashSet<u64>, files: &HashSet<u64>) {
    let synced = dirs.contains(&std::fs::metadata(dir).unwrap().ino());
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if !synced {
            if meta.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        } else if meta.is_dir() {
            walk(&path, dirs, files);
        } else if !files.contains(&meta.ino()) {
            std::fs::remove_file(&path).unwrap();
        }
    }
}

impl DurableFs for Recorder {
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        self.note(false, f.metadata()?.ino())
    }

    fn sync_dir(&self, d: &OwnedFd) -> std::io::Result<()> {
        let ino = rustix::fs::fstat(d).map_err(std::io::Error::from)?.st_ino;
        self.note(true, ino)
    }
}

fn source(bytes: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file
}

/// `<tmp>/data` with `<tmp>/data/artifacts` when `with_root`.
fn layout(with_root: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().canonicalize().unwrap().join("data");
    let root = data.join("artifacts");
    std::fs::create_dir_all(if with_root { &root } else { &data }).unwrap();
    (tmp, data, root)
}

fn fail_of(r: Result<(), crate::desktop::Fail>) -> (&'static str, String) {
    let e = r.unwrap_err();
    (e.code, e.message)
}

fn publish_failed() -> (&'static str, String) {
    (
        code::CAPTURE_PUBLISH_FAILED,
        text::CAPTURE_PUBLISH_FAILED.to_owned(),
    )
}

#[test]
fn the_import_copies_the_bytes_with_private_modes_and_syncs_in_the_contract_order() {
    let (_tmp, data, root) = layout(true);
    let bytes: Vec<u8> = (0..200_000_u32).map(|n| (n % 251) as u8).collect();
    let rec = Recorder::default();
    import_png(&root, ID, &mut source(&bytes), bytes.len() as u64, &rec).unwrap();
    let dir = root.join(ID);
    assert_eq!(std::fs::read(dir.join("1/capture.png")).unwrap(), bytes);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("1")), 0o700);
    assert_eq!(mode(&dir.join("1/capture.png")), 0o600);
    // The PNG, 1/, <id>/, artifacts/ — and not <data>/: artifacts/ existed.
    let known = [
        &dir.join("1/capture.png"),
        &dir.join("1"),
        &dir,
        &root,
        &data,
    ];
    let known: Vec<&Path> = known.iter().map(|p| p.as_path()).collect();
    assert_eq!(
        rec.order(&known),
        [
            dir.join("1/capture.png"),
            dir.join("1"),
            dir.clone(),
            root.clone()
        ]
    );
}

#[test]
fn a_new_artifacts_folder_also_syncs_the_station_data_folder() {
    let (_tmp, data, root) = layout(false);
    let rec = Recorder::default();
    import_png(&root, ID, &mut source(b"png"), 3, &rec).unwrap();
    let dir = root.join(ID);
    let known = [
        dir.join("1/capture.png"),
        dir.join("1"),
        dir.clone(),
        root.clone(),
        data.clone(),
    ];
    let known: Vec<&Path> = known.iter().map(PathBuf::as_path).collect();
    let order = rec.order(&known);
    assert_eq!(order.len(), 5);
    assert_eq!(order[3], root);
    assert_eq!(order[4], data);
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn the_fault_hook_keeps_the_png_when_every_sync_ran() {
    for with_root in [true, false] {
        let (_tmp, data, root) = layout(with_root);
        let rec = Recorder::default();
        import_png(&root, ID, &mut source(b"the png"), 7, &rec).unwrap();
        rec.drop_unsynced(if with_root { &root } else { &data });
        assert_eq!(
            std::fs::read(root.join(ID).join("1/capture.png")).unwrap(),
            b"the png"
        );
    }
}

#[test]
fn the_fault_hook_drops_the_png_when_one_sync_is_missing() {
    // Skip each of the four syncs of an existing `artifacts/` in turn.
    for skip in 0..4 {
        let (_tmp, _data, root) = layout(true);
        let rec = Recorder::skipping(skip);
        import_png(&root, ID, &mut source(b"the png"), 7, &rec).unwrap();
        rec.drop_unsynced(&root);
        assert!(
            !root.join(ID).join("1/capture.png").exists(),
            "the skipped sync {skip} must lose the PNG"
        );
    }
    // The sync of `<data>/` matters when this import made `artifacts/`.
    let (_tmp, data, root) = layout(false);
    let rec = Recorder::skipping(4);
    import_png(&root, ID, &mut source(b"the png"), 7, &rec).unwrap();
    rec.drop_unsynced(&data);
    assert!(!root.exists());
}

#[test]
fn a_failed_sync_is_a_failed_import_and_discard_removes_the_partial_folder() {
    for at in 0..4 {
        let (_tmp, _data, root) = layout(true);
        let rec = Recorder::failing_at(at);
        let r = import_png(&root, ID, &mut source(b"png"), 3, &rec);
        assert_eq!(fail_of(r), publish_failed(), "sync {at}");
        assert_eq!(rec.count(), at + 1, "no sync runs after the failed one");
        discard(&root, ID);
        assert!(!root.join(ID).exists());
        assert!(root.exists());
    }
}

#[test]
fn the_import_refuses_an_existing_folder_a_linked_root_and_a_short_source() {
    let (_tmp, data, root) = layout(true);
    std::fs::create_dir(root.join(ID)).unwrap();
    std::fs::write(root.join(ID).join("keep"), b"x").unwrap();
    let r = import_png(&root, ID, &mut source(b"png"), 3, &SyncFs);
    assert_eq!(fail_of(r), publish_failed());
    assert!(
        root.join(ID).join("keep").exists(),
        "an existing folder stays"
    );

    let linked = data.join("linked");
    symlink(&root, &linked).unwrap();
    let r = import_png(
        &linked,
        "01ARZ3NDEKTSV4RRFFQ69G5FAW",
        &mut source(b"png"),
        3,
        &SyncFs,
    );
    assert_eq!(fail_of(r), publish_failed());
    assert!(!root.join("01ARZ3NDEKTSV4RRFFQ69G5FAW").exists());

    // Fewer bytes than announced.
    let r = import_png(
        &root,
        "01ARZ3NDEKTSV4RRFFQ69G5FAX",
        &mut source(b"ab"),
        3,
        &SyncFs,
    );
    assert_eq!(fail_of(r), publish_failed());

    for bad in ["", "..", "a/b"] {
        let r = import_png(&root, bad, &mut source(b"png"), 3, &SyncFs);
        assert_eq!(fail_of(r), publish_failed(), "{bad:?}");
    }
}

// ----- the recovery -----

pub(crate) struct Rig {
    pub(crate) _tmp: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) service: ArtifactService,
    pub(crate) data: PathBuf,
    pub(crate) root: PathBuf,
    pub(crate) captures: PathBuf,
}

pub(crate) async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    let policy = Arc::new(PolicyHub::new(Policy::new(
        vec![base.clone()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )));
    let root = data.join("artifacts");
    let service = ArtifactService::new(
        store.clone(),
        policy,
        root.clone(),
        Arc::new(PrivatePaths::default()),
    );
    let captures = service.captures_dir();
    Rig {
        _tmp: tmp,
        store,
        service,
        data,
        root,
        captures,
    }
}

fn provenance() -> CaptureProvenance {
    CaptureProvenance {
        capture_id: ID.to_owned(),
        station_id: "srv".to_owned(),
        monitor: "DP-1".to_owned(),
        kind: CaptureKind::Screenshot,
        caller: CaptureCaller::Owner,
        caller_station: None,
        at: 1,
    }
}

async fn start(r: &Rig, id: &str, artifact: &str) {
    let begin = r
        .store
        .capture_begin(FP, id, "h", "DP-1", artifact, 5)
        .await
        .unwrap();
    assert_eq!(begin, Begin::Started);
}

#[tokio::test]
async fn the_captures_folder_is_next_to_the_artifacts_root() {
    let r = rig().await;
    assert_eq!(r.captures, r.data.join("captures"));
}

#[tokio::test]
async fn a_crash_before_the_file_move_fails_the_row_and_removes_the_staging() {
    let r = rig().await;
    start(&r, ID, "ART1").await;
    let staging = Staging::create(&r.captures, ID).unwrap();
    drop(staging);
    assert!(r.captures.join(ID).exists());
    r.service.ensure_recovered().await.unwrap();
    let row = r.store.capture_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Failed);
    assert_eq!(row.code.as_deref(), Some(code::CAPTURE_INTERRUPTED));
    assert_eq!(row.message.as_deref(), Some(text::CAPTURE_INTERRUPTED));
    assert_eq!(row.reserved_bytes, 0);
    assert!(!r.captures.join(ID).exists());
    assert!(
        r.store
            .artifact_detail("ART1".to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_crash_after_the_file_move_before_the_commit_removes_the_folder() {
    let r = rig().await;
    start(&r, ID, "ART1").await;
    import_png(&r.root, "ART1", &mut source(b"png"), 3, &SyncFs).unwrap();
    // An ordinary folder with no version row is not swept.
    std::fs::create_dir_all(r.root.join("ORDINARY/1")).unwrap();
    std::fs::write(r.root.join("ORDINARY/1/a.html"), b"x").unwrap();
    r.service.ensure_recovered().await.unwrap();
    assert!(!r.root.join("ART1").exists());
    assert!(r.root.join("ORDINARY/1/a.html").exists());
    let row = r.store.capture_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Failed);
    assert_eq!(row.code.as_deref(), Some(code::CAPTURE_INTERRUPTED));
}

#[tokio::test]
async fn a_crash_after_the_commit_loses_nothing_and_a_repeat_never_runs_again() {
    let r = rig().await;
    start(&r, ID, "ART1").await;
    import_png(&r.root, "ART1", &mut source(b"png"), 3, &SyncFs).unwrap();
    r.store
        .capture_commit(
            FP,
            ID,
            NewCaptureArtifact {
                artifact_id: "ART1".to_owned(),
                title: "t".to_owned(),
                size: 3,
                sha256: "ab".repeat(32),
                provenance: provenance(),
            },
        )
        .await
        .unwrap();
    // The staging of the same capture was not removed before the crash.
    drop(Staging::create(&r.captures, ID).unwrap());
    r.service.ensure_recovered().await.unwrap();
    assert_eq!(
        std::fs::read(r.root.join("ART1/1/capture.png")).unwrap(),
        b"png"
    );
    let row = r.store.capture_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Committed);
    assert!(!r.captures.join(ID).exists(), "the staging is swept");
    // A second start of the recovery changes nothing.
    r.service.ensure_recovered().await.unwrap();
    assert_eq!(
        r.store.capture_get(FP, ID).await.unwrap().unwrap().state,
        CaptureState::Committed
    );
}
