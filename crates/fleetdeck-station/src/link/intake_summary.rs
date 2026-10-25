//! `fleet.summary` (R13 P4, feature `desktop_shell.v1`): the fleet summary
//! that the deck sends to this station.
//!
//! The station keeps the last valid summary in memory only (see
//! `crate::fleet_summary`) and never writes it to disk.

use fleetdeck_proto::fleet_summary::{
    FleetSummary, MAX_BOARD, MAX_CALLSIGN, MAX_NEEDS_YOU, MAX_STATION_NAME, MAX_STATIONS,
    MAX_SUMMARY_BYTES,
};
use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;
use crate::fleet_summary::SummaryCell;

/// The text of the refusal when the setting is off.
pub(super) const OFF: &str = "The fleet summary is off on this station.";
/// The text for every parse or limit failure.
pub(super) const INVALID: &str = "The fleet summary is not valid.";

/// `fleet.summary`: refuses when the live setting is off, else checks and
/// stores the summary.
pub(super) fn fleet_summary(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    receive(&ctx.summary, ctx.manager.policy().fleet_summary(), params)
}

fn invalid() -> Outcome {
    Outcome::Err(WireError::new("invalid_params", INVALID))
}

/// The intake without the link: `enabled` is the live setting.
pub(super) fn receive(cell: &SummaryCell, enabled: bool, params: serde_json::Value) -> Outcome {
    if !enabled {
        return Outcome::Err(WireError::new("forbidden", OFF));
    }
    match serde_json::to_vec(&params) {
        Ok(bytes) if bytes.len() <= MAX_SUMMARY_BYTES => {}
        _ => return invalid(),
    }
    let Ok(summary) = serde_json::from_value::<FleetSummary>(params) else {
        return invalid();
    };
    if !within_limits(&summary) {
        return invalid();
    }
    cell.store(summary);
    Outcome::Ok(serde_json::json!({ "ok": true }))
}

/// `^[A-Za-z0-9_-]{1,64}$`.
fn id_ok(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `null` is checked by the caller. A lower-case `http://` or `https://`,
/// at most [`MAX_BOARD`] bytes, and a non-empty rest without whitespace,
/// control characters, `/`, `?`, `#` or `@`.
fn board_ok(board: &str) -> bool {
    if board.len() > MAX_BOARD {
        return false;
    }
    let Some(rest) = board
        .strip_prefix("https://")
        .or_else(|| board.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '/' | '?' | '#' | '@'))
}

