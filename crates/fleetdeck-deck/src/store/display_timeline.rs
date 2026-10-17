//! The timeline of a wall display (R13 §5D items 7 and 10, R9 Q3 to Q5): the
//! one projection from an owner event to a [`DisplayEvent`]. Every send path
//! of the display WebSocket (replay, live, lag recovery) uses it. The result
//! is a new value built from a short list of fields; it is never an owner
//! event with fields removed.
//!
//! A tool completion, an approval answer and an approval expiry carry no
//! name or kind. The projection takes them from the same batch first and
//! then from the stored `tool.started` or `approval.requested` row. A
//! missing row gives the fixed name "Tool" for a tool and drops an approval
//! answer.

use std::collections::HashMap;

use fleetdeck_proto::display_event::{
    DisplayEvent, DisplayItem, DisplayRequestState, MAX_DISPLAY_PROMPT_LINE, MAX_DISPLAY_TOOL_NAME,
    RUN_STARTED_TEXT,
};
use fleetdeck_proto::event::{Event, RequestKind, RunOrigin};
use fleetdeck_proto::{EventEnvelope, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::store::{DeckStore, Result, call, to_i64};

/// The name of a tool whose `tool.started` is not known.
const UNKNOWN_TOOL: &str = "Tool";

/// What a projection of a batch of owner events gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayBatch {
    /// The kept events, in order.
    pub events: Vec<DisplayEvent>,
    /// The highest `station_seq` that the projection examined, also when it
    /// dropped every event. `None` when it examined no event.
    pub high_water: Option<u64>,
}

/// The tool names and the requests that the projection has seen.
#[derive(Default)]
struct Names {
    /// `node_id` to the safe tool name.
    tools: HashMap<String, String>,
    /// `request_id` to the kind and the correlated safe tool name.
    requests: HashMap<String, (RequestKind, Option<String>)>,
}

/// A tool name cut to [`MAX_DISPLAY_TOOL_NAME`] characters.
#[must_use]
pub fn safe_tool_name(title: &str) -> String {
    title.chars().take(MAX_DISPLAY_TOOL_NAME).collect()
}

/// The first non-empty trimmed line of `text`, cut to
/// [`MAX_DISPLAY_PROMPT_LINE`] characters.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(MAX_DISPLAY_PROMPT_LINE)
        .collect()
}

/// The text of a run start: the first line of the prompt for the origins
/// Board and Local, else the fixed text (R9 Q3).
fn run_summary(origin: RunOrigin, input_summary: &str) -> String {
    match origin {
        RunOrigin::Board | RunOrigin::Local => first_line(input_summary),
        _ => RUN_STARTED_TEXT.to_owned(),
    }
}

/// The safe name of the tool call `node_id`, from its stored `tool.started`
/// row.
pub(super) fn tool_name_db(
    conn: &Connection,
    station: &str,
    session: &str,
    node_id: &str,
) -> Result<Option<String>> {
    let text: Option<String> = conn
        .prepare_cached(
            "SELECT envelope FROM timeline
             WHERE station_id = ?1 AND session_id = ?2 AND kind = 'tool.started'
               AND json_extract(envelope, '$.event.node_id') = ?3
             ORDER BY station_seq DESC LIMIT 1",
        )?
        .query_row(params![station, session, node_id], |r| r.get(0))
        .optional()?;
    let Some(text) = text else {
        return Ok(None);
    };
    let env: EventEnvelope = serde_json::from_str(&text)?;
    Ok(match env.event.known() {
        Some(Event::ToolStarted { title, .. }) => Some(safe_tool_name(title)),
        _ => None,
    })
}

