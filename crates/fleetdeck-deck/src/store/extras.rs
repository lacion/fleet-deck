//! The card extras of the board (DL-141): the mails that wait for delivery
//! and the files that a session touched lately. Both come from tables that
//! exist already (`mail` with its outbox item, `file_touches`).

use std::collections::BTreeMap;

use fleetdeck_proto::board::{FleetExtras, MAX_EXTRAS_FILES, SessionExtras};
use fleetdeck_proto::conflicts::CONFLICT_WINDOW_MS;
use fleetdeck_proto::{SessionId, StationId};

use super::{DeckStore, Result, call};

impl DeckStore {
    /// The extras of every session of the read model that has a pending
    /// mail or a recent touch, by station id and session id. A mail is
    /// pending while its outbox item is `queued` or `sending`; the files
    /// are the newest distinct touches inside [`CONFLICT_WINDOW_MS`].
    ///
    /// # Errors
    /// A database error.
    pub async fn fleet_extras(&self) -> Result<FleetExtras> {
        let since = self.now_ms().saturating_sub(CONFLICT_WINDOW_MS);
        call(&self.inner.db, move |conn| {
            let mut extras: BTreeMap<(String, String), (u32, Vec<String>)> = BTreeMap::new();

            let mut stmt = conn.prepare(
                "SELECT m.station_id, m.session_id, COUNT(*) FROM mail m
                 JOIN outbox o ON o.outbox_id = m.outbox_id
                 JOIN sessions s ON s.station_id = m.station_id AND s.session_id = m.session_id
                 WHERE o.state IN ('queued', 'sending')
                 GROUP BY m.station_id, m.session_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (station, session, n) = row?;
                extras.entry((station, session)).or_default().0 =
                    u32::try_from(n).unwrap_or(u32::MAX);
            }

            let mut stmt = conn.prepare(
                "SELECT t.station_id, t.session_id, t.file FROM file_touches t
                 JOIN sessions s ON s.station_id = t.station_id AND s.session_id = t.session_id
                 WHERE t.touched_at >= ?1
                 ORDER BY t.touched_at DESC, t.file",
            )?;
            let rows = stmt.query_map([since], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (station, session, file) = row?;
                let files = &mut extras.entry((station, session)).or_default().1;
                if files.len() < MAX_EXTRAS_FILES && !files.contains(&file) {
                    files.push(file);
                }
            }

            Ok(FleetExtras {
                sessions: extras
                    .into_iter()
                    .filter(|(_, (mail, files))| *mail > 0 || !files.is_empty())
                    .map(
                        |((station, session), (mail_pending, files))| SessionExtras {
                            station_id: StationId::from(station),
                            session_id: SessionId::from(session),
                            mail_pending,
                            files,
                        },
                    )
                    .collect(),
            })
        })
        .await
    }
}
