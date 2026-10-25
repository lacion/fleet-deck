//! The active-window observation (R13 P7 §7.3 steps 15 and 16): the strict
//! parser and the verdict for a recorded preview.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::desktop::events::Kind;

fn record() -> Record {
    Record {
        signature: "sig".into(),
        address: "0x5a3e8a41".into(),
        class: "fleetdeck-preview-abc".into(),
        kind: Kind::Preview,
        display_id: None,
        monitor: None,
        opener: None,
        launch_id: "abc".into(),
    }
}

fn active(address: &str, class: &str, title: &str) -> Active {
    Active {
        address: address.into(),
        class: class.into(),
        initial_class: class.into(),
        title: title.into(),
        initial_title: title.into(),
    }
}

#[test]
fn an_active_window_is_parsed_strictly() {
    let a = parse_active(
        r#"{"address":"0x5A3E8A41","class":"c","title":"T","initialClass":"ic","initialTitle":"it"}"#,
    )
    .unwrap();
    assert_eq!(a.address, "0x5a3e8a41");
    assert_eq!(
        (
            a.class.as_str(),
            a.title.as_str(),
            a.initial_class.as_str(),
            a.initial_title.as_str()
        ),
        ("c", "T", "ic", "it")
    );
    // An absent title is empty; a wrong type or a missing address or class
    // is malformed.
    assert_eq!(
        parse_active(r#"{"address":"0x1","class":"c"}"#)
            .unwrap()
            .title,
        ""
    );
    for bad in [
        "",
        "nope",
        "[]",
        "5",
        r#"{"class":"c"}"#,
        r#"{"address":"0x1"}"#,
        r#"{"address":"","class":"c"}"#,
        r#"{"address":1,"class":"c"}"#,
        r#"{"address":"0x1","class":"c","title":7}"#,
        r#"{"address":"0x1","class":"c","initialTitle":[]}"#,
    ] {
        assert!(parse_active(bad).is_none(), "{bad}");
    }
}

#[test]
fn no_active_window_is_an_empty_address() {
    for none in ["{}", "null"] {
        let a = parse_active(none).unwrap();
        assert_eq!(a.address, "");
        assert_eq!(verdict(&a, &record(), &[]), Verdict::Foreign);
    }
}

#[test]
fn the_same_address_and_class_is_the_same_window() {
    let a = active("0x5a3e8a41", "fleetdeck-preview-abc", "Dummy page");
    assert_eq!(verdict(&a, &record(), &[]), Verdict::Same);
}

#[test]
fn another_address_or_class_is_foreign_even_when_it_is_a_board() {
    for a in [
        active("0x1", "fleetdeck-preview-abc", "Dummy page"),
        active("0x5a3e8a41", "chromium", "Dummy page"),
        active("0x1", "fleetdeck-board", "Fleet Deck"),
        active("0x2", "chromium", "deck.example"),
    ] {
        assert_eq!(
            verdict(&a, &record(), &["deck.example".into()]),
            Verdict::Foreign
        );
    }
}

#[test]
fn the_recorded_window_that_looks_like_a_board_is_a_board() {
    let by_title = active("0x5a3e8a41", "fleetdeck-preview-abc", "Fleet Deck board");
    assert_eq!(verdict(&by_title, &record(), &[]), Verdict::Board);
    let by_host = active("0x5a3e8a41", "fleetdeck-preview-abc", "x deck.example y");
    assert_eq!(
        verdict(&by_host, &record(), &["deck.example".into()]),
        Verdict::Board
    );
    assert_eq!(verdict(&by_host, &record(), &[]), Verdict::Same);
    let mut wall = record();
    wall.kind = Kind::Wall;
    let a = active("0x5a3e8a41", "fleetdeck-preview-abc", "x");
    assert_eq!(verdict(&a, &wall, &[]), Verdict::Board);
}
