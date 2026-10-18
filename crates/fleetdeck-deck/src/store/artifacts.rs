//! The deck side of artifacts (R12, DL-165): the index that folded
//! `artifact.published` events feed, the version metadata, the byte cache,
//! and the share links.
//!
//! [`record_artifact`] runs inside the read-model transaction, so an index
//! row commits with the cursor that folds its event. The cache is filled
//! when an artifact is viewed or shared, and a share serves from it, so a
//! share works while the station is off. A share keeps the SHA-256 of its
//! token, never the token.

use fleetdeck_proto::artifacts::{ArtifactKind, ArtifactVersion, DeckArtifact, ShareView};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::{EventEnvelope, SessionId, StationId};
use rusqlite::{OptionalExtension, Transaction, params};

use super::artifact_tombstones::tombstoned;
use super::desktop_captures::{CAPTURE_CACHE_LIMIT, cache_put_tx};
use super::{DeckError, DeckStore, Result, call, to_i64};

/// An SQL condition that holds when the artifact of the row that `$a` names
/// has no tombstone (R13 P5a A4): every read adds it.
macro_rules! not_tombstoned {
    ($a:literal) => {
        concat!(
            "NOT EXISTS (SELECT 1 FROM artifact_tombstones x WHERE x.station_id = ",
            $a,
            ".station_id AND x.artifact_id = ",
            $a,
            ".artifact_id)"
        )
    };
}

#[path = "artifact_cache_slice.rs"]
mod cache_slice;