/// The kind and the correlated tool name of the request `request_id`, from
/// its stored `approval.requested` row.
fn request_db(
    conn: &Connection,
    station: &str,
    session: &str,
    request_id: &str,
) -> Result<Option<(RequestKind, Option<String>)>> {
    let text: Option<String> = conn
        .prepare_cached(
            "SELECT envelope FROM timeline
             WHERE station_id = ?1 AND session_id = ?2 AND kind = 'approval.requested'
               AND json_extract(envelope, '$.event.request_id') = ?3
             ORDER BY station_seq DESC LIMIT 1",
        )?
        .query_row(params![station, session, request_id], |r| r.get(0))
        .optional()?;
    let Some(text) = text else {
        return Ok(None);
    };
    let env: EventEnvelope = serde_json::from_str(&text)?;
    let Some(Event::ApprovalRequested {
        request_kind,
        node_id,
        ..
    }) = env.event.known()
    else {
        return Ok(None);
    };
    let tool = match node_id {
        Some(n) => tool_name_db(conn, station, session, n.as_str())?,
        None => None,
    };
    Ok(Some((*request_kind, tool)))
}

/// The scope of one projection: where the lookups read.
struct Ctx<'a> {
    conn: &'a Connection,
    station: &'a str,
    session: &'a str,
}

impl Ctx<'_> {
    fn tool(&self, names: &Names, node_id: &str) -> Result<Option<String>> {
        if let Some(n) = names.tools.get(node_id) {
            return Ok(Some(n.clone()));
        }
        tool_name_db(self.conn, self.station, self.session, node_id)
    }

    fn request(
        &self,
        names: &Names,
        request_id: &str,
    ) -> Result<Option<(RequestKind, Option<String>)>> {
        if let Some(r) = names.requests.get(request_id) {
            return Ok(Some(r.clone()));
        }
        request_db(self.conn, self.station, self.session, request_id)
    }
}

/// The display item of one owner event, or `None` for a dropped one.
fn item(ctx: &Ctx<'_>, names: &mut Names, event: &Event) -> Result<Option<DisplayItem>> {
    Ok(Some(match event {
        Event::RunStarted {
            run_id,
            input_summary,
            origin,
            ..
        } => DisplayItem::RunStarted {
            run_id: run_id.clone(),
            summary: run_summary(*origin, input_summary),
        },
        Event::RunCompleted { run_id, status, .. } => DisplayItem::RunEnded {
            run_id: run_id.clone(),
            status: *status,
        },
        Event::TurnStarted {
            run_id, turn_id, ..
        } => DisplayItem::TurnStarted {
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
        },
        Event::TurnCompleted {
            turn_id, status, ..
        } => DisplayItem::TurnEnded {
            turn_id: turn_id.clone(),
            status: *status,
        },
        Event::MessageCompleted { turn_id, text, .. } => DisplayItem::Message {
            turn_id: turn_id.clone(),
            text: text.clone(),
        },
        Event::ToolStarted { node_id, title, .. } => {
            let name = safe_tool_name(title);
            names
                .tools
                .insert(node_id.as_str().to_owned(), name.clone());
            DisplayItem::Tool {
                node_id: node_id.clone(),
                name,
                status: None,
            }
        }
        Event::ToolCompleted {
            node_id, status, ..
        } => DisplayItem::Tool {
            node_id: node_id.clone(),
            name: ctx
                .tool(names, node_id.as_str())?
                .unwrap_or_else(|| UNKNOWN_TOOL.to_owned()),
            status: Some(*status),
        },
        Event::ApprovalRequested {
            request_id,
            node_id,
            request_kind,
            ..
        } => {
            let tool_name = match node_id {
                Some(n) => ctx.tool(names, n.as_str())?,
                None => None,
            };
            names.requests.insert(
                request_id.as_str().to_owned(),
                (*request_kind, tool_name.clone()),
            );
            DisplayItem::Request {
                request_id: request_id.clone(),
                kind: *request_kind,
                tool_name,
                state: DisplayRequestState::Opened,
            }
        }
        Event::ApprovalResolved { request_id, .. } => {
            return answered(ctx, names, request_id, DisplayRequestState::Resolved);
        }
        Event::ApprovalExpired { request_id, .. } => {
            return answered(ctx, names, request_id, DisplayRequestState::Expired);
        }
        _ => return Ok(None),
    }))
}

