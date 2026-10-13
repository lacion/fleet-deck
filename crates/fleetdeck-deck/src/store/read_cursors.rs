//! Unread state per session (R6.1, DL-169). A read cursor is the highest
//! `session_seq` of a session that a caller has seen. The unread count of a
//! session is the number of timeline rows after the cursor that are agent
//! output or need the caller: a completed message, an approval request or a
//! proposed plan. The caller's own inputs are never counted. A caller
//! without a cursor row has every such row of the capped timeline unread.

use std::collections::BTreeMap;

use fleetdeck_proto::{SessionId, StationId};
use rusqlite::params;

use super::{DeckStore, Principal, Result, call, to_i64};

/// The timeline kinds that count as unread.
const UNREAD_KINDS: &str = "('message.completed', 'approval.requested', 'plan.proposed')";

/// The cursor key of a caller: `owner`, or the audit actor name of another
/// principal (`token:<name>`; sharing adds `member:<id>`).
#[must_use]
pub fn principal_key(principal: &Principal) -> String {
    principal.actor()
}

impl DeckStore {
    /// The unread count of each session of the read model that has one, for
    /// the caller `principal` (a [`principal_key`]), by station and session.
    ///
    /// # Errors
    /// A database error.
    pub async fn unread_counts(
        &self,
        principal: String,
    ) -> Result<BTreeMap<(StationId, SessionId), u32>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT t.station_id, t.session_id, COUNT(*) FROM timeline t
                 LEFT JOIN read_cursors c ON c.principal = ?1
                     AND c.station_id = t.station_id AND c.session_id = t.session_id
                 WHERE t.kind IN {UNREAD_KINDS} AND t.session_seq > COALESCE(c.seq, 0)
                 GROUP BY t.station_id, t.session_id"
            ))?;
            let rows = stmt.query_map(params![principal], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut out = BTreeMap::new();
            for row in rows {
                let (station, session, n) = row?;
                out.insert(
                    (StationId::from(station), SessionId::from(session)),
                    u32::try_from(n).unwrap_or(u32::MAX),
                );
            }
            Ok(out)
        })
        .await
    }

    /// Moves the read cursor of `principal` for a session up to `seq`; a
    /// lower `seq` changes nothing. Returns `false` when the read model has
    /// no such session.
    ///
    /// # Errors
    /// A database error, or a `seq` above the integer range.
    pub async fn mark_read(
        &self,
        principal: String,
        station: StationId,
        session: SessionId,
        seq: u64,
    ) -> Result<bool> {
        let now = self.now_ms();
        let seq = to_i64(seq)?;
        let known = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let known: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM sessions WHERE station_id = ?1 AND session_id = ?2)",
                params![station.as_str(), session.as_str()],
                |r| r.get(0),
            )?;
            if known {
                tx.execute(
                    "INSERT INTO read_cursors (principal, station_id, session_id, seq, at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (principal, station_id, session_id) DO UPDATE SET
                         seq = MAX(seq, excluded.seq), at = excluded.at",
                    params![principal, station.as_str(), session.as_str(), seq, now],
                )?;
            }
            tx.commit()?;
            Ok(known)
        })
        .await?;
        if known {
            self.board_changed();
        }
        Ok(known)
    }

    /// The cards of the board with the unread counts of `principal`.
    ///
    /// # Errors
    /// A database error.
    pub async fn sessions_for(&self, principal: &Principal) -> Result<Vec<super::DeckSession>> {
        let mut cards = self.sessions().await?;
        let counts = self.unread_counts(principal_key(principal)).await?;
        for card in &mut cards {
            let key = (card.station_id.clone(), card.summary.session_id.clone());
            card.unread = counts.get(&key).copied().filter(|n| *n > 0);
        }
        self.fill_projects(principal, &mut cards).await?;
        Ok(cards)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeSet;

    use fleetdeck_core::identity::Identity;
    use fleetdeck_proto::event::{Event, EventBody, RunOrigin};
    use fleetdeck_proto::link::Scope;
    use fleetdeck_proto::{EventEnvelope, IncarnationId};
    use fleetdeck_testkit::Det;

    use super::*;
    use crate::store::stations::NewStation;

    async fn setup() -> (DeckStore, StationId) {
        let det = Det::new();
        let s = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        let rec = s
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "alpha".to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap();
        s.apply_snapshot(
            rec.station_id.clone(),
            IncarnationId::from("i1"),
            0,
            Vec::new(),
        )
        .await
        .unwrap();
        (s, rec.station_id)
    }

    fn env(seq: u64, session: &str, event: Event) -> EventEnvelope {
        EventEnvelope {
            station_seq: seq,
            session_id: session.into(),
            session_seq: seq,
            at: 1_000,
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Known(event),
        }
    }

    fn message(seq: u64, session: &str) -> EventEnvelope {
        env(
            seq,
            session,
            Event::MessageCompleted {
                turn_id: "t".into(),
                parent_node_id: None,
                text: "done".to_owned(),
            },
        )
    }

    fn input(seq: u64, session: &str) -> EventEnvelope {
        env(
            seq,
            session,
            Event::RunStarted {
                run_id: format!("r{seq}").into(),
                input_summary: "hi".to_owned(),
                origin: RunOrigin::Board,
                author: None,
            },
        )
    }

    #[tokio::test]
    async fn unread_counts_agent_output_after_the_cursor_and_not_own_inputs() {
        let (s, st) = setup().await;
        s.apply_events(
            st.clone(),
            IncarnationId::from("i1"),
            vec![
                input(1, "s1"),
                message(2, "s1"),
                message(3, "s1"),
                input(4, "s1"),
            ],
        )
        .await
        .unwrap();
        let key = |session: &str| (st.clone(), SessionId::from(session));
        // No cursor: both messages are unread.
        let owner = s.unread_counts("owner".to_owned()).await.unwrap();
        assert_eq!(owner.get(&key("s1")), Some(&2));
        assert!(
            s.mark_read("owner".into(), st.clone(), "s1".into(), 2)
                .await
                .unwrap()
        );
        let owner = s.unread_counts("owner".to_owned()).await.unwrap();
        assert_eq!(owner.get(&key("s1")), Some(&1));
        // A lower seq never moves the cursor back.
        s.mark_read("owner".into(), st.clone(), "s1".into(), 1)
            .await
            .unwrap();
        assert_eq!(
            s.unread_counts("owner".to_owned())
                .await
                .unwrap()
                .get(&key("s1")),
            Some(&1)
        );
        // Another caller keeps its own cursor.
        let other = s.unread_counts("token:phone".to_owned()).await.unwrap();
        assert_eq!(other.get(&key("s1")), Some(&2));
        s.mark_read("owner".into(), st.clone(), "s1".into(), 4)
            .await
            .unwrap();
        assert!(
            s.unread_counts("owner".to_owned())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn marking_an_unknown_session_changes_nothing() {
        let (s, st) = setup().await;
        assert!(
            !s.mark_read("owner".into(), st, "nope".into(), 3)
                .await
                .unwrap()
        );
    }
}
