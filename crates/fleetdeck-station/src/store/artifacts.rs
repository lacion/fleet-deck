//! The artifact index of a station (R12, DL-165).
//!
//! [`Store::artifact_commit`] makes one version in one transaction: the
//! version rows, the `artifact.published` event of the session and the move
//! of the staged bytes to `<artifacts dir>/<artifact_id>/<version>/`. The
//! move runs before the commit, inside the transaction: if it fails, nothing
//! is stored. A directory that an earlier crash left at the target path
//! belongs to a version that was never committed, so it is replaced.

use std::path::{Path, PathBuf};

use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::event::Event;
use fleetdeck_proto::{EventBody, SessionId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{NewEvent, Result, Store, StoreError, call, log, to_i64};

/// The most suggestion rows that one session keeps (NV-02).
const MAX_SUGGESTIONS_PER_SESSION: i64 = 200;

/// One file of a staged bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// Relative to the bundle root, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
}

/// A bundle that the station copied into a staging directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The staging directory; the files are below it.
    pub dir: PathBuf,
    /// The main file first.
    pub files: Vec<StagedFile>,
}

impl Staged {
    fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The hash of the bundle: each path and file hash, sorted by path.
    #[must_use]
    pub fn manifest_sha256(&self) -> String {
        let mut rows: Vec<&StagedFile> = self.files.iter().collect();
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        for f in rows {
            ctx.update(f.path.as_bytes());
            ctx.update(&[0]);
            ctx.update(f.sha256.as_bytes());
            ctx.update(b"\n");
        }
        hex(ctx.finish().as_ref())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One version to commit.
#[derive(Debug, Clone)]
pub struct NewVersion {
    pub staged: Staged,
    /// `<station data>/artifacts`.
    pub root: PathBuf,
    pub session_id: Option<SessionId>,
    /// The title to set; `None` keeps the title of an existing artifact.
    pub title: Option<String>,
    /// The title of a new artifact when `title` is `None`.
    pub default_title: String,
    /// A new version of this artifact.
    pub artifact_id: Option<String>,
    pub author: String,
}

/// What [`Store::artifact_commit`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub artifact: Artifact,
    pub version: ArtifactVersion,
    /// False when the content equals the latest version.
    pub created: bool,
}

/// Why a commit did not happen.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error("unknown artifact {0}")]
    UnknownArtifact(String),
    #[error("unknown session {0}")]
    UnknownSession(SessionId),
    #[error("the artifact belongs to another session")]
    NotOwner,
    #[error("the bytes could not be stored: {0}")]
    Io(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn artifact_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Artifact, String)> {
    let session: Option<String> = r.get(1)?;
    let kind: String = r.get(3)?;
    Ok((
        Artifact {
            artifact_id: r.get(0)?,
            station_id: String::new(),
            session_id: session.map(SessionId::from),
            title: r.get(2)?,
            kind: ArtifactKind::Other,
            latest_version: r.get(4)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
            author: r.get(5)?,
        },
        kind,
    ))
}

const ARTIFACT_COLUMNS: &str =
    "artifact_id, session_id, title, kind, latest_version, author, created_at, updated_at";

fn load_artifact(conn: &Connection, id: &str) -> Result<Option<Artifact>> {
    let row = conn
        .query_row(
            &format!("SELECT {ARTIFACT_COLUMNS} FROM artifacts WHERE artifact_id = ?1"),
            [id],
            artifact_of,
        )
        .optional()?;
    row.map(|(mut a, kind)| {
        a.kind = ArtifactKind::parse(&kind)
            .ok_or_else(|| StoreError::Corrupt(format!("artifact kind {kind}")))?;
        Ok(a)
    })
    .transpose()
}

fn version_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<(ArtifactVersion, String)> {
    let files: String = r.get(4)?;
    Ok((
        ArtifactVersion {
            artifact_id: r.get(0)?,
            version: r.get(1)?,
            size: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
            sha256: r.get(3)?,
            files: Vec::new(),
            created_at: r.get(5)?,
        },
        files,
    ))
}

const VERSION_COLUMNS: &str = "artifact_id, version, size, sha256, files, created_at";

fn finish_version((mut v, files): (ArtifactVersion, String)) -> Result<ArtifactVersion> {
    v.files = serde_json::from_str(&files)?;
    Ok(v)
}

fn load_versions(conn: &Connection, id: &str) -> Result<Vec<ArtifactVersion>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERSION_COLUMNS} FROM artifact_versions WHERE artifact_id = ?1 ORDER BY version"
    ))?;
    let rows = stmt
        .query_map([id], version_of)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(finish_version).collect()
}

