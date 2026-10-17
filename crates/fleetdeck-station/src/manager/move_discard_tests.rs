//! `session.move_discard` and the import of the files of a move (FG-9, design
//! §12.3 "Abort" and step 4, T4.2c), on the fixture of `move_target_tests.rs`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::fleet_git::{
    ManifestFile, MoveDiscardEffect, MoveDiscardState, MoveInState, MoveKind, TranscriptManifest,
    code,
};

use super::{DISCARDED, Fx, NATIVE, sha256_hex, state};
use crate::store::move_staging::StagingState;
use crate::workspace::fleet_git::{Folder, Receiver, place_files};

#[tokio::test]
async fn a_discard_with_no_row_stores_a_tombstone_and_answers_discarded_none() {
    let Some(fx) = Fx::new().await else { return };
    // An upload that arrived, and no row yet: the folder goes by its record.
    let (_, folder, _) = fx.uploaded("m1").await;
    let answer = fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(
        (answer.state, answer.effect),
        (MoveDiscardState::Discarded, Some(MoveDiscardEffect::None))
    );
    assert!(fx.rig.store.move_tombstoned("m1".into()).await.unwrap());
    assert!(!Path::new(&folder.path).exists());
    let row = fx
        .rig
        .store
        .move_staging("m1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, StagingState::Deleted);
    // An id that nobody knows gets the same answer, and a repeat too.
    for _ in 0..2 {
        let answer = fx.manager.move_discard("never").await.unwrap();
        assert_eq!(
            (answer.state, answer.effect),
            (MoveDiscardState::Discarded, Some(MoveDiscardEffect::None))
        );
    }
    assert!(fx.rig.store.move_tombstoned("never".into()).await.unwrap());
    let status = fx.manager.move_status("never").await.unwrap();
    assert_eq!(
        status.state,
        fleetdeck_proto::fleet_git::MoveStatusState::Discarded
    );
    fx.shutdown().await;
}

/// A native `MoveIn` with its files placed in `dst-home` (an `imported`
/// state when `imported`).
async fn native_move(fx: &Fx, imported: bool) -> (String, Folder, Vec<String>) {
    let (manifest, folder, _) = fx.uploaded("m1").await;
    let decided = fx.accepted("c1", fx.move_in("m1", MoveKind::Native)).await;
    let id = fx.session_of(&decided).await;
    let mut placed = Vec::new();
    if imported {
        let cwd = fx.summary(&id).await.cwd.unwrap();
        let done = place_files(
            &fx.rig.store,
            "m1",
            fx.rig.root.join("dst-home"),
            ProviderKind::Claude,
            cwd,
            manifest,
            folder.path.clone(),
        )
        .await
        .unwrap();
        placed = done.into_iter().map(|f| f.path).collect();
        fx.advance("m1", MoveInState::Staged, MoveInState::Imported)
            .await;
    }
    (id, folder, placed)
}

#[tokio::test]
async fn place_files_records_each_placed_file_with_its_identity() {
    let Some(fx) = Fx::new().await else { return };
    let (_, _, placed) = native_move(&fx, true).await;
    assert_eq!(placed.len(), 1);
    let row = fx.row("m1").await;
    assert_eq!(row.imported.len(), 1);
    assert_eq!(row.imported[0].path, placed[0]);
    let meta = std::fs::metadata(&placed[0]).unwrap();
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            (row.imported[0].dev, row.imported[0].ino),
            (meta.dev(), meta.ino())
        );
    }
    fx.shutdown().await;
}

