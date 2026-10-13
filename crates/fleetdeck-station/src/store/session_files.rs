//! The files that a session changed, from its own `file.changed` events
//! (the shared-folder session diff, DL-159).

use fleetdeck_proto::event::EventBody;
use fleetdeck_proto::{Event, SessionId};
use rusqlite::params;

use super::{Result, Store, call};

impl Store {
    /// The paths of the `file.changed` events of `session_id`, oldest
    /// first; a path can repeat. A path is as the provider reported it: absolute
    /// or relative to the session folder.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn session_changed_paths(&self, session_id: SessionId) -> Result<Vec<String>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT payload FROM events WHERE session_id = ?1 AND kind = 'file.changed'
                 ORDER BY session_seq",
            )?;
            let rows = stmt.query_map(params![session_id.as_str()], |r| r.get::<_, String>(0))?;
            let mut paths: Vec<String> = Vec::new();
            for row in rows {
                if let EventBody::Known(Event::FileChanged { path, .. }) =
                    serde_json::from_str::<EventBody>(&row?)?
                {
                    paths.push(path);
                }
            }
            Ok(paths)
        })
        .await
    }
}
