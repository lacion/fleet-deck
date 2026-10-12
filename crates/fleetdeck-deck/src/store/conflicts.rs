//! The conflict radar across stations (M3 6-7, DL-306).
//!
//! [`record_touches`] runs in the read-model transaction of `apply_events`
//! and keeps the latest touch of each file by each session (`file.changed`
//! and the `files` of `tool.completed`). [`DeckStore::conflicts`] reports
//! the files that two or more sessions touched inside the conflict window.

use std::collections::BTreeMap;

use fleetdeck_proto::conflicts::{
    CONFLICT_WINDOW_MS, FileConflict, FileTouch, MAX_CONFLICTS, touch_key,
};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::{EventEnvelope, SessionSummary, StationId};
use rusqlite::{Transaction, params};

use super::{DeckStore, Result, call};

/// The most files of one event that the radar records.
const MAX_FILES_PER_EVENT: usize = 64;
/// The longest path that the radar records.
const MAX_PATH_BYTES: usize = 1024;

/// Records the files that an event changed; other events do nothing.
pub(super) fn record_touches(
    tx: &Transaction<'_>,
    station: &StationId,
    e: &EventEnvelope,
    summary: &SessionSummary,
) -> Result<()> {
    let paths: Vec<&str> = match &e.event {
        EventBody::Known(Event::FileChanged { path, .. }) => vec![path.as_str()],
        EventBody::Known(Event::ToolCompleted { files, .. }) => files
            .iter()
            .take(MAX_FILES_PER_EVENT)
            .map(String::as_str)
            .collect(),
        _ => return Ok(()),
    };
    for path in paths {
        if path.is_empty() || path.len() > MAX_PATH_BYTES {
            continue;
        }
        let (group, repo, file) =
            touch_key(station, summary.cwd.as_deref(), summary.repo.as_ref(), path);
        tx.execute(
            "INSERT INTO file_touches (group_key, file, repo, station_id, session_id,
                 session_title, path, touched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (group_key, file, station_id, session_id) DO UPDATE SET
                 session_title = excluded.session_title, path = excluded.path,
                 touched_at = max(touched_at, excluded.touched_at)",
            params![
                group,
                file,
                repo,
                station.as_str(),
                e.session_id.as_str(),
                summary.title,
                path,
                e.at,
            ],
        )?;
    }
    // Touches far outside the window are never read again.
    tx.execute(
        "DELETE FROM file_touches WHERE touched_at < ?1",
        [e.at.saturating_sub(2 * CONFLICT_WINDOW_MS)],
    )?;
    Ok(())
}

impl DeckStore {
    /// The files that two or more sessions touched in the last
    /// [`CONFLICT_WINDOW_MS`], the newest conflict first.
    ///
    /// # Errors
    /// A database error.
    pub async fn conflicts(&self) -> Result<Vec<FileConflict>> {
        let since = self.now_ms().saturating_sub(CONFLICT_WINDOW_MS);
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT group_key, file, repo, station_id, session_id, session_title, path,
                     touched_at
                 FROM file_touches WHERE touched_at >= ?1
                 ORDER BY touched_at DESC, station_id, session_id",
            )?;
            let mut groups: BTreeMap<(String, String), FileConflict> = BTreeMap::new();
            let rows = stmt.query_map([since], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    FileTouch {
                        station_id: StationId::from(r.get::<_, String>(3)?),
                        session_id: r.get::<_, String>(4)?.into(),
                        session_title: r.get(5)?,
                        path: r.get(6)?,
                        touched_at: r.get(7)?,
                    },
                ))
            })?;
            for row in rows {
                let (group, file, repo, touch) = row?;
                groups
                    .entry((group, file.clone()))
                    .or_insert_with(|| FileConflict {
                        repo,
                        file,
                        touches: Vec::new(),
                    })
                    .touches
                    .push(touch);
            }
            let mut out: Vec<FileConflict> = groups
                .into_values()
                .filter(|c| c.touches.len() >= 2)
                .collect();
            out.sort_by_key(|c| std::cmp::Reverse(c.touches.first().map_or(0, |t| t.touched_at)));
            out.truncate(MAX_CONFLICTS);
            Ok(out)
        })
        .await
    }
}
