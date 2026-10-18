//! The station sender of a directive (R13 P4 task E, design `omarchy-p4.md`
//! §3.6): the hotkeys of the Omarchy desktop shell ask the deck for the next
//! session that needs the owner or for the next mode of a wall.
//!
//! One transaction checks that the display exists, is not revoked and is
//! bound to the calling station, builds the body from the stored directive
//! and the open requests of the calling station's own sessions, and writes it
//! through the shared validated path
//! ([`super::display_directives::write_directive`]). The station never sends
//! a body: it names an intent only.

use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveView, SessionRef};
use fleetdeck_proto::{SessionSummary, StationId};
use rusqlite::{Connection, params};

use super::display_directives::{DirectiveError, DirectiveSender, live, write_directive};
use super::displays::load;
use super::{DeckError, DeckStore, Result, call};
use crate::link::display_directive::DisplayIntent;

/// Why a station request is refused.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StationDirectiveError {
    /// The display is unknown, revoked or not bound to the caller. One
    /// answer for all three: a station cannot probe other displays.
    #[error("the display is not bound to this station")]
    NotBound,
    /// `next_needs_you` with no open request of the caller's sessions.
    #[error("no session of this station needs the owner")]
    NothingToShow,
    /// The shared validated write refused the body.
    #[error(transparent)]
    Directive(DirectiveError),
    #[error(transparent)]
    Store(#[from] DeckError),
}

/// One open request of a session of the calling station.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Open {
    requested_at: i64,
    session_id: String,
    request_id: String,
}

/// The open requests of the sessions of `station`, oldest first (ties by
/// session id, then request id, so the order is stable).
fn open_requests(conn: &Connection, station: &StationId) -> Result<Vec<Open>> {
    let mut stmt = conn.prepare("SELECT summary FROM sessions WHERE station_id = ?1")?;
    let texts = stmt
        .query_map(params![station.as_str()], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut found = Vec::new();
    for text in texts {
        let summary: SessionSummary = serde_json::from_str(&text)?;
        for request in &summary.open_requests {
            found.push(Open {
                requested_at: request.requested_at,
                session_id: summary.session_id.to_string(),
                request_id: request.request_id.to_string(),
            });
        }
    }
    found.sort();
    Ok(found)
}

fn focus(station: &StationId, open: &Open) -> DirectiveBody {
    DirectiveBody::Focus {
        session: SessionRef {
            station_id: station.clone(),
            session_id: open.session_id.as_str().into(),
        },
        pin: false,
        filter: None,
    }
}

/// The request after the one that the current directive shows, wrapping
/// around. The current position is the last request of the focused session;
/// no focus on a session of the caller with an open request is "no current
/// focus": the oldest request. `None` without any request.
fn next_request<'a>(
    open: &'a [Open],
    current: &DirectiveBody,
    station: &StationId,
) -> Option<&'a Open> {
    if open.is_empty() {
        return None;
    }
    let position = match current {
        DirectiveBody::Focus { session, .. } if &session.station_id == station => open
            .iter()
            .rposition(|o| o.session_id == session.session_id.as_str()),
        _ => None,
    };
    open.get(position.map_or(0, |i| (i + 1) % open.len()))
}

fn plain(mission: bool) -> DirectiveBody {
    if mission {
        DirectiveBody::mission()
    } else {
        DirectiveBody::Ambient {
            pin: false,
            filter: None,
        }
    }
}

impl DeckStore {
    /// Builds the directive that `intent` asks for, from the stored directive
    /// of `display_id` and the open requests of the sessions of `station`
    /// inside the ceiling, and stores it with the sender
    /// `DirectiveSender::Station(station)`.
    ///
    /// # Errors
    /// [`StationDirectiveError`]: `NotBound` (unknown, revoked or another
    /// station), `NothingToShow`, a refusal of the shared write, or a
    /// database error.
    pub(crate) async fn display_directive_station(
        &self,
        station: StationId,
        display_id: String,
        intent: DisplayIntent,
    ) -> std::result::Result<DirectiveView, StationDirectiveError> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(display) = load(&tx, &display_id)? else {
                return Ok(Err(StationDirectiveError::NotBound));
            };
            if display.binding.as_ref().map(|b| &b.station_id) != Some(&station) {
                return Ok(Err(StationDirectiveError::NotBound));
            }
            let Some((_, current, _)) = live(&tx, &display_id)? else {
                return Ok(Err(StationDirectiveError::NotBound));
            };
            // Only the sessions of the calling station, and only when the
            // station is inside the ceiling of the display.
            let open = if display.ceiling.contains(&station) {
                open_requests(&tx, &station)?
            } else {
                Vec::new()
            };
            let body = match intent {
                DisplayIntent::NextNeedsYou => match next_request(&open, &current, &station) {
                    Some(target) => focus(&station, target),
                    None => return Ok(Err(StationDirectiveError::NothingToShow)),
                },
                DisplayIntent::NextMode => match current {
                    DirectiveBody::Mission { .. } => match open.first() {
                        Some(oldest) => focus(&station, oldest),
                        None => plain(false),
                    },
                    DirectiveBody::Focus { .. } => plain(false),
                    // `ambient` and every mode outside the cycle.
                    _ => plain(true),
                },
            };
            let sender = DirectiveSender::Station(station);
            match write_directive(&tx, &display, &sender, body, now)? {
                Ok((view, _)) => {
                    tx.commit()?;
                    Ok(Ok(view))
                }
                Err(e) => Ok(Err(StationDirectiveError::Directive(e))),
            }
        })
        .await?
    }
}

#[cfg(test)]
#[path = "display_directives_station_tests.rs"]
mod tests;
