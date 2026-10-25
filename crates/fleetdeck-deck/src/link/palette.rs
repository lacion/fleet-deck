//! `station.palette` (R13 P4 task F1, feature `desktop_shell.v1`): a station
//! sends the Omarchy theme palette of its desktop to the deck. Design
//! `omarchy-p4.md` §3.7.
//!
//! The params are exactly [`StationPalette`]: unknown keys and other shapes
//! are `invalid_params`, and so is a body above [`MAX_PALETTE_BYTES`]. The
//! deck stores the palette under the calling station only: the station id
//! comes from the link and the params cannot name one. No audit row (a
//! palette is not an owner action).

use fleetdeck_link::methods::code;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::palette::{MAX_PALETTE_BYTES, StationPalette};

use super::DeckLink;
use crate::store::station_palettes::palette_ok;

pub(super) const INVALID: &str = "The palette is not valid.";

/// Parses and checks the params: size, shape, version and colors.
pub(super) fn parse(params: &serde_json::Value) -> Option<StationPalette> {
    if params.to_string().len() > MAX_PALETTE_BYTES {
        return None;
    }
    serde_json::from_value::<StationPalette>(params.clone())
        .ok()
        .filter(palette_ok)
}

/// `station.palette`: the answer is `{"ok":true}`.
pub(super) async fn station_palette(
    ctx: &DeckLink,
    station: &StationId,
    params: serde_json::Value,
) -> Outcome {
    let Some(palette) = parse(&params) else {
        return Outcome::Err(WireError::new("invalid_params", INVALID));
    };
    match ctx
        .store
        .put_station_palette(station.clone(), palette)
        .await
    {
        Ok(()) => Outcome::Ok(serde_json::json!({ "ok": true })),
        Err(e) => {
            tracing::warn!(error = %e, "station.palette");
            Outcome::Err(WireError::new(code::PROTOCOL_ERROR, "deck store error"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> serde_json::Value {
        json!({"v":1,"mode":"dark","colors":{
            "background":"#000000","foreground":"#FFFFFF","accent":"#112233",
            "muted":"#222222","dark_background":"#010101","dark_foreground":"#fefefe",
            "bright_foreground":"#ffffff","selection":"#334455","red":"#ff0000",
            "yellow":"#ffff00"}})
    }

    #[test]
    fn a_good_palette_parses_and_keeps_the_case_of_its_colors() {
        let p = parse(&body()).unwrap();
        assert_eq!(p.colors.foreground, "#FFFFFF");
        assert_eq!(p.colors.dark_foreground, "#fefefe");
    }

    #[test]
    fn a_palette_with_a_wrong_shape_is_refused() {
        let mut cases = Vec::new();
        let mut v2 = body();
        v2["v"] = json!(2);
        cases.push(v2);
        let mut mode = body();
        mode["mode"] = json!("blue");
        cases.push(mode);
        let mut color = body();
        color["colors"]["red"] = json!("#ff00");
        cases.push(color);
        let mut missing = body();
        missing["colors"].as_object_mut().unwrap().remove("red");
        cases.push(missing);
        let mut extra = body();
        extra["colors"]["color3"] = json!("#000000");
        cases.push(extra);
        // The params cannot name a station.
        let mut station = body();
        station["station_id"] = json!("other");
        cases.push(station);
        cases.push(json!({}));
        cases.push(json!("text"));
        for case in cases {
            assert!(parse(&case).is_none(), "{case}");
        }
    }

    #[test]
    fn a_body_above_2_kib_is_refused() {
        let mut big = body();
        big["padding"] = json!("x".repeat(MAX_PALETTE_BYTES));
        assert!(parse(&big).is_none());
    }
}