/// The item of a resolved or expired request; dropped when the opened
/// request is not known.
fn answered(
    ctx: &Ctx<'_>,
    names: &Names,
    request_id: &fleetdeck_proto::ids::RequestId,
    state: DisplayRequestState,
) -> Result<Option<DisplayItem>> {
    Ok(ctx
        .request(names, request_id.as_str())?
        .map(|(kind, tool_name)| DisplayItem::Request {
            request_id: request_id.clone(),
            kind,
            tool_name,
            state,
        }))
}

/// The projection of a batch of one session. The caller holds the database
/// connection, so the lookups read one consistent state.
fn project_batch(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
    envelopes: &[EventEnvelope],
) -> Result<DisplayBatch> {
    let ctx = Ctx {
        conn,
        station: station.as_str(),
        session: session.as_str(),
    };
    let mut names = Names::default();
    let mut events = Vec::new();
    let mut high_water: Option<u64> = None;
    for e in envelopes {
        high_water = Some(high_water.map_or(e.station_seq, |h| h.max(e.station_seq)));
        let Some(known) = e.event.known() else {
            continue;
        };
        if let Some(item) = item(&ctx, &mut names, known)? {
            events.push(DisplayEvent {
                station_seq: e.station_seq,
                session_seq: e.session_seq,
                at: e.at,
                item,
            });
        }
    }
    Ok(DisplayBatch { events, high_water })
}

impl DeckStore {
    /// The projection of live events of one session for a display. The
    /// events are in `station_seq` order.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_project(
        &self,
        station: StationId,
        session: SessionId,
        events: Vec<EventEnvelope>,
    ) -> Result<DisplayBatch> {
        call(&self.inner.db, move |conn| {
            project_batch(conn, &station, &session, &events)
        })
        .await
    }

    /// The stored timeline of a session after `after`, projected for a
    /// display.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_timeline_after(
        &self,
        station: StationId,
        session: SessionId,
        after: u64,
    ) -> Result<DisplayBatch> {
        let after = to_i64(after)?;
        call(&self.inner.db, move |conn| {
            let rows: Vec<String> = conn
                .prepare(
                    "SELECT envelope FROM timeline
                     WHERE station_id = ?1 AND session_id = ?2 AND station_seq > ?3
                     ORDER BY station_seq",
                )?
                .query_map(params![station.as_str(), session.as_str(), after], |r| {
                    r.get(0)
                })?
                .collect::<rusqlite::Result<_>>()?;
            let mut envelopes = Vec::with_capacity(rows.len());
            for text in rows {
                envelopes.push(serde_json::from_str::<EventEnvelope>(&text)?);
            }
            project_batch(conn, &station, &session, &envelopes)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_name_is_cut_at_40_characters() {
        assert_eq!(safe_tool_name("Shell"), "Shell");
        assert_eq!(safe_tool_name(&"é".repeat(60)).chars().count(), 40);
    }

    #[test]
    fn a_prompt_line_is_the_first_non_empty_line_cut_at_120() {
        assert_eq!(first_line("\n  hello  \nsecond"), "hello");
        assert_eq!(first_line(&"x".repeat(300)).chars().count(), 120);
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn only_a_board_or_local_run_shows_its_prompt() {
        for origin in [RunOrigin::Board, RunOrigin::Local] {
            assert_eq!(run_summary(origin, "fix it"), "fix it");
        }
        for origin in [
            RunOrigin::Mail,
            RunOrigin::Coordinator,
            RunOrigin::Shell,
            RunOrigin::Resume,
            RunOrigin::Provider,
            RunOrigin::Continuation,
        ] {
            assert_eq!(run_summary(origin, "SECRET"), RUN_STARTED_TEXT);
        }
    }
}
