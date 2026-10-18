//! The Omarchy palette of each station (R13 P4 task F1, design
//! `omarchy-p4.md` §3.7).
//!
//! A station sends its theme palette with the link method `station.palette`
//! ([`crate::link::palette`]). The deck keeps one row for each station. The
//! row goes in the same transaction that removes the station
//! ([`delete_station`]). The owner route and the display route read it
//! ([`crate::http::palette_routes`]).

use fleetdeck_proto::StationId;
use fleetdeck_proto::palette::{PaletteColors, PaletteMode, StationPalette};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::{DeckError, DeckStore, Result, call};

/// The migration (V33).
pub(super) const SQL: &str = "
CREATE TABLE station_palettes (          -- R13 P4: the Omarchy palette of each station
    station_id TEXT    PRIMARY KEY,
    mode       TEXT    NOT NULL,         -- dark, light
    colors     TEXT    NOT NULL,         -- PaletteColors JSON
    updated_at INTEGER NOT NULL
) STRICT;
";

/// The palette of one station, as the routes answer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredPalette {
    pub mode: PaletteMode,
    pub colors: PaletteColors,
    pub updated_at: i64,
}

/// `#rrggbb`: a hash and six hex digits of either case.
#[must_use]
pub fn color_ok(color: &str) -> bool {
    color
        .strip_prefix('#')
        .is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// True when the shape is version 1 and each of the ten colors is `#rrggbb`.
#[must_use]
pub fn palette_ok(palette: &StationPalette) -> bool {
    let c = &palette.colors;
    palette.v == 1
        && [
            &c.background,
            &c.foreground,
            &c.accent,
            &c.muted,
            &c.dark_background,
            &c.dark_foreground,
            &c.bright_foreground,
            &c.selection,
            &c.red,
            &c.yellow,
        ]
        .into_iter()
        .all(|color| color_ok(color))
}

fn mode_name(mode: PaletteMode) -> &'static str {
    match mode {
        PaletteMode::Dark => "dark",
        PaletteMode::Light => "light",
    }
}

fn parse_mode(text: &str) -> Result<PaletteMode> {
    match text {
        "dark" => Ok(PaletteMode::Dark),
        "light" => Ok(PaletteMode::Light),
        other => Err(DeckError::Corrupt(format!(
            "a stored palette has the mode `{other}`"
        ))),
    }
}

/// Deletes the palette of a station. The caller holds the transaction.
pub(super) fn delete_station(conn: &Connection, station: &StationId) -> Result<()> {
    conn.execute(
        "DELETE FROM station_palettes WHERE station_id = ?1",
        [station.as_str()],
    )?;
    Ok(())
}

impl DeckStore {
    /// Stores the palette of `station`: one row, replaced by each call. The
    /// colors are stored as given.
    ///
    /// # Errors
    /// A database error.
    pub async fn put_station_palette(
        &self,
        station: StationId,
        palette: StationPalette,
    ) -> Result<()> {
        let now = self.now_ms();
        let colors = serde_json::to_string(&palette.colors)?;
        let mode = mode_name(palette.mode);
        call(&self.inner.db, move |conn| {
            conn.execute(
                // Only while the station row exists: a call that races with
                // `revoke_station` must not leave an orphan row.
                "INSERT INTO station_palettes (station_id, mode, colors, updated_at)
                 SELECT ?1, ?2, ?3, ?4
                 WHERE EXISTS (SELECT 1 FROM stations WHERE station_id = ?1)
                 ON CONFLICT(station_id) DO UPDATE
                 SET mode = excluded.mode, colors = excluded.colors,
                     updated_at = excluded.updated_at",
                params![station.as_str(), mode, colors, now],
            )?;
            Ok(())
        })
        .await
    }

    /// The palette of `station`, if it sent one.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub async fn station_palette(&self, station: StationId) -> Result<Option<StoredPalette>> {
        let row = call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT mode, colors, updated_at FROM station_palettes
                     WHERE station_id = ?1",
                    [station.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?)
        })
        .await?;
        row.map(|(mode, colors, updated_at)| {
            Ok(StoredPalette {
                mode: parse_mode(&mode)?,
                colors: serde_json::from_str(&colors)?,
                updated_at,
            })
        })
        .transpose()
    }
}

#[cfg(test)]
#[path = "station_palettes_tests.rs"]
mod tests;
