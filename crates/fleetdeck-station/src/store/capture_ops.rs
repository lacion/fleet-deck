//! The durable record of the owner screenshots (R13 P5a task D2, design
//! `omarchy-p5a.md` §4, §6.2). One `capture_ops` row per `(deck, capture
//! id)` is kept for ever. Each [`Store`] method runs one transaction on the
//! one station connection.
//!
//! - [`Store::capture_begin`] decides a new request, a repeat, a conflict or
//!   a quota refusal, and reserves [`MAX_PNG_BYTES`] for a new one.
//! - [`Store::capture_commit`] makes the artifact, its version and the
//!   `committed` state of the row in one transaction. A capture has no
//!   session, so it writes no log event.
//! - [`Store::capture_fail`] ends a `running` row and frees its reservation.
//! - [`Store::capture_lookup`] reads a row with its artifact for the answer
//!   of a repeated request and of `desktop.capture_status`.

use std::path::PathBuf;

use fleetdeck_proto::artifacts::{AUTHOR_OWNER, Artifact, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::capture::CaptureProvenance;
use rusqlite::{Connection, OptionalExtension, params};

use super::artifact_deletions::tombstoned;
use super::artifacts::{CommitError, Committed, Staged, StagedFile, load_artifact, load_versions};
use super::{Result, Store, StoreError, call, to_i64, to_u64};
use crate::desktop::capture::{CAPTURE_QUOTA, MAX_PNG_BYTES};

/// The one file of a capture version.
pub const CAPTURE_FILE: &str = "capture.png";

/// The state of a `capture_ops` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureState {
    Running,
    Committed,
    Failed,
}

impl CaptureState {
    fn parse(text: &str) -> Result<Self> {
        match text {
            "running" => Ok(Self::Running),
            "committed" => Ok(Self::Committed),
            "failed" => Ok(Self::Failed),
            other => Err(StoreError::Corrupt(format!("capture state {other}"))),
        }
    }
}

/// One `capture_ops` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOp {
    pub deck_fp: String,
    pub capture_id: String,
    pub action_hash: String,
    pub state: CaptureState,
    pub monitor: String,
    pub artifact_id: String,
    pub sha256: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub reserved_bytes: u64,
    pub started_at: i64,
    pub updated_at: i64,
}

/// What [`Store::capture_begin`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a decision lives only between the store call and the next step"
)]
pub enum Begin {
    /// A new `running` row holds the reservation.
    Started,
    /// The same capture ID and the same action hash were seen before.
    Existing(CaptureOp),
    /// The capture ID was used for another request.
    Conflict,
    /// The reservation would cross the capture quota.
    Quota,
}

/// The artifact that a capture makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCaptureArtifact {
    pub artifact_id: String,
    pub title: String,
    pub size: u64,
    pub sha256: String,
    pub provenance: CaptureProvenance,
}

/// A row with what its state points to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a lookup lives only between the store call and the answer"
)]
pub enum Lookup {
    /// No row.
    Unknown,
    Running(CaptureOp),
    Failed(CaptureOp),
    /// The artifact has a tombstone.
    Deleted(CaptureOp),
    Committed {
        op: CaptureOp,
        artifact: Artifact,
        version: ArtifactVersion,
    },
}

type RawOp = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
);

const COLUMNS: &str = "deck_fp, capture_id, action_hash, state, monitor, artifact_id, sha256,
                       code, message, reserved_bytes, started_at, updated_at";

fn raw_op(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawOp> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
    ))
}

fn finish_op(raw: RawOp) -> Result<CaptureOp> {
    let (
        deck_fp,
        capture_id,
        action_hash,
        state,
        monitor,
        artifact_id,
        sha256,
        code,
        message,
        reserved,
        started_at,
        updated_at,
    ) = raw;
    Ok(CaptureOp {
        deck_fp,
        capture_id,
        action_hash,
        state: CaptureState::parse(&state)?,
        monitor,
        artifact_id,
        sha256,
        code,
        message,
        reserved_bytes: to_u64(reserved)?,
        started_at,
        updated_at,
    })
}

fn read_op(conn: &Connection, deck_fp: &str, capture_id: &str) -> Result<Option<CaptureOp>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM capture_ops WHERE deck_fp = ?1 AND capture_id = ?2"),
        params![deck_fp, capture_id],
        raw_op,
    )
    .optional()?
    .map(finish_op)
    .transpose()
}

impl Store {
    /// A new artifact ID for a capture, from the station ULID source.
    #[must_use]
    pub fn capture_artifact_id(&self) -> String {
        self.inner.ids.next_ulid().to_string()
    }

