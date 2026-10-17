//! The directive store of a display (R13 §5D items 3 to 5).
//!
//! STEP 1 STUB. The shared interface is final; the P2b directives task
//! replaces the bodies:
//! - [`DeckStore::display_directive`] reads the stored row, validates it
//!   again (the sender authority and every reference against the current
//!   ceiling) and returns it, or `mission`;
//! - [`DeckStore::display_scope`] returns the ceiling narrowed by the
//!   directive filter (stations and repository match).
//!
//! The table `display_directives` and the invalidation helper
//! ([`super::display_directive_schema::invalidate_directive`]) exist now.

use std::collections::BTreeSet;

use fleetdeck_proto::StationId;
use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveView};
use rusqlite::{OptionalExtension, params};

use super::displays::load;
use super::{DeckStore, Result, call};

/// What a display may read now: the stations of its ceiling, narrowed by the
/// filter of its directive (R13 §5D item 17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveScope {
    pub stations: BTreeSet<StationId>,
    /// A repository basename; `None`: no repository filter.
    pub repo: Option<String>,
}

impl DeckStore {
    /// The directive that the display reads now and its generation. A display
    /// without a valid stored directive reads `mission`. `None` when the
    /// display does not exist or is revoked.
    ///
    /// Step 1 stub: always `mission` with the stored generation.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_directive(&self, display_id: String) -> Result<Option<DirectiveView>> {
        call(&self.inner.db, move |conn| {
            let generation: Option<i64> = conn
                .query_row(
                    "SELECT directive_generation FROM displays
                     WHERE display_id = ?1 AND revoked_at IS NULL",
                    params![display_id],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(generation.map(|g| DirectiveView {
                generation: u64::try_from(g).unwrap_or(0),
                directive: DirectiveBody::mission(),
            }))
        })
        .await
    }

    /// The effective scope of a display. `None` when the display does not
    /// exist or is revoked.
    ///
    /// Step 1 stub: the ceiling, with no repository filter.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_scope(&self, display_id: String) -> Result<Option<EffectiveScope>> {
        call(&self.inner.db, move |conn| {
            Ok(load(conn, &display_id)?.map(|d| EffectiveScope {
                stations: d.ceiling.into_iter().collect(),
                repo: None,
            }))
        })
        .await
    }
}