/// Stores or advances the index row of an `artifact.published` event; other
/// events do nothing. The latest version never goes down.
pub(super) fn record_artifact(
    tx: &Transaction<'_>,
    station: &StationId,
    e: &EventEnvelope,
) -> Result<()> {
    let EventBody::Known(Event::ArtifactPublished {
        artifact_id,
        version,
        title,
        artifact_kind,
    }) = &e.event
    else {
        return Ok(());
    };
    if tombstoned(tx, station.as_str(), artifact_id)? {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO artifacts (station_id, artifact_id, session_id, title, kind,
             latest_version, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (station_id, artifact_id) DO UPDATE SET
             title = CASE WHEN excluded.latest_version >= latest_version
                          THEN excluded.title ELSE title END,
             kind = CASE WHEN excluded.latest_version >= latest_version
                         THEN excluded.kind ELSE kind END,
             latest_version = MAX(latest_version, excluded.latest_version),
             updated_at = MAX(updated_at, excluded.updated_at)",
        params![
            station.as_str(),
            artifact_id,
            e.session_id.as_str(),
            title,
            artifact_kind.as_str(),
            version,
            e.at,
        ],
    )?;
    Ok(())
}

/// The index row of a relayed publish, inside the caller's transaction. A
/// tombstoned identity is never restored (R13 P5a A4).
///
/// # Errors
/// A database error.
pub(crate) fn note_artifact_tx(
    tx: &Transaction<'_>,
    station: &StationId,
    a: &fleetdeck_proto::artifacts::Artifact,
) -> Result<()> {
    if tombstoned(tx, station.as_str(), &a.artifact_id)? {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO artifacts (station_id, artifact_id, session_id, title, kind,
             latest_version, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (station_id, artifact_id) DO UPDATE SET
             title = CASE WHEN excluded.latest_version >= latest_version
                          THEN excluded.title ELSE title END,
             kind = CASE WHEN excluded.latest_version >= latest_version
                         THEN excluded.kind ELSE kind END,
             latest_version = MAX(latest_version, excluded.latest_version),
             updated_at = MAX(updated_at, excluded.updated_at)",
        params![
            station.as_str(),
            a.artifact_id,
            a.session_id.as_ref().map(SessionId::as_str),
            a.title,
            a.kind.as_str(),
            a.latest_version,
            a.updated_at
        ],
    )?;
    Ok(())
}

/// Version metadata, inside the caller's transaction. The `capture` column
/// is never written here: only the capture path sets it.
///
/// # Errors
/// A database error.
pub(crate) fn put_artifact_versions_tx(
    tx: &Transaction<'_>,
    station: &StationId,
    versions: &[ArtifactVersion],
) -> Result<()> {
    for v in versions {
        if tombstoned(tx, station.as_str(), &v.artifact_id)? {
            continue;
        }
        tx.execute(
            "INSERT OR IGNORE INTO artifact_versions
                 (station_id, artifact_id, version, size, sha256, files, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                station.as_str(),
                v.artifact_id,
                v.version,
                to_i64(v.size)?,
                v.sha256,
                serde_json::to_string(&v.files)?,
                v.created_at
            ],
        )?;
    }
    Ok(())
}

/// One share row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRow {
    pub share_id: String,
    pub station_id: StationId,
    pub artifact_id: String,
    /// `None`: always the latest version.
    pub version: Option<u32>,
    /// `view` is the board's own short link.
    pub kind: String,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub views: u64,
}

impl ShareRow {
    /// True while the share can be used at `now`.
    #[must_use]
    pub fn live(&self, now: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

/// A new share row.
#[derive(Debug, Clone)]
pub struct NewShare {
    pub token_hash: String,
    pub station_id: StationId,
    pub artifact_id: String,
    pub version: Option<u32>,
    pub view: bool,
    pub created_by: String,
    pub expires_at: i64,
}

/// The id of a share: the first 16 characters of its token hash. It names
/// the share in lists and revocations and is not the secret.
#[must_use]
pub fn share_id_of(token_hash: &str) -> String {
    token_hash.chars().take(16).collect()
}

const SHARE_COLUMNS: &str = "share_id, station_id, artifact_id, version, kind, created_by,
     created_at, expires_at, revoked_at, views";

fn share_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShareRow> {
    Ok(ShareRow {
        share_id: r.get(0)?,
        station_id: StationId::from(r.get::<_, String>(1)?),
        artifact_id: r.get(2)?,
        version: r.get(3)?,
        kind: r.get(4)?,
        created_by: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        revoked_at: r.get(8)?,
        views: u64::try_from(r.get::<_, i64>(9)?).unwrap_or(0),
    })
}

fn index_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(DeckArtifact, String)> {
    Ok((
        DeckArtifact {
            station_id: r.get(0)?,
            station_name: r.get(1)?,
            artifact_id: r.get(2)?,
            session_id: r.get::<_, Option<String>>(3)?.map(SessionId::from),
            title: r.get(4)?,
            kind: ArtifactKind::Other,
            latest_version: r.get(6)?,
            updated_at: r.get(7)?,
        },
        r.get(5)?,
    ))
}

fn with_kind((mut a, kind): (DeckArtifact, String)) -> Result<DeckArtifact> {
    a.kind = ArtifactKind::parse(&kind)
        .ok_or_else(|| DeckError::Corrupt(format!("artifact kind {kind}")))?;
    Ok(a)
}

const INDEX_SELECT: &str = "SELECT a.station_id, COALESCE(t.name, a.station_id), a.artifact_id,
         a.session_id, a.title, a.kind, a.latest_version, a.updated_at
     FROM artifacts a LEFT JOIN stations t ON t.station_id = a.station_id";

/// The `LIKE` pattern of a search text, with the wildcards escaped.
fn like_pattern(q: &str) -> String {
    let mut out = String::from("%");
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

impl DeckStore {
    /// The index, newest first. `station` is a station id; `q` matches the
    /// title (case-insensitive).
    ///
    /// # Errors
    /// A database error.
    pub async fn artifacts(
        &self,
        station: Option<StationId>,
        session: Option<SessionId>,
        q: Option<String>,
    ) -> Result<Vec<DeckArtifact>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{INDEX_SELECT}
                 WHERE (?1 IS NULL OR a.station_id = ?1)
                   AND (?2 IS NULL OR a.session_id = ?2)
                   AND (?3 IS NULL OR a.title LIKE ?3 ESCAPE '\\')
                   AND {}
                 ORDER BY a.updated_at DESC, a.artifact_id DESC
                 LIMIT 1000",
                not_tombstoned!("a")
            ))?;
            let rows = stmt
                .query_map(
                    params![
                        station.as_ref().map(StationId::as_str),
                        session.as_ref().map(SessionId::as_str),
                        q.as_deref().map(like_pattern),
                    ],
                    index_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(with_kind).collect()
        })
        .await
    }

    /// One index row.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact(
        &self,
        station: StationId,
        artifact_id: String,
    ) -> Result<Option<DeckArtifact>> {
        call(&self.inner.db, move |conn| {
            conn.query_row(
                &format!(
                    "{INDEX_SELECT} WHERE a.station_id = ?1 AND a.artifact_id = ?2 AND {}",
                    not_tombstoned!("a")
                ),
                params![station.as_str(), artifact_id],
                index_row,
            )
            .optional()?
            .map(with_kind)
            .transpose()
        })
        .await
    }

    /// Stores the index row of a publish that the deck relayed, before its
    /// event arrives. It never lowers the latest version.
    ///
    /// # Errors
    /// A database error.
    pub async fn note_artifact(
        &self,
        station: StationId,
        a: &fleetdeck_proto::artifacts::Artifact,
    ) -> Result<()> {
        let a = a.clone();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            note_artifact_tx(&tx, &station, &a)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The stored versions of an artifact, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact_versions(
        &self,
        station: StationId,
        artifact_id: String,
    ) -> Result<Vec<ArtifactVersion>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(concat!(
                "SELECT version, size, sha256, files, created_at, capture FROM artifact_versions v
                     WHERE station_id = ?1 AND artifact_id = ?2 AND ",
                not_tombstoned!("v"),
                " ORDER BY version"
            ))?;
            let rows = stmt
                .query_map(params![station.as_str(), artifact_id], |r| {
                    Ok((
                        r.get::<_, u32>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(version, size, sha256, files, created_at, capture)| {
                    Ok(ArtifactVersion {
                        artifact_id: artifact_id.clone(),
                        version,
                        size: u64::try_from(size).unwrap_or(0),
                        sha256,
                        files: serde_json::from_str(&files)?,
                        created_at,
                        capture: capture.map(|c| serde_json::from_str(&c)).transpose()?,
                    })
                })
                .collect()
        })
        .await
    }

    /// Stores version metadata that a station reported.
    ///
    /// # Errors
    /// A database error.
    pub async fn put_artifact_versions(
        &self,
        station: StationId,
        versions: Vec<ArtifactVersion>,
    ) -> Result<()> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            put_artifact_versions_tx(&tx, &station, &versions)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The cached bytes of one file of a version.
    ///
    /// # Errors
    /// A database error.
    pub async fn cached_artifact_file(
        &self,
        station: StationId,
        artifact_id: String,
        version: u32,
        path: String,
    ) -> Result<Option<Vec<u8>>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let bytes: Option<Vec<u8>> = conn
                .query_row(
                    concat!(
                        "SELECT bytes FROM artifact_cache c
                         WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3
                           AND path = ?4 AND ",
                        not_tombstoned!("c")
                    ),
                    params![station.as_str(), artifact_id, version, path],
                    |r| r.get(0),
                )
                .optional()?;
            if bytes.is_some() {
                // The capture cache evicts by this time (R13 P5a).
                conn.execute(
                    "UPDATE artifact_cache SET last_used_at = ?5
                     WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3 AND path = ?4",
                    params![station.as_str(), artifact_id, version, path, now],
                )?;
            }
            Ok(bytes)
        })
        .await
    }

    /// The versions of an artifact that the cache holds completely: every
    /// file of the version's file list is cached. Newest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn cached_versions(
        &self,
        station: StationId,
        artifact_id: String,
    ) -> Result<Vec<u32>> {
        let versions = self
            .artifact_versions(station.clone(), artifact_id.clone())
            .await?;
        call(&self.inner.db, move |conn| {
            let mut out = Vec::new();
            for v in versions.iter().rev() {
                let have: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM artifact_cache
                     WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3",
                    params![station.as_str(), artifact_id, v.version],
                    |r| r.get(0),
                )?;
                if !v.files.is_empty() && usize::try_from(have).is_ok_and(|n| n == v.files.len()) {
                    out.push(v.version);
                }
            }
            Ok(out)
        })
        .await
    }

    /// Caches the bytes of one file of a version. A file of a capture
    /// version is a capture row and the capture rows stay within
    /// [`CAPTURE_CACHE_LIMIT`] (R13 P5a).
    ///
    /// # Errors
    /// A database error.
    pub async fn put_artifact_file(
        &self,
        station: StationId,
        artifact_id: String,
        version: u32,
        path: String,
        bytes: Vec<u8>,
    ) -> Result<()> {
        self.put_artifact_file_with_limit(
            station,
            artifact_id,
            version,
            path,
            bytes,
            CAPTURE_CACHE_LIMIT,
        )
        .await
    }

    /// [`Self::put_artifact_file`] with another capture-cache limit (a test
    /// makes it small).
    ///
    /// # Errors
    /// A database error.
    #[doc(hidden)]
    pub async fn put_artifact_file_with_limit(
        &self,
        station: StationId,
        artifact_id: String,
        version: u32,
        path: String,
        bytes: Vec<u8>,
        limit: u64,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            // The check and the write share one transaction (R13 P5a A4).
            if tombstoned(&tx, station.as_str(), &artifact_id)? {
                return Ok(());
            }
            cache_put_tx(
                &tx,
                (station.as_str(), &artifact_id, version, &path),
                &bytes,
                now,
                limit,
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Stores a share. The token itself is never stored. `None`: the
    /// artifact is tombstoned and nothing was stored (R13 P5a A4).
    ///
    /// # Errors
    /// A database error.
    pub async fn create_artifact_share(&self, new: NewShare) -> Result<Option<ShareRow>> {
        let now = self.now_ms();
        let id = share_id_of(&new.token_hash);
        let row = ShareRow {
            share_id: id.clone(),
            station_id: new.station_id.clone(),
            artifact_id: new.artifact_id.clone(),
            version: new.version,
            kind: if new.view { "view" } else { "share" }.to_owned(),
            created_by: new.created_by.clone(),
            created_at: now,
            expires_at: new.expires_at,
            revoked_at: None,
            views: 0,
        };
        let stored = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if tombstoned(&tx, new.station_id.as_str(), &new.artifact_id)? {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO artifact_shares (token_hash, share_id, station_id, artifact_id,
                     version, kind, created_by, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    new.token_hash,
                    id,
                    new.station_id.as_str(),
                    new.artifact_id,
                    new.version,
                    if new.view { "view" } else { "share" },
                    new.created_by,
                    now,
                    new.expires_at
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await?;
        Ok(stored.then_some(row))
    }

    /// The share of a token hash (any state).
    ///
    /// # Errors
    /// A database error.
    pub async fn share_by_hash(&self, token_hash: String) -> Result<Option<ShareRow>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {SHARE_COLUMNS} FROM artifact_shares s
                         WHERE token_hash = ?1 AND {}",
                        not_tombstoned!("s")
                    ),
                    [token_hash],
                    share_row,
                )
                .optional()?)
        })
        .await
    }

    /// The owner's shares of an artifact, newest first (not the board's
    /// view links).
    ///
    /// # Errors
    /// A database error.
    pub async fn shares_of(
        &self,
        station: StationId,
        artifact_id: String,
    ) -> Result<Vec<ShareView>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SHARE_COLUMNS} FROM artifact_shares s
                 WHERE station_id = ?1 AND artifact_id = ?2 AND kind = 'share' AND {}
                 ORDER BY created_at DESC, share_id",
                not_tombstoned!("s")
            ))?;
            let rows = stmt
                .query_map(params![station.as_str(), artifact_id], share_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .map(|s| ShareView {
                    share_id: s.share_id,
                    version: s.version,
                    created_at: s.created_at,
                    expires_at: s.expires_at,
                    revoked: s.revoked_at.is_some(),
                    views: s.views,
                })
                .collect())
        })
        .await
    }

    /// Revokes the owner's share of an artifact named by its token hash or
    /// its share id. Returns false for an unknown share; a revoked share
    /// stays revoked.
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_artifact_share(
        &self,
        station: StationId,
        artifact_id: String,
        token_hash: String,
        share_id: String,
    ) -> Result<bool> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let found: Option<String> = conn
                .query_row(
                    "SELECT token_hash FROM artifact_shares
                     WHERE station_id = ?1 AND artifact_id = ?2 AND kind = 'share'
                       AND (token_hash = ?3 OR share_id = ?4)",
                    params![station.as_str(), artifact_id, token_hash, share_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(found) = found else {
                return Ok(false);
            };
            conn.execute(
                "UPDATE artifact_shares SET revoked_at = COALESCE(revoked_at, ?2)
                 WHERE token_hash = ?1",
                params![found, now],
            )?;
            Ok(true)
        })
        .await
    }

    /// Counts one view of a share.
    ///
    /// # Errors
    /// A database error.
    pub async fn count_share_view(&self, token_hash: String) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                concat!(
                    "UPDATE artifact_shares SET views = views + 1
                     WHERE token_hash = ?1 AND ",
                    not_tombstoned!("artifact_shares")
                ),
                [token_hash],
            )?;
            Ok(())
        })
        .await
    }

    /// Deletes the expired view links (the board's own short links).
    ///
    /// # Errors
    /// A database error.
    pub async fn prune_view_links(&self) -> Result<usize> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "DELETE FROM artifact_shares WHERE kind = 'view' AND expires_at <= ?1",
                [now],
            )?)
        })
        .await
    }
}