fn within_limits(summary: &FleetSummary) -> bool {
    summary.v == 1
        && summary.board.as_deref().is_none_or(board_ok)
        && summary.needs_you.len() <= MAX_NEEDS_YOU
        && summary.stations.len() <= MAX_STATIONS
        && summary.needs_you.iter().all(|item| {
            item.station.len() <= MAX_STATION_NAME
                && id_ok(&item.station_id)
                && id_ok(&item.session)
                && id_ok(&item.request)
                && item
                    .callsign
                    .as_ref()
                    .is_none_or(|c| c.len() <= MAX_CALLSIGN)
        })
        && summary
            .stations
            .iter()
            .all(|station| station.name.len() <= MAX_STATION_NAME)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn valid() -> Value {
        json!({
            "v": 1,
            "at": "2026-10-18T12:04:00Z",
            "board": "https://deck.example.ts.net:7420",
            "deck": "up",
            "counts": {"working": 7, "idle": 3, "needs_you": 2, "blocked": 1, "lost": 0},
            "needs_you": [{
                "station": "srv", "station_id": "st-1", "session": "s_1", "request": "r1",
                "callsign": "lynx-12", "kind": "tool", "age_s": 95
            }],
            "usage": {"label": "claude · 5 h", "pct": 63, "resets_in_s": 4320},
            "stations": [{"name": "srv", "up": true, "cpu_load_pct": 41}]
        })
    }

    fn code(outcome: &Outcome) -> &str {
        match outcome {
            Outcome::Err(e) => &e.code,
            Outcome::Ok(_) => "ok",
        }
    }

    fn refused(params: Value) {
        let cell = SummaryCell::new();
        let got = receive(&cell, true, params);
        assert_eq!(code(&got), "invalid_params", "{got:?}");
        let Outcome::Err(e) = got else { unreachable!() };
        assert_eq!(e.message, "The fleet summary is not valid.");
        assert_eq!(cell.read().seq, 0, "nothing is stored");
    }

    #[test]
    fn summary_intake_refuses_when_the_setting_is_off() {
        let cell = SummaryCell::new();
        let got = receive(&cell, false, valid());
        let Outcome::Err(e) = got else {
            panic!("{got:?}");
        };
        assert_eq!(e.code, "forbidden");
        assert_eq!(e.message, "The fleet summary is off on this station.");
        assert_eq!(cell.read().seq, 0);
    }

    #[test]
    fn summary_intake_stores_a_valid_summary_and_seq_grows_for_a_new_json() {
        let cell = SummaryCell::new();
        let got = receive(&cell, true, valid());
        assert_eq!(got, Outcome::Ok(json!({"ok": true})));
        let read = cell.read();
        assert_eq!(read.seq, 1);
        assert_eq!(read.summary.unwrap().needs_you[0].session, "s_1");
        receive(&cell, true, valid());
        assert_eq!(cell.read().seq, 1, "the same JSON keeps seq");
        let mut changed = valid();
        changed["at"] = json!("2026-10-18T12:05:00Z");
        receive(&cell, true, changed);
        assert_eq!(cell.read().seq, 2);
    }

    #[test]
    fn summary_intake_accepts_a_null_board_and_a_plain_http_board() {
        for board in [Value::Null, json!("http://192.168.1.5:7420")] {
            let mut params = valid();
            params["board"] = board;
            let got = receive(&SummaryCell::new(), true, params);
            assert_eq!(got, Outcome::Ok(json!({"ok": true})));
        }
    }

    #[test]
    fn summary_intake_refuses_text_that_does_not_parse() {
        refused(json!("nope"));
        refused(json!({"v": 1}));
        let mut params = valid();
        params["needs_you"][0]["kind"] = json!("approval");
        refused(params);
    }

    #[test]
    fn summary_intake_refuses_another_version() {
        let mut params = valid();
        params["v"] = json!(2);
        refused(params);
    }

    #[test]
    fn summary_intake_refuses_a_summary_above_16_kib() {
        let mut params = valid();
        params["usage"]["label"] = json!("x".repeat(MAX_SUMMARY_BYTES));
        refused(params);
    }

    #[test]
    fn summary_intake_refuses_each_limit() {
        let item = valid()["needs_you"][0].clone();
        let mut params = valid();
        params["needs_you"] = Value::Array(vec![item.clone(); 11]);
        refused(params);
        let mut params = valid();
        params["needs_you"] = Value::Array(vec![item; 10]);
        assert_eq!(
            receive(&SummaryCell::new(), true, params),
            Outcome::Ok(json!({"ok": true})),
            "10 items are the limit"
        );
        let station = valid()["stations"][0].clone();
        let mut params = valid();
        params["stations"] = Value::Array(vec![station; 33]);
        refused(params);
        let mut params = valid();
        params["needs_you"][0]["callsign"] = json!("c".repeat(33));
        refused(params);
        let mut params = valid();
        params["needs_you"][0]["station"] = json!("s".repeat(65));
        refused(params);
        let mut params = valid();
        params["stations"][0]["name"] = json!("s".repeat(65));
        refused(params);
    }

    #[test]
    fn summary_intake_refuses_an_id_outside_the_grammar() {
        for field in ["station_id", "session", "request"] {
            for bad in ["", "a b", "a/b", "a.b", "é", &"i".repeat(65)] {
                let mut params = valid();
                params["needs_you"][0][field] = json!(bad);
                refused(params);
            }
            let mut params = valid();
            params["needs_you"][0][field] = json!("A_b-9".repeat(12) + "abcd");
            assert_eq!(
                receive(&SummaryCell::new(), true, params),
                Outcome::Ok(json!({"ok": true})),
                "{field}: 64 bytes are valid"
            );
        }
    }

    #[test]
    fn summary_intake_refuses_a_board_outside_the_origin_rule() {
        for bad in [
            "",
            "https://",
            "HTTPS://deck.example",
            "ftp://deck.example",
            "deck.example",
            "https://deck.example/",
            "https://deck.example/path",
            "https://deck.example?x=1",
            "https://deck.example#f",
            "https://user@deck.example",
            "https://deck example",
            "https://deck\u{7}example",
            "https://deck\texample",
        ] {
            let mut params = valid();
            params["board"] = json!(bad);
            refused(params);
        }
        let mut params = valid();
        params["board"] = json!(format!("https://{}", "h".repeat(MAX_BOARD)));
        refused(params);
        let mut params = valid();
        params["board"] = json!(format!("https://{}", "h".repeat(MAX_BOARD - 8)));
        assert_eq!(
            receive(&SummaryCell::new(), true, params),
            Outcome::Ok(json!({"ok": true})),
            "2048 bytes are valid"
        );
    }
}
