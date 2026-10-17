//! The binding of a display to a station and a monitor (R13 §5.4, §6D items
//! 1 and 2). The binding gives station authority and the wake automation, so
//! every change is checked and runs in one transaction.
//!
//! - `display_begin_open` starts a `wall.open` launch: it refuses a display
//!   that is bound to another station and moves `binding_revision` up. The
//!   launch carries the new revision; the pairing of the kiosk stores it.
//! - `display_commit_binding` commits the binding after a successful open,
//!   only when the revision is still the one of the launch.
//! - `display_clear_binding` clears it after a successful explicit close.
//! - `display_clear_binding_at` clears it only when it is still the binding
//!   (station and revision) that the close saw at its start.
//! - `display_set_binding` is the owner's edit in the settings.
//!
//! A change of the binding deletes the stored directive in the same
//! transaction (R13 §6D item 12).

use fleetdeck_proto::StationId;
use fleetdeck_proto::display::DisplayBinding;
use rusqlite::{Connection, OptionalExtension, params};

use super::display_directive_schema::invalidate_directive;
use super::displays::{DisplayRecord, load};
use super::{DeckError, DeckStore, Result, call};

/// Why a binding change is refused.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// The display does not exist or is revoked.
    #[error("no such display")]
    Unknown,
    /// The display is bound to another station: `409 display_bound`.
    #[error("the display is bound to another station")]
    Bound,
    /// The operation revision moved on: `409 superseded`.
    #[error("the operation was superseded")]
    Superseded,
    /// A bad monitor name or an unpaired station.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] DeckError),
}

impl From<rusqlite::Error> for BindError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}

/// The start of a `wall.open` launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenTicket {
    /// The new operation revision of the display.
    pub op_revision: i64,
}

/// A monitor name: `^[A-Za-z0-9._-]{1,64}$` (R13 §5A item 8).
///
/// # Errors
/// [`BindError::Invalid`].
pub fn check_monitor(monitor: &str) -> std::result::Result<(), BindError> {
    let ok = !monitor.is_empty()
        && monitor.len() <= 64
        && monitor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(BindError::Invalid(
            "a monitor name is 1-64 letters, digits, dots, dashes or underscores".to_owned(),
        ))
    }
}

fn check_station(conn: &Connection, station: &StationId) -> std::result::Result<(), BindError> {
    let known = conn
        .query_row(
            "SELECT 1 FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if known {
        Ok(())
    } else {
        Err(BindError::Invalid(
            "the binding names a station that is not paired".to_owned(),
        ))
    }
}

fn bump(conn: &Connection, display_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "UPDATE displays SET binding_revision = binding_revision + 1
         WHERE display_id = ?1 RETURNING binding_revision",
        params![display_id],
        |r| r.get(0),
    )?)
}

fn write_binding(
    conn: &Connection,
    display_id: &str,
    binding: Option<&DisplayBinding>,
) -> Result<()> {
    conn.execute(
        "UPDATE displays SET binding_station = ?2, binding_monitor = ?3 WHERE display_id = ?1",
        params![
            display_id,
            binding.map(|b| b.station_id.as_str()),
            binding.map(|b| b.monitor.as_str())
        ],
    )?;
    Ok(())
}

