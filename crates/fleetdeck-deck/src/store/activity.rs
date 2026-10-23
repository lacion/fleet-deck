//! The activity stream (R6.5, DL-169): the newest rows of the stored
//! timelines and of the mail, merged across the fleet. A row is an agent
//! message, a completed tool call, an approval, mail or a session lifecycle
//! change. The rows come from the condensed timeline that the deck keeps
//! (at most [`super::TIMELINE_CAP`] rows per session), so the stream reaches
//! back as far as those do.

use fleetdeck_proto::board::{ACTIVITY_TEXT_CHARS, ActivityItem, ActivityKind, ActivityList};
use fleetdeck_proto::event::{Event, OptionKind, RequestKind, ToolStatus};
use fleetdeck_proto::{EventEnvelope, SessionId, StationId};
use rusqlite::{Connection, params};

use super::{DeckStore, Result, call, to_u64};

#[path = "display_activity.rs"]
pub mod display_activity;
#[path = "display_timeline.rs"]
pub mod display_timeline;

/// The timeline kinds of the stream.
const KINDS: &str = "('message.completed', 'plan.proposed', 'tool.completed', \
    'approval.requested', 'approval.resolved', 'approval.expired', \
    'session.started', 'session.ended', 'session.hibernated')";

/// The timeline kinds of the stream of a display: no plan row (R13 §5D item 8).
const KINDS_DISPLAY: &str = "('message.completed', 'tool.completed', \
    'approval.requested', 'approval.resolved', 'approval.expired', \
    'session.started', 'session.ended', 'session.hibernated')";

/// The filter of [`DeckStore::activity`].
#[derive(Debug, Clone, Default)]
pub struct ActivityQuery {
    /// Only rows with a time above this Unix ms.
    pub since: Option<i64>,
    /// Only sessions of this repository (its origin or its root).
    pub repo: Option<String>,
    /// Only this session.
    pub session: Option<String>,
    pub limit: u32,
}

/// The first line of `text`, cut to [`ACTIVITY_TEXT_CHARS`] characters.
fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= ACTIVITY_TEXT_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(ACTIVITY_TEXT_CHARS - 1).collect();
    cut.push('…');
    cut
}

fn status_word(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Ok => "ok",
        ToolStatus::Failed => "failed",
        ToolStatus::Denied => "denied",
    }
}

/// The words of the end of a session for the owner: a known reason in
/// words, every other reason (`process_exit`, an exit cause, a move) none.
/// The wall says "Session ended" too (`display_activity`).
fn ended_text(reason: &str) -> String {
    match reason {
        "start_failed" => "Session ended: the start failed".to_owned(),
        "handover" => "Session ended: handed over".to_owned(),
        _ => "Session ended".to_owned(),
    }
}

/// The words of an answered request: the words of the approval card for
/// the kind of the chosen option ("Allowed", "Denied"), "Answered" for a
/// question or an option of no known kind. The wall says "Answered" for
/// every answer (no approval detail on a display).
fn answer_text(request: Option<RequestKind>, option: Option<OptionKind>) -> &'static str {
    if request == Some(RequestKind::Question) {
        return "Answered";
    }
    match option {
        Some(OptionKind::AllowOnce) => "Allowed",
        Some(OptionKind::AllowSession) => "Allowed for this session",
        Some(OptionKind::AllowAlways) => "Always allowed",
        Some(OptionKind::Amend) => "Allowed with changes",
        Some(OptionKind::Deny) => "Denied",
        Some(OptionKind::Other) | None => "Answered",
    }
}

/// The text of the `approval.resolved` row of `request_id`: the kind of the
/// request and of its chosen option come from its `approval.requested` row.
fn resolved_text(
    conn: &Connection,
    station: &str,
    session: &str,
    request_id: &str,
    option_id: &str,
) -> Result<&'static str> {
    let mut stmt = conn.prepare_cached(
        "SELECT envelope FROM timeline
         WHERE station_id = ?1 AND session_id = ?2 AND kind = 'approval.requested'
           AND json_extract(envelope, '$.event.request_id') = ?3
         ORDER BY station_seq DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![station, session, request_id])?;
    let Some(row) = rows.next()? else {
        return Ok(answer_text(None, None));
    };
    let env: EventEnvelope = serde_json::from_str(&row.get::<_, String>(0)?)?;
    Ok(match env.event.known() {
        Some(Event::ApprovalRequested {
            request_kind,
            options,
            ..
        }) => answer_text(
            Some(*request_kind),
            options
                .iter()
                .find(|o| o.option_id == option_id)
                .and_then(|o| o.kind),
        ),
        _ => answer_text(None, None),
    })
}

/// The kind and the text of a timeline event; `None` for a kind that the
/// stream does not show. A tool call gets its title, and an answer its
/// words, in `activity` (they read other rows).
fn describe(event: &Event) -> Option<(ActivityKind, String)> {
    Some(match event {
        Event::MessageCompleted { text, .. } => (ActivityKind::Message, one_line(text)),
        Event::PlanProposed { .. } => (ActivityKind::Message, "Proposed a plan".to_owned()),
        Event::ToolCompleted { status, .. } => (
            ActivityKind::Tool,
            format!("Tool call ({})", status_word(*status)),
        ),
        Event::ApprovalRequested { title, .. } => (
            ActivityKind::Approval,
            one_line(&format!("Needs you: {title}")),
        ),
        Event::ApprovalResolved { .. } => {
            (ActivityKind::Approval, answer_text(None, None).to_owned())
        }
        Event::ApprovalExpired { .. } => (ActivityKind::Approval, "Request expired".to_owned()),
        Event::SessionStarted { .. } => (ActivityKind::Lifecycle, "Session started".to_owned()),
        Event::SessionEnded { reason, .. } => (ActivityKind::Lifecycle, ended_text(reason)),
        Event::SessionHibernated { after_min } => (
            ActivityKind::Lifecycle,
            format!("Hibernated after {after_min} min idle"),
        ),
        _ => return None,
    })
}

