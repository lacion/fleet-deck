//! The reserved Git names (design §3.2, §8.2; FG-5, phase 1). The first sync
//! start of a station for a repository reserves `(repo, slug)` for that
//! station id. A reservation is never released in v1: a rename, a removal of
//! a project or of a station does not free it, so an existing branch
//! namespace on the remote never passes to another station.

use rusqlite::{Connection, OptionalExtension, params};

use super::Result;

/// Reserves `(repo, slug)` for `station`. `Ok(None)`: the station owns the
/// name now (a new or an earlier reservation). `Ok(Some(other))`: the name is
/// reserved for the station id `other`; nothing was written.
pub(super) fn reserve(
    conn: &Connection,
    repo: &str,
    slug: &str,
    station: &str,
    now: i64,
) -> Result<Option<String>> {
    let holder: Option<String> = conn
        .query_row(
            "SELECT station_id FROM slug_reservations WHERE repo = ?1 AND slug = ?2",
            params![repo, slug],
            |r| r.get(0),
        )
        .optional()?;
    match holder {
        Some(holder) if holder == station => Ok(None),
        Some(holder) => Ok(Some(holder)),
        None => {
            conn.execute(
                "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![repo, slug, station, now],
            )?;
            Ok(None)
        }
    }
}

/// The name of the station `id`, else the id itself (a removed station).
pub(super) fn station_name(conn: &Connection, id: &str) -> Result<String> {
    let name: Option<String> = conn
        .query_row(
            "SELECT name FROM stations WHERE station_id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(name.unwrap_or_else(|| id.to_owned()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::store::schema::migrations;

    fn conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut conn).unwrap();
        conn
    }

    #[test]
    fn the_first_station_owns_a_name_and_another_station_is_refused() {
        let conn = conn();
        assert_eq!(reserve(&conn, "h/a", "alpha", "st-1", 1).unwrap(), None);
        assert_eq!(reserve(&conn, "h/a", "alpha", "st-1", 2).unwrap(), None);
        assert_eq!(
            reserve(&conn, "h/a", "alpha", "st-2", 3).unwrap(),
            Some("st-1".to_owned())
        );
        // Another repository has its own namespace.
        assert_eq!(reserve(&conn, "h/b", "alpha", "st-2", 4).unwrap(), None);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM slug_reservations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "a refusal writes nothing");
    }

    #[test]
    fn a_removed_station_is_named_by_its_id() {
        let conn = conn();
        assert_eq!(station_name(&conn, "gone").unwrap(), "gone");
    }
}
