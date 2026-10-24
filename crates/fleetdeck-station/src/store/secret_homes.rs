//! The home of a project, as the secret file methods read it (Fleet Sync
//! FS-3). `station_homes` holds every home of the newest snapshot.

use rusqlite::OptionalExtension;

use super::{Result, Store, call};

impl Store {
    /// The generation and the path of the home of `project_id` on this
    /// station, or `None` when the station has no home of the project.
    pub(crate) async fn station_home(&self, project_id: String) -> Result<Option<(u64, String)>> {
        call(&self.inner.db, move |conn| {
            let row: Option<(i64, String)> = conn
                .query_row(
                    "SELECT generation, path FROM station_homes WHERE project_id = ?1",
                    [project_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            Ok(row.map(|(generation, path)| (u64::try_from(generation).unwrap_or(0), path)))
        })
        .await
    }
}
