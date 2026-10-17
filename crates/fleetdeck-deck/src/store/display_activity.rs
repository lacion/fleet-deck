//! The activity of a wall display (R13 §5A item 1, §5B item 1, §5D items 8
//! and 17, R9 Q8): a projection built from fixed labels and the few allowed
//! fields. It has no mail row, no plan row, no tool input, no approval
//! detail and no free end reason.
//!
//! The scope applies before the pagination. The deck first reads the
//! sessions that the scope allows (stations, repository basename, the
//! `repo=` and `session=` queries), then it reads the timeline rows of
//! those sessions only, newest first, with the limit.

use std::collections::HashMap;

use fleetdeck_proto::board::{ActivityItem, ActivityKind, ActivityList};
use fleetdeck_proto::event::Event;
use fleetdeck_proto::{EventEnvelope, SessionId, StationId};
use rusqlite::params;

use super::display_timeline::{safe_tool_name, tool_name_db};
use super::{KINDS_DISPLAY, one_line, status_word};
use crate::http::display_view::{last_component, request_label};
use crate::store::display_directives::EffectiveScope;
use crate::store::{DeckStore, Result, call, to_u64};

/// The filter of [`DeckStore::display_activity`].
#[derive(Debug, Clone)]
pub struct DisplayActivityQuery {
    /// What the display may read now.
    pub scope: EffectiveScope,
    /// Only rows with a time above this Unix ms.
    pub since: Option<i64>,
    /// Only sessions whose displayed repository basename equals this.
    pub repo: Option<String>,
    /// Only this session.
    pub session: Option<String>,
    pub limit: u32,
}

/// The text of a row: fixed labels, never an input.
fn describe(event: &Event) -> Option<(ActivityKind, String)> {
    Some(match event {
        Event::MessageCompleted { text, .. } => (ActivityKind::Message, one_line(text)),
        Event::ApprovalRequested { request_kind, .. } => (
            ActivityKind::Approval,
            format!("Needs you: {}", request_label(*request_kind)),
        ),
        Event::ApprovalResolved { .. } => (ActivityKind::Approval, "Approval answered".to_owned()),
        Event::ApprovalExpired { .. } => (ActivityKind::Approval, "Approval expired".to_owned()),
        Event::SessionStarted { .. } => (ActivityKind::Lifecycle, "Session started".to_owned()),
        Event::SessionEnded { .. } => (ActivityKind::Lifecycle, "Session ended".to_owned()),
        Event::SessionHibernated { after_min } => (
            ActivityKind::Lifecycle,
            format!("Hibernated after {after_min} min idle"),
        ),
        _ => return None,
    })
}

/// A session that the scope allows: its key, its title and its displayed
/// repository basename.
struct Allowed {
    station: String,
    session: String,
    title: Option<String>,
    repo: Option<String>,
}

impl DeckStore {
    /// The activity of a display, newest first, at most `query.limit` rows.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_activity(&self, query: DisplayActivityQuery) -> Result<ActivityList> {
        let limit = i64::from(query.limit);
        call(&self.inner.db, move |conn| {
            let stations: Vec<&str> = query.scope.stations.iter().map(StationId::as_str).collect();
            let stations = serde_json::to_string(&stations)?;
            let rows: Vec<(String, String, Option<String>, Option<String>)> = conn
                .prepare(
                    "SELECT s.station_id, s.session_id,
                            json_extract(s.summary, '$.title'),
                            json_extract(s.summary, '$.repo.root')
                     FROM sessions s
                     JOIN stations t ON t.station_id = s.station_id
                     WHERE s.station_id IN (SELECT value FROM json_each(?1))",
                )?
                .query_map(params![stations], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let allowed: Vec<Allowed> = rows
                .into_iter()
                .map(|(station, session, title, root)| Allowed {
                    station,
                    session,
                    title,
                    repo: root.map(|r| last_component(&r)).filter(|r| !r.is_empty()),
                })
                .filter(|a| {
                    query
                        .scope
                        .repo
                        .as_ref()
                        .is_none_or(|r| a.repo.as_ref() == Some(r))
                        && query
                            .repo
                            .as_ref()
                            .is_none_or(|r| a.repo.as_ref() == Some(r))
                        && query.session.as_ref().is_none_or(|s| &a.session == s)
                })
                .collect();
            if allowed.is_empty() {
                return Ok(ActivityList { items: Vec::new() });
            }
            let by_key: HashMap<String, &Allowed> = allowed
                .iter()
                .map(|a| (format!("{}\u{1f}{}", a.station, a.session), a))
                .collect();
            let keys: Vec<&String> = by_key.keys().collect();
            let keys = serde_json::to_string(&keys)?;
            let since = query.since.unwrap_or(i64::MIN);
            let mut stmt = conn.prepare(&format!(
                "SELECT t.station_id, t.session_id, t.station_seq, t.at, t.envelope
                 FROM timeline t
                 WHERE t.kind IN {KINDS_DISPLAY} AND t.at > ?1
                   AND (t.station_id || char(31) || t.session_id)
                       IN (SELECT value FROM json_each(?2))
                 ORDER BY t.at DESC, t.station_seq DESC LIMIT ?3"
            ))?;
            let rows = stmt.query_map(params![since, keys, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut items = Vec::new();
            for row in rows {
                let (station, session, seq, at, envelope) = row?;
                let env: EventEnvelope = serde_json::from_str(&envelope)?;
                let Some(event) = env.event.known() else {
                    continue;
                };
                let described = match event {
                    Event::ToolCompleted {
                        node_id, status, ..
                    } => {
                        let name = tool_name_db(conn, &station, &session, node_id.as_str())?
                            .unwrap_or_else(|| safe_tool_name("Tool"));
                        Some((
                            ActivityKind::Tool,
                            format!("{name} ({})", status_word(*status)),
                        ))
                    }
                    other => describe(other),
                };
                let Some((kind, text)) = described else {
                    continue;
                };
                let meta = by_key.get(&format!("{station}\u{1f}{session}")).copied();
                items.push(ActivityItem {
                    station_id: StationId::from(station),
                    session_id: SessionId::from(session),
                    session_title: meta.and_then(|a| a.title.clone()),
                    repo: meta.and_then(|a| a.repo.clone()),
                    kind,
                    text,
                    at,
                    station_seq: Some(to_u64(seq)?),
                });
            }
            Ok(ActivityList { items })
        })
        .await
    }
}
