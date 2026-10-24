//! Station traits and placement preferences (design §9, FG-6, O19, O20).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The traits of a station in `station.info` (design §9.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct StationTraits {
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// `None`: the station cannot tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub on_battery: Option<bool>,
    /// The `traits` of `station.toml`.
    #[serde(default)]
    pub labels: Vec<String>,
}

/// A placement preference of a start (design §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum Prefer {
    Idle,
    NotOnBattery,
    AlwaysOn,
    /// A preference of a newer board. It counts as never satisfied.
    #[serde(other)]
    Unknown,
}