fn move_staged(staged: &Path, target: &Path) -> std::io::Result<()> {
    if target.exists() {
        std::fs::remove_dir_all(target)?;
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(staged, target)
}

impl Store {
    /// Commits one version of an artifact (new artifact when
    /// `artifact_id` is `None`). When `owner_session` is `Some` and
    /// `artifact_id` names an artifact, the artifact must belong to that
    /// session; the check runs inside the commit transaction.
    ///
    /// # Errors
    /// [`CommitError`]. On every error the staging directory stays for the
    /// caller to remove; on success it is gone (moved).
    pub async fn artifact_commit(
        &self,
        new: NewVersion,
        owner_session: Option<SessionId>,
    ) -> std::result::Result<Committed, CommitError> {
        let now = self.now();
        let fresh = self.inner.ids.next_ulid().to_string();
        let out = call(&self.inner.db, move |conn| {
            Ok(commit(conn, &new, owner_session.as_ref(), &fresh, now))
        })
        .await?;
        if let Ok((_, Some(seq))) = &out {
            self.publish(*seq);
        }
        out.map(|(c, _)| c)
    }

    /// One artifact with its versions, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact_detail(
        &self,
        artifact_id: String,
    ) -> Result<Option<(Artifact, Vec<ArtifactVersion>)>> {
        call(&self.inner.db, move |conn| {
            let Some(artifact) = load_artifact(conn, &artifact_id)? else {
                return Ok(None);
            };
            let versions = load_versions(conn, &artifact_id)?;
            Ok(Some((artifact, versions)))
        })
        .await
    }

    /// The artifacts of a session, or of the station, newest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifacts_of(&self, session: Option<SessionId>) -> Result<Vec<Artifact>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ARTIFACT_COLUMNS} FROM artifacts
                 WHERE ?1 IS NULL OR session_id = ?1
                 ORDER BY updated_at DESC, artifact_id DESC"
            ))?;
            let rows = stmt
                .query_map([session.as_ref().map(SessionId::as_str)], artifact_of)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(mut a, kind)| {
                    a.kind = ArtifactKind::parse(&kind)
                        .ok_or_else(|| StoreError::Corrupt(format!("artifact kind {kind}")))?;
                    Ok(a)
                })
                .collect()
        })
        .await
    }

    /// Records a suggestion for `path` and appends `artifact.suggested`
    /// when this session had none for the path. Returns true if it did. A
    /// session that already has 200 suggestions gets no new row and no
    /// event, the same as a repeated path (NV-02).
    ///
    /// # Errors
    /// A database error, or an unknown session.
    pub async fn artifact_suggest(&self, session_id: SessionId, path: String) -> Result<bool> {
        let now = self.now();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let held: i64 = tx.query_row(
                "SELECT COUNT(*) FROM artifact_suggestions WHERE session_id = ?1",
                [session_id.as_str()],
                |r| r.get(0),
            )?;
            if held >= MAX_SUGGESTIONS_PER_SESSION {
                return Ok(None);
            }
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO artifact_suggestions (session_id, path, at)
                 VALUES (?1, ?2, ?3)",
                params![session_id.as_str(), path, now],
            )?;
            if inserted == 0 {
                return Ok(None);
            }
            let event = NewEvent::from(Event::ArtifactSuggested { path });
            let env = log::append_one(&tx, &session_id, event, now)?;
            tx.commit()?;
            Ok(Some(env.station_seq))
        })
        .await?;
        if let Some(seq) = out {
            self.publish(seq);
        }
        Ok(out.is_some())
    }
}