#[tokio::test]
async fn a_failed_import_deletes_the_placed_files_by_identity() {
    let Some(fx) = Fx::new().await else { return };
    // A manifest of a main file and a subagent file; the folder of the
    // subagent file is a regular file, so the second placement fails after
    // the first one.
    let main = format!("{{\"sessionId\":\"{NATIVE}\",\"type\":\"user\"}}\n").into_bytes();
    let sub = b"{\"x\":1}\n".to_vec();
    let manifest = TranscriptManifest {
        provider: ProviderKind::Claude,
        native_session_id: NATIVE.to_owned(),
        files: vec![
            ManifestFile {
                key: "main".to_owned(),
                rel: format!("projects/-src-/{NATIVE}.jsonl"),
                size: main.len() as u64,
                sha256: sha256_hex(&main),
            },
            ManifestFile {
                key: "subagent:a1".to_owned(),
                rel: format!("projects/-src-/{NATIVE}/subagents/agent-a1.jsonl"),
                size: sub.len() as u64,
                sha256: sha256_hex(&sub),
            },
        ],
        total: (main.len() + sub.len()) as u64,
    };
    let mut receiver = Receiver::create(&fx.rig.root.join("moves"), "m1", &manifest).unwrap();
    receiver.feed(&main).unwrap();
    receiver.feed(&sub).unwrap();
    let folder = receiver.finish().unwrap();
    let store = &fx.rig.store;
    let at = (folder.path.display().to_string(), folder.dev, folder.ino);
    store
        .move_staging_insert("m1".to_owned(), at, manifest.clone())
        .await
        .unwrap();
    store
        .move_staging_set(
            "m1".to_owned(),
            StagingState::Uploading,
            StagingState::Ready,
        )
        .await
        .unwrap();
    let decided = fx.accepted("c1", fx.move_in("m1", MoveKind::Native)).await;
    let id = fx.session_of(&decided).await;
    let cwd = fx.summary(&id).await.cwd.unwrap();
    let home = fx.rig.root.join("dst-home");
    let project = home
        .join("projects")
        .join(crate::history::locate::encode_cwd(&cwd));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(NATIVE), "in the way").unwrap();
    let err = place_files(
        store,
        "m1",
        home,
        ProviderKind::Claude,
        cwd,
        manifest,
        folder.path.clone(),
    )
    .await
    .expect_err("the second placement fails");
    assert_eq!(err.code, code::MOVE_RESUME_FAILED);
    assert!(
        !project.join(format!("{NATIVE}.jsonl")).exists(),
        "the placed main file is deleted"
    );
    // The record stays: a discard finds nothing more to delete.
    assert_eq!(fx.row("m1").await.imported.len(), 1);
    fx.shutdown().await;
}

#[tokio::test]
async fn a_discard_in_staged_or_imported_deletes_the_staging_folder_and_imported_files_by_identity()
{
    for imported in [false, true] {
        let Some(fx) = Fx::new().await else { return };
        let (id, folder, placed) = native_move(&fx, imported).await;
        assert_eq!(placed.len(), usize::from(imported));
        let answer = fx.manager.move_discard("m1").await.unwrap();
        assert_eq!(
            (answer.state, answer.effect),
            (MoveDiscardState::Discarded, Some(MoveDiscardEffect::None)),
            "imported {imported}"
        );
        assert!(!folder.path.exists(), "the staging folder is deleted");
        for path in &placed {
            assert!(!Path::new(path).exists(), "{path}");
        }
        let row = fx.row("m1").await;
        assert_eq!(row.state, MoveInState::Discarded);
        assert!(fx.rig.store.move_tombstoned("m1".into()).await.unwrap());
        assert!(
            fx.rig
                .store
                .session_hold(id.as_str().into())
                .await
                .unwrap()
                .is_none()
        );
        let summary = fx.summary(&id).await;
        assert_eq!(summary.state, SessionState::Exited);
        assert_eq!(summary.move_in.unwrap().state, MoveInState::Discarded);
        let staging = fx
            .rig
            .store
            .move_staging("m1".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(staging.state, StagingState::Deleted);
        // The session of a discarded move takes no input and the id never runs.
        assert_eq!(
            fx.refused("again", fx.move_in("m1", MoveKind::Native))
                .await,
            state(DISCARDED)
        );
        fx.shutdown().await;
    }
}

#[tokio::test]
async fn a_discard_after_resumed_stores_no_tombstone_and_answers_resumed() {
    let Some(fx) = Fx::new().await else { return };
    fx.accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    fx.advance("m1", MoveInState::Staged, MoveInState::Launching)
        .await;
    fx.advance("m1", MoveInState::Launching, MoveInState::Resumed)
        .await;
    for _ in 0..2 {
        let answer = fx.manager.move_discard("m1").await.unwrap();
        assert_eq!(
            (answer.state, answer.effect),
            (MoveDiscardState::Resumed, Some(MoveDiscardEffect::None))
        );
    }
    assert!(!fx.rig.store.move_tombstoned("m1".into()).await.unwrap());
    assert_eq!(fx.row("m1").await.state, MoveInState::Resumed);
    fx.shutdown().await;
}

#[tokio::test]
async fn a_discard_in_launching_answers_abandoned_and_deletes_no_file() {
    let Some(fx) = Fx::new().await else { return };
    let (id, folder, placed) = native_move(&fx, true).await;
    fx.advance("m1", MoveInState::Imported, MoveInState::Launching)
        .await;
    let answer = fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(
        (answer.state, answer.effect),
        (MoveDiscardState::Abandoned, Some(MoveDiscardEffect::None))
    );
    // No file goes; the copy stays read only (the hold stays).
    assert!(folder.path.exists());
    assert!(Path::new(&placed[0]).exists());
    let row = fx.row("m1").await;
    assert_eq!(row.state, MoveInState::Abandoned);
    assert_eq!(row.code.as_deref(), Some("move_cancelled"));
    assert!(
        row.message
            .as_deref()
            .unwrap()
            .starts_with("The move was cancelled after Claude started on Bravo."),
        "{:?}",
        row.message
    );
    assert!(
        fx.rig
            .store
            .session_hold(id.as_str().into())
            .await
            .unwrap()
            .is_some()
    );
    assert!(fx.rig.store.move_tombstoned("m1".into()).await.unwrap());
    let summary = fx.summary(&id).await;
    assert_eq!(summary.state, SessionState::Exited);
    assert_eq!(summary.move_in.unwrap().state, MoveInState::Abandoned);
    // A repeat answers the same and changes nothing.
    let again = fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(again.state, MoveDiscardState::Abandoned);
    assert!(Path::new(&placed[0]).exists());
    fx.shutdown().await;
}

#[tokio::test]
async fn a_repeated_discard_answers_the_same() {
    let Some(fx) = Fx::new().await else { return };
    fx.accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    let first = fx.manager.move_discard("m1").await.unwrap();
    let second = fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first.state, MoveDiscardState::Discarded);
    fx.shutdown().await;
}

