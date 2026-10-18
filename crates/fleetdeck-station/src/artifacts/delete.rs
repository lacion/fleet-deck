//! `artifacts.delete` (R13 P5a, A4 station side): a permanent local
//! tombstone, the removal of the whole version tree without following a
//! link, and the refusal of every later publish and read of the artifact.
//!
//! The order is: the tombstone (`pending`), the files, then the rows and
//! `complete` in one transaction. A crash or a failure between the steps
//! leaves a `pending` tombstone: every read already refuses the id, the
//! deck retries, and [`ArtifactService::finish_pending_deletions`] runs the
//! rest at the start of the station.

use fleetdeck_proto::artifacts::{
    ArtifactDeleteParams, ArtifactDeleted, ArtifactDeletedState, code,
};

use super::{ArtifactError, ArtifactService};
use crate::store::artifact_deletions::DeleteBegin;

/// The station artifact ids are ULIDs: 1 to 64 ASCII letters or digits.
fn check_id(id: &str) -> Result<(), ArtifactError> {
    if (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(ArtifactError::new(
            code::INVALID_PARAMS,
            "an artifact id is 1 to 64 letters or digits",
        ))
    }
}

fn internal(e: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::new("internal", e.to_string())
}

impl ArtifactService {
    /// `artifacts.delete`. A repeated call, and an id that the station does
    /// not know, answer `deleted` too.
    ///
    /// # Errors
    /// `invalid_params` for a bad id; `failed` when the files could not be
    /// removed (the tombstone stays `pending`, a retry continues);
    /// `internal` for a database error.
    pub async fn delete(
        &self,
        req: ArtifactDeleteParams,
    ) -> Result<ArtifactDeleted, ArtifactError> {
        let id = req.artifact_id;
        check_id(&id)?;
        match self
            .store
            .artifact_delete_begin(id.clone())
            .await
            .map_err(internal)?
        {
            DeleteBegin::Complete => {}
            DeleteBegin::Pending => self.finish_deletion(&id).await?,
        }
        Ok(ArtifactDeleted {
            artifact_id: id,
            state: ArtifactDeletedState::Deleted,
        })
    }

    /// Finishes every deletion that a crash or a failure left `pending`. An
    /// error is logged and the next artifact goes on.
    pub async fn finish_pending_deletions(&self) {
        let pending = match self.store.artifact_deletions_pending().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "the pending artifact deletions could not be listed");
                return;
            }
        };
        for id in pending {
            if let Err(e) = self.finish_deletion(&id).await {
                tracing::warn!(artifact_id = %id, error = %e, "an artifact deletion is still pending");
            }
        }
    }

    /// Removes `<root>/<id>` (a missing folder is success), then the rows.
    /// Both steps are safe to repeat, so concurrent deletes of one id
    /// agree.
    async fn finish_deletion(&self, id: &str) -> Result<(), ArtifactError> {
        let (root, name) = (self.root.clone(), id.to_owned());
        let removed =
            tokio::task::spawn_blocking(move || crate::desktop::profile::remove(&root, &name))
                .await
                .map_err(internal)?;
        if let Err(e) = removed {
            tracing::warn!(artifact_id = %id, error = ?e, "the artifact files could not be removed");
            return Err(ArtifactError::new(
                "failed",
                "The station could not remove the artifact files.",
            ));
        }
        self.store
            .artifact_delete_finish(id.to_owned())
            .await
            .map_err(internal)
    }
}