    /// Decides a capture request in one transaction. A row with another
    /// action hash is a conflict; a row with the same hash is returned and
    /// nothing changes; otherwise the quota is checked (the committed
    /// capture bytes, the reservations of the `running` rows and the new
    /// reservation of [`MAX_PNG_BYTES`] must stay within [`CAPTURE_QUOTA`])
    /// and a `running` row that holds the reservation is inserted.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_begin(
        &self,
        deck_fp: &str,
        capture_id: &str,
        action_hash: &str,
        monitor: &str,
        artifact_id: &str,
        now: i64,
    ) -> Result<Begin> {
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        let (action_hash, monitor) = (action_hash.to_owned(), monitor.to_owned());
        let artifact_id = artifact_id.to_owned();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(row) = read_op(&tx, &deck_fp, &capture_id)? {
                return Ok(if row.action_hash == action_hash {
                    Begin::Existing(row)
                } else {
                    Begin::Conflict
                });
            }
            let committed: i64 = tx.query_row(
                "SELECT COALESCE(SUM(size), 0) FROM artifact_versions WHERE capture IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            let reserved: i64 = tx.query_row(
                "SELECT COALESCE(SUM(reserved_bytes), 0) FROM capture_ops WHERE state = 'running'",
                [],
                |r| r.get(0),
            )?;
            let used = to_u64(committed)?
                .saturating_add(to_u64(reserved)?)
                .saturating_add(MAX_PNG_BYTES);
            if used > CAPTURE_QUOTA {
                return Ok(Begin::Quota);
            }
            tx.execute(
                "INSERT INTO capture_ops
                     (deck_fp, capture_id, action_hash, state, monitor, artifact_id,
                      reserved_bytes, started_at, updated_at)
                 VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7, ?7)",
                params![
                    deck_fp,
                    capture_id,
                    action_hash,
                    monitor,
                    artifact_id,
                    to_i64(MAX_PNG_BYTES)?,
                    now
                ],
            )?;
            tx.commit()?;
            Ok(Begin::Started)
        })
        .await
    }

    /// Makes the artifact, its version 1 and the `committed` state of the
    /// operation in one transaction (design §6.2). The bytes are already in
    /// `<artifacts>/<artifact_id>/1/` and synced.
    ///
    /// # Errors
    /// [`CommitError::UnknownArtifact`] when the artifact ID has a
    /// tombstone; a store error when the operation is not `running`. Nothing
    /// changes on an error.
    pub async fn capture_commit(
        &self,
        deck_fp: &str,
        capture_id: &str,
        new: NewCaptureArtifact,
    ) -> std::result::Result<Committed, CommitError> {
        let now = self.now();
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        call(&self.inner.db, move |conn| {
            Ok(commit(conn, &deck_fp, &capture_id, &new, now))
        })
        .await?
    }

    /// Ends a `running` operation as `failed` and frees its reservation. A
    /// row in another state does not change.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_fail(
        &self,
        deck_fp: &str,
        capture_id: &str,
        code: &str,
        message: &str,
    ) -> Result<()> {
        let now = self.now();
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        let (code, message) = (code.to_owned(), message.to_owned());
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE capture_ops
                 SET state = 'failed', code = ?3, message = ?4, reserved_bytes = 0, updated_at = ?5
                 WHERE deck_fp = ?1 AND capture_id = ?2 AND state = 'running'",
                params![deck_fp, capture_id, code, message, now],
            )?;
            Ok(())
        })
        .await
    }

    /// One operation row.
    ///
    /// # Errors
    /// A database error.
    pub async fn capture_get(&self, deck_fp: &str, capture_id: &str) -> Result<Option<CaptureOp>> {
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        call(&self.inner.db, move |conn| {
            read_op(conn, &deck_fp, &capture_id)
        })
        .await
    }

    /// The `running` operations, oldest first (the startup recovery).
    ///
    /// # Errors
    /// A database error.
    pub async fn captures_running(&self) -> Result<Vec<CaptureOp>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM capture_ops WHERE state = 'running'
                 ORDER BY started_at, capture_id"
            ))?;
            let rows = stmt
                .query_map([], raw_op)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(finish_op).collect()
        })
        .await
    }

    /// Whether a version row exists for the artifact.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact_has_version(&self, artifact_id: &str) -> Result<bool> {
        let artifact_id = artifact_id.to_owned();
        call(&self.inner.db, move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM artifact_versions WHERE artifact_id = ?1)",
                [&artifact_id],
                |r| r.get(0),
            )?)
        })
        .await
    }

    /// One operation with its artifact, in one read transaction. A
    /// tombstoned artifact gives [`Lookup::Deleted`] before anything else.
    ///
    /// # Errors
    /// A database error; a committed operation whose artifact has no row.
    pub async fn capture_lookup(&self, deck_fp: &str, capture_id: &str) -> Result<Lookup> {
        let (deck_fp, capture_id) = (deck_fp.to_owned(), capture_id.to_owned());
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(op) = read_op(&tx, &deck_fp, &capture_id)? else {
                return Ok(Lookup::Unknown);
            };
            match op.state {
                CaptureState::Running => Ok(Lookup::Running(op)),
                CaptureState::Failed => Ok(Lookup::Failed(op)),
                CaptureState::Committed => {
                    if tombstoned(&tx, &op.artifact_id)? {
                        return Ok(Lookup::Deleted(op));
                    }
                    let artifact = load_artifact(&tx, &op.artifact_id)?;
                    let version = load_versions(&tx, &op.artifact_id)?
                        .into_iter()
                        .find(|v| v.version == 1);
                    match (artifact, version) {
                        (Some(artifact), Some(version)) => Ok(Lookup::Committed {
                            op,
                            artifact,
                            version,
                        }),
                        _ => Err(StoreError::Corrupt(format!(
                            "committed capture {} has no artifact",
                            op.capture_id
                        ))),
                    }
                }
            }
        })
        .await
    }
}

