//! The captured transcripts of the session history (DL-189, table
//! `session_transcripts`) and the session facts that the history reads.

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{ProviderInstanceId, SessionId};
use rusqlite::{OptionalExtension, params};

use super::{Result, Store, call, log, to_i64, to_u64};

/// How a transcript was captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// Before the import resumed the conversation.
    Import,
    /// On the first read of an older import.
    Lazy,
    /// The capture did not work; `reason` says why.
    Failed,
}

impl Capture {
    fn name(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Lazy => "lazy",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "import" => Ok(Self::Import),
            "lazy" => Ok(Self::Lazy),
            "failed" => Ok(Self::Failed),
            other => Err(super::StoreError::Corrupt(format!(
                "capture kind `{other}`"
            ))),
        }
    }
}

/// One row of `session_transcripts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    pub session_id: SessionId,
    pub home: Option<String>,
    pub path: Option<String>,
    pub capture: Capture,
    pub reason: Option<String>,
    pub prefix_size: Option<u64>,
    pub prefix_sha256: Option<String>,
    pub snapshot: Option<String>,
    pub item_count: Option<u64>,
    pub captured_at: i64,
}

/// What the first effect of a session says about how it began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstEffect {
    /// `session.start`.
    Start,
    /// `session.import`.
    Import,
    /// No effect row (pruned), or another kind.
    Unknown,
}

/// The session facts that the history reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryFacts {
    pub provider: ProviderKind,
    pub provider_instance: ProviderInstanceId,
    pub cwd: String,
    pub native: Option<String>,
    /// `processes.started_at` of the first process, in Unix ms.
    pub first_process_at: Option<i64>,
    pub first_effect: FirstEffect,
}

impl Store {
    /// The transcript row of a session.
    ///
    /// # Errors
    /// A database error.
    pub async fn transcript(&self, session_id: SessionId) -> Result<Option<TranscriptRow>> {
        call(&self.inner.db, move |conn| {
            let row = conn
                .query_row(
                    "SELECT home, path, capture, reason, prefix_size, prefix_sha256, snapshot,
                            item_count, captured_at
                     FROM session_transcripts WHERE session_id = ?1",
                    params![session_id.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<i64>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                            r.get::<_, Option<String>>(6)?,
                            r.get::<_, Option<i64>>(7)?,
                            r.get::<_, i64>(8)?,
                        ))
                    },
                )
                .optional()?;
            let Some((home, path, capture, reason, size, sha, snapshot, count, at)) = row else {
                return Ok(None);
            };
            Ok(Some(TranscriptRow {
                session_id,
                home,
                path,
                capture: Capture::parse(&capture)?,
                reason,
                prefix_size: size.map(to_u64).transpose()?,
                prefix_sha256: sha,
                snapshot,
                item_count: count.map(to_u64).transpose()?,
                captured_at: at,
            }))
        })
        .await
    }

    /// Stores a transcript row unless the session has one. True when this
    /// call stored it.
    ///
    /// # Errors
    /// A database error.
    pub async fn put_transcript(&self, row: TranscriptRow) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let size = row.prefix_size.map(to_i64).transpose()?;
            let count = row.item_count.map(to_i64).transpose()?;
            let n = conn.execute(
                "INSERT OR IGNORE INTO session_transcripts
                   (session_id, home, path, capture, reason, prefix_size, prefix_sha256,
                    snapshot, item_count, captured_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    row.session_id.as_str(),
                    row.home,
                    row.path,
                    row.capture.name(),
                    row.reason,
                    size,
                    row.prefix_sha256,
                    row.snapshot,
                    count,
                    row.captured_at,
                ],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// Stores the item count of a transcript that has none yet.
    ///
    /// # Errors
    /// A database error.
    pub async fn set_transcript_items(&self, session_id: SessionId, count: u64) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE session_transcripts SET item_count = ?2
                 WHERE session_id = ?1 AND item_count IS NULL",
                params![session_id.as_str(), to_i64(count)?],
            )?;
            Ok(())
        })
        .await
    }

    /// The facts of a session for the history; `None` for an unknown
    /// session or one without a working directory.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn history_facts(&self, session_id: SessionId) -> Result<Option<HistoryFacts>> {
        call(&self.inner.db, move |conn| {
            let Some(summary) = log::summary(conn, &session_id)? else {
                return Ok(None);
            };
            let (Some(provider), Some(provider_instance), Some(cwd)) =
                (summary.provider, summary.provider_instance, summary.cwd)
            else {
                return Ok(None);
            };
            let native: Option<String> = conn
                .query_row(
                    "SELECT native_session_id FROM sessions WHERE session_id = ?1",
                    params![session_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let first_process_at: Option<i64> = conn
                .query_row(
                    "SELECT MIN(started_at) FROM processes WHERE session_id = ?1",
                    params![session_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let first_effect = first_effect(conn, &session_id)?;
            Ok(Some(HistoryFacts {
                provider,
                provider_instance,
                cwd,
                native,
                first_process_at,
                first_effect,
            }))
        })
        .await
    }
}

/// The first `start` or `resume` effect of a session by `seq`: how the
/// session began its conversation. The history and the usage recount share
/// this rule (B20).
///
/// # Errors
/// A database or JSON error.
pub(super) fn first_effect(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<FirstEffect> {
    let first: Option<(String, String)> = conn
        .query_row(
            "SELECT kind, payload FROM effects WHERE session_id = ?1
               AND kind IN ('start', 'resume') ORDER BY seq LIMIT 1",
            params![session.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match first {
        Some((kind, _)) if kind == "start" => FirstEffect::Start,
        Some((kind, payload)) if kind == "resume" => {
            let import = serde_json::from_str::<serde_json::Value>(&payload)?
                .get("import")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if import {
                FirstEffect::Import
            } else {
                FirstEffect::Unknown
            }
        }
        _ => FirstEffect::Unknown,
    })
}

#[cfg(test)]
impl Store {
    /// Runs SQL on the database (tests only).
    pub(crate) async fn exec_for_tests(&self, sql: String) {
        call(&self.inner.db, move |conn| {
            conn.execute_batch(&sql)?;
            Ok(())
        })
        .await
        .unwrap();
    }
}