/// The kind of the request of an `approval.requested` event; `None` for every
/// other event.
fn request_kind_of(event: &Event) -> Option<RequestKind> {
    match event {
        Event::ApprovalRequested { request_kind, .. } => Some(*request_kind),
        _ => None,
    }
}

/// The title of the tool call `node_id` of a session, from its
/// `tool.started` row: `Title: input summary`.
fn tool_title(
    conn: &Connection,
    station: &str,
    session: &str,
    node_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT envelope FROM timeline
         WHERE station_id = ?1 AND session_id = ?2 AND kind = 'tool.started'
           AND json_extract(envelope, '$.event.node_id') = ?3
         ORDER BY station_seq DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![station, session, node_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let env: EventEnvelope = serde_json::from_str(&row.get::<_, String>(0)?)?;
    Ok(match env.event.known() {
        Some(Event::ToolStarted {
            title,
            input_summary,
            ..
        }) => Some(if input_summary.is_empty() {
            title.clone()
        } else {
            format!("{title}: {input_summary}")
        }),
        _ => None,
    })
}

impl DeckStore {
    /// The activity stream, newest first, at most `query.limit` rows.
    ///
    /// # Errors
    /// A database error.
    pub async fn activity(&self, query: ActivityQuery) -> Result<ActivityList> {
        let limit = i64::from(query.limit);
        call(&self.inner.db, move |conn| {
            let since = query.since.unwrap_or(i64::MIN);
            let mut items = Vec::new();
            let mut stmt = conn.prepare(&format!(
                "SELECT t.station_id, t.session_id, t.station_seq, t.at, t.envelope,
                        json_extract(s.summary, '$.title'),
                        COALESCE(json_extract(s.summary, '$.repo.origin'),
                                 json_extract(s.summary, '$.repo.root'))
                 FROM timeline t
                 JOIN sessions s ON s.station_id = t.station_id AND s.session_id = t.session_id
                 WHERE t.kind IN {KINDS} AND t.at > ?1
                   AND (?2 IS NULL OR t.session_id = ?2)
                   AND (?3 IS NULL OR json_extract(s.summary, '$.repo.origin') = ?3
                        OR json_extract(s.summary, '$.repo.root') = ?3)
                 ORDER BY t.at DESC, t.station_seq DESC LIMIT ?4"
            ))?;
            let rows = stmt.query_map(params![since, query.session, query.repo, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })?;
            for row in rows {
                let (station, session, seq, at, envelope, title, repo) = row?;
                let env: EventEnvelope = serde_json::from_str(&envelope)?;
                let Some((kind, mut text)) = env.event.known().and_then(describe) else {
                    continue;
                };
                if let Some(Event::ToolCompleted {
                    node_id, status, ..
                }) = env.event.known()
                    && let Some(name) = tool_title(conn, &station, &session, node_id.as_str())?
                {
                    text = one_line(&format!("{name} ({})", status_word(*status)));
                }
                if let Some(Event::ApprovalResolved {
                    request_id,
                    option_id,
                    ..
                }) = env.event.known()
                {
                    text = resolved_text(conn, &station, &session, request_id.as_str(), option_id)?
                        .to_owned();
                }
                items.push(ActivityItem {
                    station_id: StationId::from(station),
                    session_id: SessionId::from(session),
                    session_title: title,
                    repo,
                    kind,
                    request_kind: env.event.known().and_then(request_kind_of),
                    text,
                    at,
                    station_seq: Some(to_u64(seq)?),
                });
            }

            let mut stmt = conn.prepare(
                "SELECT m.station_id, m.session_id, m.text, m.created_at,
                        json_extract(s.summary, '$.title'),
                        COALESCE(json_extract(s.summary, '$.repo.origin'),
                                 json_extract(s.summary, '$.repo.root'))
                 FROM mail m
                 JOIN sessions s ON s.station_id = m.station_id AND s.session_id = m.session_id
                 WHERE m.created_at > ?1
                   AND (?2 IS NULL OR m.session_id = ?2)
                   AND (?3 IS NULL OR json_extract(s.summary, '$.repo.origin') = ?3
                        OR json_extract(s.summary, '$.repo.root') = ?3)
                 ORDER BY m.created_at DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![since, query.session, query.repo, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?;
            for row in rows {
                let (station, session, text, at, title, repo) = row?;
                items.push(ActivityItem {
                    station_id: StationId::from(station),
                    session_id: SessionId::from(session),
                    session_title: title,
                    repo,
                    kind: ActivityKind::Mail,
                    request_kind: None,
                    text: one_line(&format!("Mail: {text}")),
                    at,
                    station_seq: None,
                });
            }
            items.sort_by(|a, b| b.at.cmp(&a.at).then(b.station_seq.cmp(&a.station_seq)));
            items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            Ok(ActivityList { items })
        })
        .await
    }
}
