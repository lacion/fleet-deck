//! Tests of the upload stream (FG-9, design §12.3 step 3, ADR-009 §9, T4.2c):
//! the task of one `move-import:` stream against a recording port.

use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{TranscriptManifest, code};
use fleetdeck_testkit::Det;
use tokio::sync::mpsc;

use super::{MoveUploads, Settled, UploadEvent, UploadPort, serve_upload};
use crate::store::move_staging::StagingState;
use crate::store::{Store, StoreConfig};
use crate::workspace::fleet_git::move_layout::staged_name;
use crate::workspace::fleet_git::move_layout::tests::{NATIVE, Rig, claude_line};

/// What the stream task did to its stream.
#[derive(Default)]
struct Recorder {
    log: Mutex<Vec<String>>,
}

impl Recorder {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait]
impl UploadPort for Recorder {
    async fn consumed(&self, n: usize) {
        self.log.lock().unwrap().push(format!("consumed:{n}"));
    }

    async fn close(&self) {
        self.log.lock().unwrap().push("close".to_owned());
    }

    async fn reset(&self, reason: &str) {
        self.log.lock().unwrap().push(format!("reset:{reason}"));
    }
}

struct Fx {
    store: Store,
    uploads: MoveUploads,
    root: tempfile::TempDir,
    manifest: TranscriptManifest,
    bytes: Vec<u8>,
}

impl Fx {
    async fn new() -> Self {
        let rig = Rig::new();
        let body = format!("{}{}", claude_line(NATIVE, 1), claude_line(NATIVE, 2));
        rig.write(&rig.claude_main(), body.as_bytes());
        rig.write(&rig.claude_sub("agent-a1.jsonl"), b"{\"x\":1}\n");
        let exported = rig.export(ProviderKind::Claude).unwrap();
        let mut bytes = Vec::new();
        for open in &exported.files {
            let mut buf = vec![0_u8; usize::try_from(open.entry.size).unwrap()];
            open.file.read_exact_at(&mut buf, 0).unwrap();
            bytes.extend_from_slice(&buf);
        }
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let uploads = MoveUploads::default();
        uploads.set_dir(root.path().join("moves"));
        Self {
            store,
            uploads,
            root,
            manifest: exported.manifest,
            bytes,
        }
    }

    fn folder(&self, move_id: &str) -> PathBuf {
        self.root.path().join("moves").join(move_id)
    }

    /// Runs the task of a stream that receives `events` and ends.
    async fn run(&self, move_id: &str, events: Vec<UploadEvent>) -> Vec<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        for event in events {
            tx.send(event).unwrap();
        }
        let port = Recorder::default();
        let reset: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
        serve_upload(&self.store, &self.uploads, &port, reset, move_id, rx).await;
        port.log()
    }

    fn data(&self) -> Vec<UploadEvent> {
        self.bytes
            .chunks(11)
            .map(|c| UploadEvent::Data(c.to_vec()))
            .collect()
    }
}

fn resets(log: &[String]) -> Vec<&str> {
    log.iter()
        .filter_map(|l| l.strip_prefix("reset:"))
        .collect()
}

