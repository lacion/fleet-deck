//! The local socket op `palette_set` (R13 P4 task F1, design `omarchy-p4.md`
//! §3.7): the answer for `fleetdeck omarchy theme-sync`.
//!
//! The station checks the palette again, keeps it in memory (never on disk)
//! in its [`SummaryCell`], and sends `station.palette` over the current deck
//! link that negotiated `desktop_shell.v1`. The op answers
//! [`LocalResponse::PaletteStored`]: `sent` is true only when the deck
//! answered ok. [`resend`] sends the stored palette again at each link up.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::palette::StationPalette;

use crate::fleet_summary::SummaryCell;
use crate::link::StationNode;
use crate::local::LocalResponse;

/// The time that one `station.palette` request may take.
pub const SEND_LIMIT: Duration = Duration::from_secs(10);

/// The text of the local error for a palette that is not valid.
pub const INVALID: &str = "The palette is not valid.";

/// `#rrggbb`: a hash and six hex digits of either case.
fn color_ok(color: &str) -> bool {
    color
        .strip_prefix('#')
        .is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// True when the shape is version 1 and each of the ten colors is `#rrggbb`.
#[must_use]
pub fn palette_valid(palette: &StationPalette) -> bool {
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

/// Sends `palette` to the deck over `control` and waits at most `limit`.
/// True when the deck answered ok. A refusal, a time-out and a link that is
/// gone are false (logged at debug).
pub async fn send_over(control: &Control, palette: &StationPalette, limit: Duration) -> bool {
    let params = match serde_json::to_value(palette) {
        Ok(params) => params,
        Err(e) => {
            tracing::debug!(error = %e, "station.palette: the palette does not serialize");
            return false;
        }
    };
    match tokio::time::timeout(limit, control.request("station.palette", None, params)).await {
        Ok(Ok(Outcome::Ok(_))) => true,
        Ok(Ok(Outcome::Err(e))) => {
            tracing::debug!(code = %e.code, "station.palette: the deck refused");
            false
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "station.palette: the link is gone");
            false
        }
        Err(_) => {
            tracing::debug!("station.palette: the deck did not answer in time");
            false
        }
    }
}

/// Keeps `palette` in `cell` and sends it over the current deck control of
/// the cell. True when the deck answered ok.
pub async fn store_and_send(cell: &SummaryCell, palette: StationPalette) -> bool {
    cell.set_palette(palette.clone());
    match cell.deck() {
        Some(control) => send_over(&control, &palette, SEND_LIMIT).await,
        None => false,
    }
}

/// Sends the stored palette of `cell` once over `control`, in its own task.
/// Called when a link with `desktop_shell.v1` comes up. Nothing is sent
/// without a stored palette.
pub fn resend(cell: &Arc<SummaryCell>, control: &Control) {
    let (cell, control) = (Arc::clone(cell), control.clone());
    tokio::spawn(async move {
        if let Some(palette) = cell.palette() {
            let _sent = send_over(&control, &palette, SEND_LIMIT).await;
        }
    });
}

/// The op as `local::handle` calls it. Without a link node there is no cell
/// to keep the palette in: nothing is stored and `sent` is false.
pub(crate) async fn palette_set(
    node: Option<&StationNode>,
    palette: StationPalette,
) -> LocalResponse {
    if !palette_valid(&palette) {
        return LocalResponse::Error {
            message: INVALID.to_owned(),
        };
    }
    let sent = match node {
        Some(node) => store_and_send(&node.link().summary, palette).await,
        None => false,
    };
    LocalResponse::PaletteStored { sent }
}

#[cfg(test)]
pub(crate) mod tests {
    use fleetdeck_proto::palette::{PaletteColors, PaletteMode};

    use super::*;
    use crate::local::LocalRequest;

    /// A valid palette; `background` is `color`.
    pub(crate) fn sample(color: &str) -> StationPalette {
        StationPalette {
            v: 1,
            mode: PaletteMode::Dark,
            colors: PaletteColors {
                background: color.to_owned(),
                foreground: "#ffffff".to_owned(),
                accent: "#112233".to_owned(),
                muted: "#222222".to_owned(),
                dark_background: "#010101".to_owned(),
                dark_foreground: "#fefefe".to_owned(),
                bright_foreground: "#FFFFFF".to_owned(),
                selection: "#334455".to_owned(),
                red: "#ff0000".to_owned(),
                yellow: "#ffff00".to_owned(),
            },
        }
    }

    #[test]
    fn a_color_is_a_hash_and_six_hex_digits_of_either_case() {
        for ok in ["#000000", "#ffffff", "#FFFFFF", "#aB09fE"] {
            assert!(color_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "#",
            "000000",
            "#00000",
            "#0000000",
            "#00000g",
            " #000000",
            "#000000\n",
            "#00 000",
            "##00000",
            "#+00000",
            "red",
        ] {
            assert!(!color_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_palette_needs_version_1_and_ten_good_colors() {
        assert!(palette_valid(&sample("#000000")));
        assert!(!palette_valid(&sample("#00000")));
        let mut v2 = sample("#000000");
        v2.v = 2;
        assert!(!palette_valid(&v2));
        let mut yellow = sample("#000000");
        yellow.colors.yellow = "yellow".to_owned();
        assert!(!palette_valid(&yellow));
    }

    #[test]
    fn the_palette_wire_is_palette_set_and_palette_stored() {
        let request = LocalRequest::PaletteSet {
            palette: sample("#000000"),
        };
        let line = serde_json::to_string(&request).unwrap();
        assert!(line.starts_with(r#"{"op":"palette_set","palette":{"v":1,"mode":"dark","#));
        let back: LocalRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(back, request);
        // An extra key in the palette is refused.
        let extra = line.replace(r#""v":1,"#, r#""v":1,"x":1,"#);
        assert!(serde_json::from_str::<LocalRequest>(&extra).is_err());

        let stored = serde_json::to_string(&LocalResponse::PaletteStored { sent: true }).unwrap();
        assert_eq!(stored, r#"{"status":"palette_stored","sent":true}"#);
        let back: LocalResponse = serde_json::from_str(&stored).unwrap();
        assert_eq!(back, LocalResponse::PaletteStored { sent: true });
    }

    #[tokio::test]
    async fn an_invalid_palette_is_refused_and_not_kept() {
        let mut bad = sample("#000000");
        bad.colors.red = "#ff00".to_owned();
        let got = palette_set(None, bad).await;
        assert_eq!(
            got,
            LocalResponse::Error {
                message: "The palette is not valid.".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn without_a_link_node_nothing_is_sent() {
        let got = palette_set(None, sample("#000000")).await;
        assert_eq!(got, LocalResponse::PaletteStored { sent: false });
    }

    #[tokio::test]
    async fn without_a_deck_control_the_palette_is_kept_and_not_sent() {
        let cell = SummaryCell::new();
        assert!(!store_and_send(&cell, sample("#abcdef")).await);
        assert_eq!(cell.palette(), Some(sample("#abcdef")));
    }
}