/// The commit transaction. The inner result carries the domain errors; the
/// outer one is for the database.
fn commit(
    conn: &mut Connection,
    deck_fp: &str,
    capture_id: &str,
    new: &NewCaptureArtifact,
    now: i64,
) -> std::result::Result<Committed, CommitError> {
    let tx = conn.transaction().map_err(StoreError::from)?;
    if tombstoned(&tx, &new.artifact_id).map_err(StoreError::from)? {
        return Err(CommitError::UnknownArtifact(new.artifact_id.clone()));
    }
    // The hash of the bundle as the normal publish computes it.
    let manifest = Staged {
        dir: PathBuf::new(),
        files: vec![StagedFile {
            path: CAPTURE_FILE.to_owned(),
            size: new.size,
            sha256: new.sha256.clone(),
        }],
    }
    .manifest_sha256();
    let files = vec![CAPTURE_FILE.to_owned()];
    let provenance = serde_json::to_string(&new.provenance).map_err(StoreError::from)?;
    let kind = ArtifactKind::Image;
    tx.execute(
        "INSERT INTO artifacts (artifact_id, session_id, title, kind, latest_version,
             author, created_at, updated_at)
         VALUES (?1, NULL, ?2, ?3, 1, ?4, ?5, ?5)",
        params![new.artifact_id, new.title, kind.as_str(), AUTHOR_OWNER, now],
    )
    .map_err(StoreError::from)?;
    // The version row refers to the artifact row, so it comes second.
    tx.execute(
        "INSERT INTO artifact_versions
             (artifact_id, version, size, sha256, manifest_sha256, files, created_at, capture)
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            new.artifact_id,
            to_i64(new.size)?,
            new.sha256,
            manifest,
            serde_json::to_string(&files).map_err(StoreError::from)?,
            now,
            provenance
        ],
    )
    .map_err(StoreError::from)?;
    let changed = tx
        .execute(
            "UPDATE capture_ops
             SET state = 'committed', sha256 = ?3, reserved_bytes = 0, updated_at = ?4
             WHERE deck_fp = ?1 AND capture_id = ?2 AND state = 'running'",
            params![deck_fp, capture_id, new.sha256, now],
        )
        .map_err(StoreError::from)?;
    if changed != 1 {
        return Err(StoreError::Corrupt(format!("capture {capture_id} is not running")).into());
    }
    tx.commit().map_err(StoreError::from)?;
    Ok(Committed {
        artifact: Artifact {
            artifact_id: new.artifact_id.clone(),
            station_id: String::new(),
            session_id: None,
            title: new.title.clone(),
            kind,
            latest_version: 1,
            created_at: now,
            updated_at: now,
            author: AUTHOR_OWNER.to_owned(),
        },
        version: ArtifactVersion {
            artifact_id: new.artifact_id.clone(),
            version: 1,
            size: new.size,
            sha256: new.sha256.clone(),
            files,
            created_at: now,
            capture: Some(new.provenance.clone()),
        },
        created: true,
    })
}

#[cfg(test)]
#[path = "capture_ops_tests.rs"]
mod tests;