/// The commit transaction. The inner result carries the domain errors; the
/// outer one is for the database.
fn commit(
    conn: &mut Connection,
    new: &NewVersion,
    owner_session: Option<&SessionId>,
    fresh_id: &str,
    now: i64,
) -> std::result::Result<(Committed, Option<u64>), CommitError> {
    let tx = conn.transaction().map_err(StoreError::from)?;
    if let Some(session) = &new.session_id {
        let known: bool = tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sessions WHERE session_id = ?1)",
                [session.as_str()],
                |r| r.get(0),
            )
            .map_err(StoreError::from)?;
        if !known {
            return Err(CommitError::UnknownSession(session.clone()));
        }
    }
    let manifest = new.staged.manifest_sha256();
    let main = new
        .staged
        .files
        .first()
        .ok_or_else(|| CommitError::Io("the bundle has no main file".to_owned()))?;
    let kind = ArtifactKind::of_path(&main.path);
    let (artifact_id, version, existing) = match &new.artifact_id {
        Some(id) => {
            let existing =
                load_artifact(&tx, id)?.ok_or_else(|| CommitError::UnknownArtifact(id.clone()))?;
            if owner_session.is_some_and(|owner| existing.session_id.as_ref() != Some(owner)) {
                return Err(CommitError::NotOwner);
            }
            let latest: Option<(String, String)> = tx
                .query_row(
                    "SELECT manifest_sha256, files FROM artifact_versions
                     WHERE artifact_id = ?1 AND version = ?2",
                    params![id, existing.latest_version],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(StoreError::from)?;
            if latest.is_some_and(|(m, _)| m == manifest) {
                let versions = load_versions(&tx, id)?;
                let version = versions
                    .into_iter()
                    .find(|v| v.version == existing.latest_version)
                    .ok_or_else(|| StoreError::Corrupt("latest version row".to_owned()))?;
                return Ok((
                    Committed {
                        artifact: existing,
                        version,
                        created: false,
                    },
                    None,
                ));
            }
            let next = existing.latest_version + 1;
            (id.clone(), next, Some(existing))
        }
        None => (fresh_id.to_owned(), 1, None),
    };
    let files: Vec<String> = new.staged.files.iter().map(|f| f.path.clone()).collect();
    let size = new.staged.size();
    let title = new
        .title
        .clone()
        .or_else(|| existing.as_ref().map(|a| a.title.clone()))
        .unwrap_or_else(|| new.default_title.clone());
    let artifact = match existing {
        Some(mut a) => {
            tx.execute(
                "UPDATE artifacts SET title = ?2, kind = ?3, latest_version = ?4, updated_at = ?5
                 WHERE artifact_id = ?1",
                params![artifact_id, title, kind.as_str(), version, now],
            )
            .map_err(StoreError::from)?;
            a.title = title.clone();
            a.kind = kind;
            a.latest_version = version;
            a.updated_at = now;
            a
        }
        None => {
            tx.execute(
                "INSERT INTO artifacts (artifact_id, session_id, title, kind, latest_version,
                     author, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6)",
                params![
                    artifact_id,
                    new.session_id.as_ref().map(SessionId::as_str),
                    title,
                    kind.as_str(),
                    new.author,
                    now
                ],
            )
            .map_err(StoreError::from)?;
            Artifact {
                artifact_id: artifact_id.clone(),
                station_id: String::new(),
                session_id: new.session_id.clone(),
                title: title.clone(),
                kind,
                latest_version: 1,
                created_at: now,
                updated_at: now,
                author: new.author.clone(),
            }
        }
    };
    // The version row refers to the artifact row, so it comes second.
    tx.execute(
        "INSERT INTO artifact_versions
             (artifact_id, version, size, sha256, manifest_sha256, files, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            artifact_id,
            version,
            to_i64(size)?,
            main.sha256,
            manifest,
            serde_json::to_string(&files).map_err(StoreError::from)?,
            now
        ],
    )
    .map_err(StoreError::from)?;
    let seq = match &artifact.session_id {
        Some(session) => {
            let event = NewEvent {
                event: EventBody::Known(Event::ArtifactPublished {
                    artifact_id: artifact_id.clone(),
                    version,
                    title,
                    artifact_kind: kind,
                }),
                raw_ref: None,
            };
            Some(log::append_one(&tx, session, event, now)?.station_seq)
        }
        None => None,
    };
    let target = new.root.join(&artifact_id).join(version.to_string());
    move_staged(&new.staged.dir, &target).map_err(|e| CommitError::Io(e.to_string()))?;
    tx.commit().map_err(StoreError::from)?;
    let version = ArtifactVersion {
        artifact_id,
        version,
        size,
        sha256: main.sha256.clone(),
        files,
        created_at: now,
    };
    Ok((
        Committed {
            artifact,
            version,
            created: true,
        },
        seq,
    ))
}