#[tokio::test]
async fn a_discard_resets_an_active_upload_with_move_discarded_and_waits_for_its_task() {
    let Some(fx) = Fx::new().await else { return };
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let reset: Arc<dyn Fn(&str) + Send + Sync> = {
        let seen = Arc::clone(&seen);
        Arc::new(move |reason| seen.lock().unwrap().push(reason.to_owned()))
    };
    let guard = fx
        .manager
        .move_uploads()
        .register("m1", reset)
        .expect("no upload is active");
    let task = tokio::spawn(async move {
        guard.cancelled().await;
        // The task needs a moment to end after the cancel.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(guard);
    });
    let answer = fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(
        (answer.state, answer.effect),
        (
            MoveDiscardState::Discarded,
            Some(MoveDiscardEffect::Settled)
        )
    );
    assert_eq!(*seen.lock().unwrap(), [code::MOVE_DISCARDED]);
    task.await.unwrap();
    fx.shutdown().await;
}

#[tokio::test]
async fn a_settle_that_takes_too_long_answers_settling_and_ends_in_the_background() {
    let Some(fx) = Fx::new().await else { return };
    let (_, folder, _) = fx.uploaded("m1").await;
    let reset: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
    let guard = fx.manager.move_uploads().register("m1", reset).unwrap();
    // The task ignores the cancel for longer than the settle wait.
    let task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        drop(guard);
    });
    let answer = fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(answer.state, MoveDiscardState::Settling);
    assert_eq!(answer.effect, None);
    assert!(!answer.is_ack(), "settling is not an acknowledgement");
    task.await.unwrap();
    // The background work deletes the folder once the upload ended.
    for _ in 0..200 {
        if !folder.path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!folder.path.exists());
    let again = fx.manager.move_discard("m1").await.unwrap();
    assert!(again.is_ack(), "{again:?}");
    fx.shutdown().await;
}

#[tokio::test]
async fn the_start_deletes_the_folder_of_a_cut_upload_with_its_identity() {
    let Some(fx) = Fx::new().await else { return };
    let (manifest, _, _) = fx.uploaded("m0").await;
    let receiver = Receiver::create(&fx.rig.root.join("moves"), "cut", &manifest).unwrap();
    let folder = receiver.folder().clone();
    let at = (folder.path.display().to_string(), folder.dev, folder.ino);
    fx.rig
        .store
        .move_staging_insert("cut".to_owned(), at, manifest)
        .await
        .unwrap();
    fx.manager.clear_cut_uploads().await;
    assert!(!folder.path.exists());
    let row = fx
        .rig
        .store
        .move_staging("cut".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, StagingState::Deleted);
    fx.shutdown().await;
}
