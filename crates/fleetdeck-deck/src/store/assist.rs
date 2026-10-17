//! Reads for the model-help routes (R10, DL-167): one session's summary and
//! one library plan.

use fleetdeck_proto::plans::DeckPlan;
use fleetdeck_proto::{SessionId, SessionSummary, StationId};
use rusqlite::{OptionalExtension, params};

use super::{DeckStore, Result, call};

impl DeckStore {
    /// The stored summary of one session.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn session_summary(
        &self,
        station: StationId,
        session: SessionId,
    ) -> Result<Option<SessionSummary>> {
        call(&self.inner.db, move |conn| {
            let text: Option<String> = conn
                .query_row(
                    "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
                    params![station.as_str(), session.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
        })
        .await
    }

    /// One plan of the library by its id.
    ///
    /// # Errors
    /// A database error.
    pub async fn plan(&self, id: String) -> Result<Option<DeckPlan>> {
        Ok(self.plans(true).await?.into_iter().find(|p| p.id == id))
    }
}
