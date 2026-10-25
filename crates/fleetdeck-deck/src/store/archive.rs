//! The archived (cleared) sessions of the board (DL-149): the deck keeps
//! the set, so every device shows the same board. A change carries a
//! client-chosen `mutation_id`; a resend with a known id changes nothing,
//! so a stale retry cannot undo a later change.

use fleetdeck_proto::StationId;
use fleetdeck_proto::board::{ArchiveChange, ArchiveList, ArchiveRef};
use rusqlite::{Connection, params};

use super::{DeckStore, Result, call};

fn list(conn: &Connection) -> Result<ArchiveList> {
    let mut stmt = conn.prepare(
        "SELECT station_id, session_id FROM archived_sessions
         WHERE archived = 1 ORDER BY station_id, session_id",
    )?;
    let archived = stmt
        .query_map([], |r| {
            Ok(ArchiveRef {
                station_id: StationId::from(r.get::<_, String>(0)?),
                session_id: r.get::<_, String>(1)?.into(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ArchiveList { archived })
}

impl DeckStore {
    /// The archived sessions, ordered by station and session.
    ///
    /// # Errors
    /// A database error.
    pub async fn archive_list(&self) -> Result<ArchiveList> {
        call(&self.inner.db, |conn| list(conn)).await
    }

    /// Applies the changes of one mutation in one transaction and returns
    /// the current list and whether the mutation was applied. A known
    /// `mutation_id` changes nothing and returns `false`.
    ///
    /// # Errors
    /// A database error.
    pub async fn apply_archive(&self, change: ArchiveChange) -> Result<(ArchiveList, bool)> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let applied = tx.execute(
                "INSERT OR IGNORE INTO archive_mutations (mutation_id, applied_at)
                 VALUES (?1, ?2)",
                params![change.mutation_id, now],
            )? == 1;
            if applied {
                let mut upsert = tx.prepare(
                    "INSERT INTO archived_sessions (station_id, session_id, archived, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (station_id, session_id)
                     DO UPDATE SET archived = excluded.archived, updated_at = excluded.updated_at",
                )?;
                for item in &change.changes {
                    upsert.execute(params![
                        item.station_id.as_str(),
                        item.session_id.as_str(),
                        i64::from(item.archived),
                        now,
                    ])?;
                }
            }
            let current = list(&tx)?;
            tx.commit()?;
            Ok((current, applied))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use fleetdeck_proto::board::{ArchiveChange, ArchiveItem};
    use fleetdeck_testkit::Det;

    use crate::store::DeckStore;

    async fn store() -> DeckStore {
        let det = Det::new();
        DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap()
    }

    fn change(id: &str, items: &[(&str, &str, bool)]) -> ArchiveChange {
        ArchiveChange {
            mutation_id: id.to_owned(),
            changes: items
                .iter()
                .map(|(st, se, archived)| ArchiveItem {
                    station_id: (*st).into(),
                    session_id: (*se).into(),
                    archived: *archived,
                })
                .collect(),
        }
    }

    fn refs(l: &fleetdeck_proto::board::ArchiveList) -> Vec<String> {
        l.archived
            .iter()
            .map(|r| format!("{}/{}", r.station_id.as_str(), r.session_id.as_str()))
            .collect()
    }

    #[tokio::test]
    async fn a_new_id_applies_and_the_list_is_ordered() {
        let s = store().await;
        assert!(s.archive_list().await.unwrap().archived.is_empty());
        let (l, applied) = s
            .apply_archive(change(
                "mutation-1",
                &[("b", "s2", true), ("a", "s9", true), ("a", "s1", true)],
            ))
            .await
            .unwrap();
        assert!(applied);
        assert_eq!(refs(&l), ["a/s1", "a/s9", "b/s2"]);
        assert_eq!(s.archive_list().await.unwrap(), l);
    }

    #[tokio::test]
    async fn the_same_id_again_changes_nothing() {
        let s = store().await;
        let (first, applied) = s
            .apply_archive(change("mutation-1", &[("a", "s1", true)]))
            .await
            .unwrap();
        assert!(applied);
        // The same id with other items is still a replay.
        let (again, applied) = s
            .apply_archive(change("mutation-1", &[("a", "s2", true)]))
            .await
            .unwrap();
        assert!(!applied);
        assert_eq!(again, first);
    }

    #[tokio::test]
    async fn an_archive_then_an_unarchive_with_two_ids() {
        let s = store().await;
        s.apply_archive(change(
            "mutation-1",
            &[("a", "s1", true), ("a", "s2", true)],
        ))
        .await
        .unwrap();
        let (l, applied) = s
            .apply_archive(change("mutation-2", &[("a", "s1", false)]))
            .await
            .unwrap();
        assert!(applied);
        assert_eq!(refs(&l), ["a/s2"]);
    }

    #[tokio::test]
    async fn a_stale_retry_does_not_undo_a_later_change() {
        let s = store().await;
        // A archives with id 1, B unarchives with id 2, A retries id 1.
        s.apply_archive(change("mutation-1", &[("a", "s1", true)]))
            .await
            .unwrap();
        s.apply_archive(change("mutation-2", &[("a", "s1", false)]))
            .await
            .unwrap();
        let (l, applied) = s
            .apply_archive(change("mutation-1", &[("a", "s1", true)]))
            .await
            .unwrap();
        assert!(!applied);
        assert!(l.archived.is_empty(), "the session stays unarchived");
    }
}