impl DeckStore {
    /// Starts a `wall.open` launch for `display_id` on `station`.
    ///
    /// # Errors
    /// [`BindError::Unknown`], [`BindError::Bound`] when the display is
    /// bound to another station, [`BindError::Invalid`].
    pub async fn display_begin_open(
        &self,
        display_id: String,
        station: StationId,
        monitor: String,
    ) -> std::result::Result<OpenTicket, BindError> {
        check_monitor(&monitor)?;
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction().map_err(DeckError::from)?;
            let ticket = (|| -> std::result::Result<_, BindError> {
                let display = load(&tx, &display_id)?.ok_or(BindError::Unknown)?;
                check_station(&tx, &station)?;
                if display.binding.is_some_and(|b| b.station_id != station) {
                    return Err(BindError::Bound);
                }
                Ok(OpenTicket {
                    op_revision: bump(&tx, &display_id)?,
                })
            })();
            match ticket {
                Ok(t) => {
                    tx.commit().map_err(DeckError::from)?;
                    Ok(Ok(t))
                }
                Err(e) => Ok(Err(e)),
            }
        })
        .await?
    }

    /// Commits the binding after a successful open, when the operation
    /// revision is still `expected_revision`.
    ///
    /// # Errors
    /// [`BindError::Unknown`], [`BindError::Superseded`], [`BindError::Bound`]
    /// for a display bound to another station, [`BindError::Invalid`].
    pub async fn display_commit_binding(
        &self,
        display_id: String,
        expected_revision: i64,
        station: StationId,
        monitor: String,
    ) -> std::result::Result<(), BindError> {
        check_monitor(&monitor)?;
        let changed = call(&self.inner.db, move |conn| {
            let tx = conn.transaction().map_err(DeckError::from)?;
            let result = (|| -> std::result::Result<_, BindError> {
                let display = load(&tx, &display_id)?.ok_or(BindError::Unknown)?;
                check_station(&tx, &station)?;
                if display.binding_revision != expected_revision {
                    return Err(BindError::Superseded);
                }
                if display
                    .binding
                    .as_ref()
                    .is_some_and(|b| b.station_id != station)
                {
                    return Err(BindError::Bound);
                }
                let new = DisplayBinding {
                    station_id: station,
                    monitor,
                };
                let changed = display.binding.as_ref() != Some(&new);
                if changed {
                    write_binding(&tx, &display_id, Some(&new))?;
                    invalidate_directive(&tx, &display_id)?;
                }
                Ok(changed)
            })();
            match result {
                Ok(changed) => {
                    tx.commit().map_err(DeckError::from)?;
                    Ok(Ok(changed))
                }
                Err(e) => Ok(Err(e)),
            }
        })
        .await??;
        if changed {
            self.board_changed();
        }
        Ok(())
    }

    /// Clears the binding after a successful explicit close. False when the
    /// display does not exist or is revoked.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_clear_binding(&self, display_id: String) -> Result<bool> {
        let (found, changed) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(display) = load(&tx, &display_id)? else {
                return Ok((false, false));
            };
            bump(&tx, &display_id)?;
            let changed = display.binding.is_some();
            if changed {
                write_binding(&tx, &display_id, None)?;
                invalidate_directive(&tx, &display_id)?;
            }
            tx.commit()?;
            Ok((true, changed))
        })
        .await?;
        if changed {
            self.board_changed();
        }
        Ok(found)
    }

    /// Clears the binding after a successful explicit close, only when it is
    /// still on `station` at `revision`: one statement, so an owner edit or
    /// a new launch in between is kept. The revision moves up and the
    /// directive goes, as in [`Self::display_clear_binding`], when a row
    /// changed. False when the binding changed meanwhile (not an error).
    ///
    /// # Errors
    /// A database error.
    pub async fn display_clear_binding_at(
        &self,
        display_id: String,
        station: StationId,
        revision: i64,
    ) -> Result<bool> {
        let changed = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let rows = tx.execute(
                "UPDATE displays
                 SET binding_station = NULL, binding_monitor = NULL,
                     binding_revision = binding_revision + 1
                 WHERE display_id = ?1 AND binding_station = ?2 AND binding_revision = ?3",
                params![display_id, station.as_str(), revision],
            )?;
            if rows == 1 {
                invalidate_directive(&tx, &display_id)?;
            }
            tx.commit()?;
            Ok(rows == 1)
        })
        .await?;
        if changed {
            self.board_changed();
        }
        Ok(changed)
    }

    /// The owner's edit of the binding in the settings: `Some` sets or
    /// changes it, `None` clears it. The revision moves up.
    ///
    /// # Errors
    /// [`BindError::Unknown`], [`BindError::Invalid`].
    pub async fn display_set_binding(
        &self,
        display_id: String,
        binding: Option<DisplayBinding>,
    ) -> std::result::Result<DisplayRecord, BindError> {
        if let Some(b) = &binding {
            check_monitor(&b.monitor)?;
        }
        let (record, changed) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction().map_err(DeckError::from)?;
            let result = (|| -> std::result::Result<_, BindError> {
                let display = load(&tx, &display_id)?.ok_or(BindError::Unknown)?;
                if let Some(b) = &binding {
                    check_station(&tx, &b.station_id)?;
                }
                bump(&tx, &display_id)?;
                let changed = display.binding != binding;
                if changed {
                    write_binding(&tx, &display_id, binding.as_ref())?;
                    invalidate_directive(&tx, &display_id)?;
                }
                let record = load(&tx, &display_id)?.ok_or(BindError::Unknown)?;
                Ok((record, changed))
            })();
            match result {
                Ok(v) => {
                    tx.commit().map_err(DeckError::from)?;
                    Ok(Ok(v))
                }
                Err(e) => Ok(Err(e)),
            }
        })
        .await??;
        if changed {
            self.board_changed();
        }
        Ok(record)
    }
}
