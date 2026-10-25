//! V50 (B17 wall apps): the switch that lets a wall run an HTML artifact as
//! an app. Off for every display that exists. To reverse it, drop the column.

pub(super) const SQL: &str =
    "ALTER TABLE displays ADD COLUMN allow_apps INTEGER NOT NULL DEFAULT 0;";
