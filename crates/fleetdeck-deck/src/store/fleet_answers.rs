//! What a fleet-control coordinator answered (ADR-008 §5.3 "Owner
//! visibility"): the last 50 `coordinator.answer` audit rows of the
//! coordinator session, the count of the successful answers of the last
//! hour, and the answer cap and use of its grant. The owner reads it; no
//! audit row is written for the read.

use fleetdeck_proto::{SessionId, StationId};
use rusqlite::params;
use serde::Serialize;

use super::{DeckStore, Result, call, caller_grant};

/// The most answers that the list holds.
const LIST_LIMIT: i64 = 50;
/// One hour, in ms.
const HOUR_MS: i64 = 3_600_000;

/// One `coordinator.answer` audit row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetAnswer {
    /// Unix ms.
    pub at: i64,
    /// `<station>/<session>` of the answered session, when known.
    pub target: Option<String>,
    /// `ok`, `refused` or `failed`.
    pub result: String,
    pub detail: Option<String>,
    pub command_id: Option<String>,
}

/// The answers of one coordinator session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetAnswers {
    /// The newest grant of the session is a fleet-control grant.
    pub fleet_control: bool,
    pub answer_cap: Option<u32>,
    pub answer_used: u32,
    /// The successful answers of the last hour.
    pub last_hour: u32,
    /// The last 50 answers, newest first.
    pub answers: Vec<FleetAnswer>,
}

impl DeckStore {
    /// The answers of the coordinator `session` of `station`. The rows are
    /// returned also when the newest grant is not a fleet-control grant: an
    /// older grant may have answered.
    ///
    /// # Errors
    /// A database error.
    pub async fn fleet_answers(
        &self,
        station: StationId,
        session: SessionId,
    ) -> Result<FleetAnswers> {
        let since = self.now_ms().saturating_sub(HOUR_MS);
        call(&self.inner.db, move |conn| {
            let (fleet_control, answer_cap, answer_used) =
                match caller_grant(conn, &station, &session)? {
                    Some(g) if g.is_fleet_control() => (true, g.answer_cap, g.answer_used),
                    _ => (false, None, 0),
                };
            let actor = format!("coordinator:{}/{}", station.as_str(), session.0);
            let mut stmt = conn.prepare(
                "SELECT at, target, result, detail, command_id FROM audit
                 WHERE actor = ?1 AND action = 'coordinator.answer'
                 ORDER BY audit_id DESC LIMIT ?2",
            )?;
            let answers = stmt
                .query_map(params![actor, LIST_LIMIT], |r| {
                    Ok(FleetAnswer {
                        at: r.get(0)?,
                        target: r.get(1)?,
                        result: r.get(2)?,
                        detail: r.get(3)?,
                        command_id: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let last_hour: i64 = conn.query_row(
                "SELECT COUNT(*) FROM audit
                 WHERE actor = ?1 AND action = 'coordinator.answer'
                   AND at >= ?2 AND result = 'ok'",
                params![actor, since],
                |r| r.get(0),
            )?;
            Ok(FleetAnswers {
                fleet_control,
                answer_cap,
                answer_used,
                last_hour: u32::try_from(last_hour).unwrap_or(u32::MAX),
                answers,
            })
        })
        .await
    }
}
