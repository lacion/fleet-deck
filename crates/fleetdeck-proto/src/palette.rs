//! The Omarchy theme palette that a station sends to the deck (R13 P4,
//! `omarchy-p4.md` §3.7; link method `station.palette`, feature
//! `desktop_shell.v1`).
//!
//! The ten color keys are the keys that the Omarchy theme template proves.
//! The types carry no validation of the color grammar; the station intake
//! and the deck do that in their own modules.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The largest `station.palette` body, in bytes (2 KiB).
pub const MAX_PALETTE_BYTES: usize = 2048;

/// Whether the theme is dark or light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "palette/")]
pub enum PaletteMode {
    Dark,
    Light,
}

/// The ten colors, each `#rrggbb`. An unknown key is a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "palette/")]
pub struct PaletteColors {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub muted: String,
    pub dark_background: String,
    pub dark_foreground: String,
    pub bright_foreground: String,
    pub selection: String,
    pub red: String,
    pub yellow: String,
}

/// The params of `station.palette`: `{"v":1,"mode":…,"colors":{…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "palette/")]
pub struct StationPalette {
    /// The version of the shape: 1.
    pub v: u32,
    pub mode: PaletteMode,
    pub colors: PaletteColors,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r##"{"v":1,"mode":"dark","colors":{"background":"#000000","foreground":"#ffffff","accent":"#112233","muted":"#222222","dark_background":"#010101","dark_foreground":"#fefefe","bright_foreground":"#ffffff","selection":"#334455","red":"#ff0000","yellow":"#ffff00"}}"##;

    #[test]
    fn a_palette_round_trips() {
        let p: StationPalette = serde_json::from_str(BODY).unwrap();
        assert_eq!(p.mode, PaletteMode::Dark);
        assert_eq!(p.colors.accent, "#112233");
        let again: StationPalette =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(again, p);
        assert!(BODY.len() <= MAX_PALETTE_BYTES);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let extra = BODY.replace("\"yellow\"", "\"color3\":\"#000000\",\"yellow\"");
        assert!(serde_json::from_str::<StationPalette>(&extra).is_err());
        let top = BODY.replace("\"v\":1,", "\"v\":1,\"x\":1,");
        assert!(serde_json::from_str::<StationPalette>(&top).is_err());
    }

    #[test]
    fn a_missing_key_and_a_bad_mode_are_refused() {
        let missing = BODY.replace("\"red\":\"#ff0000\",", "");
        assert!(serde_json::from_str::<StationPalette>(&missing).is_err());
        let mode = BODY.replace("dark\"", "blue\"");
        assert!(serde_json::from_str::<StationPalette>(&mode).is_err());
    }
}
