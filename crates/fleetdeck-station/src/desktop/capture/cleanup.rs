//! The durable cleanup of a capture (R13 P5b task S2, design `omarchy-p5b.md`
//! §5.3 "Durable removal", decision O12-7). The screenshot and the recording
//! operation share it.
//!
//! - [`cleanup_failed`]: the operation becomes `failed` (`capture_fail` for a
//!   screenshot, `recording_fail` for a recording) and keeps its
//!   reservation, the staging folder and the uncommitted destination folder
//!   are removed durably, and only when both removals returned `Ok` the
//!   reservation is freed (`capture_cleaned`). A removal that fails leaves the
//!   operation in its cleanup state with its reservation (the quota counts it)
//!   and the next start retries it.
//! - [`finish_staging`]: after a commit, the staging folder is removed
//!   durably; for a recording the reservation of the staging copy is freed
//!   (`capture_staging_cleaned`) only after that.

use std::sync::Arc;

use super::Staging;
use crate::artifacts::ArtifactService;
use crate::artifacts::capture_import::{DurableFs, discard_durable};
use crate::desktop::Fail;
use crate::store::capture_ops::RecordingRow;

/// A failure after the operation row exists: the row becomes `failed` (the
/// reservation stays), then the staging folder and the uncommitted
/// destination folder go durably, and when both removals worked the
/// reservation is freed. A row that is not `running` (already failed or
/// committed) keeps its state and code; the removal still runs.
///
/// A recording whose latch holds a stop reason is failed with
/// `recording_fail` and that reason; any other row is failed with
/// `capture_fail`.
///
/// A log line names an error kind or a code, never a path.
pub(crate) async fn cleanup_failed(
    svc: &ArtifactService,
    deck_fp: &str,
    capture_id: &str,
    artifact_id: &str,
    fail: &Fail,
    fs: Arc<dyn DurableFs>,
) {
    let store = svc.store();
    // When the row cannot be recorded it stays `running` with its files: the
    // startup recovery handles both.
    let recorded = match store.recording_get(deck_fp, capture_id).await {
        Ok(Some(RecordingRow {
            stop_reason: Some(reason),
            ..
        })) => {
            store
                .recording_fail(deck_fp, capture_id, fail.code, &fail.message, reason)
                .await
        }
        Ok(_) => {
            store
                .capture_fail(deck_fp, capture_id, fail.code, &fail.message)
                .await
        }
        Err(e) => Err(e),
    };
    if let Err(e) = recorded {
        tracing::warn!(error = %e, "cannot record the failed capture");
        return;
    }
    let (captures, id) = (svc.captures_dir(), capture_id.to_owned());
    let (root, artifact) = (svc.root().to_path_buf(), artifact_id.to_owned());
    let removed = tokio::task::spawn_blocking(move || {
        let staging = Staging::remove(&captures, &id, &*fs);
        let folder = discard_durable(&root, &artifact, &*fs);
        (staging, folder)
    })
    .await;
    match removed {
        Ok((Ok(()), Ok(()))) => {}
        Ok((staging, folder)) => {
            tracing::warn!(
                staging = ?staging.err().map(|e| e.code),
                folder = ?folder.err().map(|e| e.kind()),
                "the capture cleanup did not finish; the next start retries"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "the capture cleanup ended abnormally");
            return;
        }
    }
    if let Err(e) = store.capture_cleaned(deck_fp, capture_id).await {
        tracing::warn!(error = %e, "cannot free the reservation of the failed capture");
    }
}

/// After a commit: removes the staging folder durably. With `release` set (a
/// recording, whose commit kept the staging bytes reserved) the reservation is
/// freed after the removal; a screenshot reserved nothing at its commit, so
/// it only removes. A failure is logged and the next start retries it: the
/// recovery removes the staging folder of a committed operation.
pub(crate) async fn finish_staging(
    svc: &ArtifactService,
    deck_fp: &str,
    capture_id: &str,
    fs: Arc<dyn DurableFs>,
    release: bool,
) {
    let (captures, id) = (svc.captures_dir(), capture_id.to_owned());
    match tokio::task::spawn_blocking(move || Staging::remove(&captures, &id, &*fs)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(code = e.code, "cannot remove the capture staging");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "cannot remove the capture staging");
            return;
        }
    }
    if release
        && let Err(e) = svc
            .store()
            .capture_staging_cleaned(deck_fp, capture_id)
            .await
    {
        tracing::warn!(error = %e, "cannot free the staging reservation of the capture");
    }
}