#[tokio::test]
async fn the_upload_writes_the_staged_files_and_closes_its_half_when_all_arrived() {
    let fx = Fx::new().await;
    assert!(fx.uploads.put_pending("m1", fx.manifest.clone()));
    let mut events = fx.data();
    events.push(UploadEvent::Closed);
    let log = fx.run("m1", events).await;
    assert_eq!(log.last().map(String::as_str), Some("close"), "{log:?}");
    assert!(resets(&log).is_empty(), "{log:?}");
    let consumed: usize = log
        .iter()
        .filter_map(|l| l.strip_prefix("consumed:"))
        .map(|n| n.parse::<usize>().unwrap())
        .sum();
    assert_eq!(consumed, fx.bytes.len(), "the credit is given back");
    let row = fx.store.move_staging("m1".into()).await.unwrap().unwrap();
    assert_eq!(row.state, StagingState::Ready);
    assert_eq!(row.manifest, fx.manifest);
    let folder = fx.folder("m1");
    assert_eq!(row.path, folder.display().to_string());
    assert_eq!(
        std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let first = std::fs::read(folder.join(staged_name(0))).unwrap();
    assert_eq!(first.len() as u64, fx.manifest.files[0].size);
    assert_eq!(
        std::fs::metadata(folder.join(staged_name(1)))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    // The manifest was used up: a second stream needs a new prepare.
    let log = fx.run("m1", vec![UploadEvent::Closed]).await;
    assert_eq!(resets(&log), ["unknown_move"]);
}

#[tokio::test]
async fn an_upload_with_a_bad_hash_is_reset_with_move_checksum_and_leaves_no_folder() {
    let mut fx = Fx::new().await;
    fx.bytes[2] ^= 0xff;
    assert!(fx.uploads.put_pending("m1", fx.manifest.clone()));
    let mut events = fx.data();
    events.push(UploadEvent::Closed);
    let log = fx.run("m1", events).await;
    assert_eq!(resets(&log), [code::MOVE_CHECKSUM], "{log:?}");
    assert!(!fx.folder("m1").exists());
    let row = fx.store.move_staging("m1".into()).await.unwrap().unwrap();
    assert_eq!(row.state, StagingState::Deleted);
}

#[tokio::test]
async fn a_short_upload_is_reset_with_move_checksum() {
    let fx = Fx::new().await;
    assert!(fx.uploads.put_pending("m1", fx.manifest.clone()));
    let mut events = vec![UploadEvent::Data(fx.bytes[..fx.bytes.len() - 1].to_vec())];
    events.push(UploadEvent::Closed);
    let log = fx.run("m1", events).await;
    assert_eq!(resets(&log), [code::MOVE_CHECKSUM], "{log:?}");
    assert!(!fx.folder("m1").exists());
}

#[tokio::test]
async fn an_upload_over_the_manifest_total_is_reset_with_move_too_large() {
    let fx = Fx::new().await;
    assert!(fx.uploads.put_pending("m1", fx.manifest.clone()));
    let mut bytes = fx.bytes.clone();
    bytes.push(b'x');
    let log = fx.run("m1", vec![UploadEvent::Data(bytes)]).await;
    assert_eq!(resets(&log), [code::MOVE_TOO_LARGE], "{log:?}");
    assert!(!fx.folder("m1").exists());
}

#[tokio::test]
async fn a_reset_from_the_deck_deletes_the_folder() {
    let fx = Fx::new().await;
    assert!(fx.uploads.put_pending("m1", fx.manifest.clone()));
    let log = fx
        .run(
            "m1",
            vec![
                UploadEvent::Data(fx.bytes[..5].to_vec()),
                UploadEvent::Reset,
            ],
        )
        .await;
    assert!(resets(&log).is_empty(), "{log:?}");
    assert!(!fx.folder("m1").exists());
    let row = fx.store.move_staging("m1".into()).await.unwrap().unwrap();
    assert_eq!(row.state, StagingState::Deleted);
}

#[tokio::test]
async fn an_upload_that_opens_after_the_tombstone_is_reset() {
    let fx = Fx::new().await;
    // A discard of an unknown move id stores the tombstone.
    fx.store
        .move_discard_tx("m1".into(), "bravo".into())
        .await
        .unwrap();
    assert!(fx.uploads.put_pending("m1", fx.manifest.clone()));
    let log = fx.run("m1", vec![UploadEvent::Closed]).await;
    assert_eq!(resets(&log), [code::MOVE_DISCARDED], "{log:?}");
    assert!(!fx.folder("m1").exists());
}

#[tokio::test]
async fn a_stream_without_a_prepare_or_a_staging_root_is_reset() {
    let fx = Fx::new().await;
    let log = fx.run("nobody", vec![UploadEvent::Closed]).await;
    assert_eq!(resets(&log), ["unknown_move"]);
    let bare = MoveUploads::default();
    let (tx, rx) = mpsc::unbounded_channel();
    drop(tx);
    let port = Recorder::default();
    let reset: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
    serve_upload(&fx.store, &bare, &port, reset, "m1", rx).await;
    assert_eq!(resets(&port.log()), ["unsupported"]);
}

#[tokio::test]
async fn a_discard_resets_an_active_upload_and_waits_until_its_task_ended() {
    let fx = Fx::new().await;
    assert!(fx.uploads.put_pending("m1", fx.manifest.clone()));
    let (tx, rx) = mpsc::unbounded_channel::<UploadEvent>();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let port = Arc::new(Recorder::default());
    let task = {
        let (store, uploads, port) = (fx.store.clone(), fx.uploads.clone(), Arc::clone(&port));
        let seen = Arc::clone(&seen);
        let reset: Arc<dyn Fn(&str) + Send + Sync> =
            Arc::new(move |reason| seen.lock().unwrap().push(reason.to_owned()));
        tokio::spawn(async move {
            serve_upload(&store, &uploads, port.as_ref(), reset, "m1", rx).await;
        })
    };
    // The task made its folder and its record: it waits for bytes.
    for _ in 0..200 {
        if fx.folder("m1").exists() && fx.store.move_staging("m1".into()).await.unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(fx.folder("m1").exists());
    // A second prepare is refused while the upload is active.
    assert!(!fx.uploads.put_pending("m1", fx.manifest.clone()));
    let settled = fx.uploads.settle("m1", Duration::from_secs(10)).await;
    assert_eq!(settled, Settled::Ended);
    assert_eq!(*seen.lock().unwrap(), [code::MOVE_DISCARDED]);
    task.await.unwrap();
    drop(tx);
    assert!(!fx.folder("m1").exists(), "the task deleted its folder");
    // Nothing is active any more.
    assert_eq!(
        fx.uploads.settle("m1", Duration::from_secs(1)).await,
        Settled::NoUpload
    );
}
