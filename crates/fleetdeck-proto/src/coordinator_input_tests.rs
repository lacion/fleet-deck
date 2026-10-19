//! The schemas of §4 and the grammar of §5 of `omarchy-p7.md`.

use serde_json::{Value, json};

use super::*;
use crate::coordinator_desktop::DesktopCall;

const WINDOW: &str = "0123456789abcdef0123456789abcdef";

fn key(chord: &str) -> KeyPressArguments {
    KeyPressArguments {
        request_id: "r1".into(),
        station: None,
        window_id: WINDOW.into(),
        chord: chord.into(),
    }
}

fn text(value: &str) -> TypeTextArguments {
    TypeTextArguments {
        request_id: "r1".into(),
        station: None,
        window_id: WINDOW.into(),
        text: value.into(),
    }
}

fn point(x: u32, y: u32) -> PointerMoveArguments {
    PointerMoveArguments {
        request_id: "r1".into(),
        station: None,
        window_id: WINDOW.into(),
        x,
        y,
    }
}

fn call(v: Value) -> DesktopCall {
    serde_json::from_value(v).unwrap()
}

#[test]
fn the_three_calls_serialize_as_their_tools_and_skip_an_absent_station() {
    let k = call(json!({"tool": "key_press", "arguments": {
        "request_id": "r1", "window_id": WINDOW, "chord": "Ctrl+Shift+a"}}));
    assert_eq!(k, DesktopCall::KeyPress(key("Ctrl+Shift+a")));
    assert_eq!(
        serde_json::to_value(&k).unwrap(),
        json!({"tool": "key_press", "arguments": {
            "request_id": "r1", "window_id": WINDOW, "chord": "Ctrl+Shift+a"}})
    );
    let t = call(json!({"tool": "type_text", "arguments": {
        "request_id": "r1", "station": "st-1", "window_id": WINDOW, "text": "héllo"}}));
    let DesktopCall::TypeText(a) = &t else {
        panic!("not a type_text")
    };
    assert_eq!(a.station.as_deref(), Some("st-1"));
    assert_eq!(a.text, "héllo");
    assert_eq!(
        serde_json::to_value(&t).unwrap()["arguments"]["station"],
        "st-1"
    );
    let p = call(json!({"tool": "pointer_move", "arguments": {
        "request_id": "r1", "window_id": WINDOW, "x": 0, "y": 32767}}));
    assert_eq!(p, DesktopCall::PointerMove(point(0, 32767)));
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        json!({"tool": "pointer_move", "arguments": {
            "request_id": "r1", "window_id": WINDOW, "x": 0, "y": 32767}})
    );
    for c in [&k, &t, &p] {
        assert_eq!(c.validate(), Ok(()));
    }
}

#[test]
fn unknown_fields_a_missing_field_null_and_wrong_types_are_refused() {
    let base = |tool: &str, extra: Value| {
        let mut args = match tool {
            "key_press" => json!({"request_id": "r", "window_id": WINDOW, "chord": "a"}),
            "type_text" => json!({"request_id": "r", "window_id": WINDOW, "text": "a"}),
            _ => json!({"request_id": "r", "window_id": WINDOW, "x": 1, "y": 2}),
        };
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        json!({"tool": tool, "arguments": args})
    };
    for tool in ["key_press", "type_text", "pointer_move"] {
        assert!(
            serde_json::from_value::<DesktopCall>(base(tool, json!({}))).is_ok(),
            "{tool}"
        );
        // No caller identity, grant, address, executable, argv, environment,
        // monitor, delay, repeat count or scope override (§4).
        for field in [
            "caller",
            "grant_id",
            "generation",
            "address",
            "executable",
            "argv",
            "env",
            "monitor",
            "delay",
            "repeat",
            "scope",
            "input_scope",
            "x2",
        ] {
            let bad = base(tool, json!({ field: 1 }));
            assert!(
                serde_json::from_value::<DesktopCall>(bad).is_err(),
                "{tool} {field}"
            );
        }
        // A null station is not an omission.
        let null = base(tool, json!({"station": null}));
        assert!(
            serde_json::from_value::<DesktopCall>(null).is_err(),
            "{tool}"
        );
        // A top-level extra key is refused too.
        let mut top = base(tool, json!({}));
        top["x"] = json!(1);
        assert!(
            serde_json::from_value::<DesktopCall>(top).is_err(),
            "{tool}"
        );
    }
    for bad in [
        json!({"tool": "key_press", "arguments": {"request_id": "r", "window_id": WINDOW}}),
        json!({"tool": "type_text", "arguments": {"request_id": "r", "window_id": WINDOW}}),
        json!({"tool": "pointer_move", "arguments": {"request_id": "r", "window_id": WINDOW, "x": 1}}),
        json!({"tool": "pointer_move", "arguments": {"request_id": "r", "window_id": WINDOW, "x": -1, "y": 0}}),
        json!({"tool": "pointer_move", "arguments": {"request_id": "r", "window_id": WINDOW, "x": 1.5, "y": 0}}),
        json!({"tool": "pointer_move", "arguments": {"request_id": "r", "window_id": WINDOW, "x": "1", "y": 0}}),
        json!({"tool": "pointer_move", "arguments": {"request_id": "r", "window_id": WINDOW, "x": 1, "y": null}}),
        json!({"tool": "key_press", "arguments": {"request_id": "r", "window_id": WINDOW, "chord": null}}),
        // Scroll is not a tool of any contract; a click needs `x` and `y`
        // (R13 P7b §5.1; its own tests are in `coordinator_input_p7b_tests.rs`).
        json!({"tool": "pointer_click", "arguments": {"request_id": "r", "window_id": WINDOW}}),
        json!({"tool": "scroll", "arguments": {"request_id": "r", "window_id": WINDOW}}),
    ] {
        assert!(
            serde_json::from_value::<DesktopCall>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn the_request_id_station_and_window_id_follow_the_schema_patterns() {
    let ok = |c: DesktopCall| c.validate();
    // request_id: 1 to 64 of [A-Za-z0-9_-].
    for good in ["r", "A-b_9", &"x".repeat(64)] {
        let mut a = key("a");
        a.request_id = good.into();
        assert_eq!(ok(DesktopCall::KeyPress(a)), Ok(()), "{good}");
    }
    for bad in ["", &"x".repeat(65), "a b", "a.b", "é", "a\n"] {
        let mut a = key("a");
        a.request_id = bad.into();
        assert_eq!(
            ok(DesktopCall::KeyPress(a)),
            Err(ArgError {
                field: "request_id"
            })
        );
    }
    // station: 1 to 128 of [A-Za-z0-9_-].
    for good in ["s", &"s".repeat(128)] {
        let mut a = text("a");
        a.station = Some(good.into());
        assert_eq!(ok(DesktopCall::TypeText(a)), Ok(()));
    }
    for bad in ["", &"s".repeat(129), "s/1", "s s"] {
        let mut a = text("a");
        a.station = Some(bad.into());
        assert_eq!(
            ok(DesktopCall::TypeText(a)),
            Err(ArgError { field: "station" })
        );
    }
    // window_id: exactly 32 lowercase hexadecimal digits.
    for bad in [
        "",
        &WINDOW[..31],
        &format!("{WINDOW}0"),
        &WINDOW.to_uppercase(),
        "0123456789abcdef0123456789abcdeg",
        "0123456789abcdef0123456789abcde ",
        // 32 bytes but not 32 hexadecimal digits (a multi-byte scalar).
        "0123456789abcdef0123456789abcdé",
    ] {
        let mut a = point(1, 1);
        a.window_id = bad.into();
        assert_eq!(
            ok(DesktopCall::PointerMove(a)),
            Err(ArgError { field: "window_id" }),
            "{bad}"
        );
    }
}

#[test]
fn a_chord_has_the_exact_grammar_of_the_contract() {
    for good in [
        "a",
        "z",
        "0",
        "9",
        "Shift+a",
        "Ctrl+a",
        "Ctrl+Shift+a",
        "Return",
        "Shift+Tab",
        "Ctrl+Home",
        "Tab",
        "Escape",
        "BackSpace",
        "Delete",
        "Insert",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Left",
        "Right",
        "Up",
        "Down",
        "Space",
        "Ctrl+Shift+PageDown",
    ] {
        assert_eq!(key(good).validate(), Ok(()), "{good}");
    }
    for bad in [
        // The rejected examples of §5.1.
        "CTRL+A",
        "Control+a",
        "Shift+Ctrl+a",
        "Ctrl+Ctrl+a",
        "Alt+Tab",
        "Super+Return",
        "F5",
        "Ctrl++",
        "a,b",
        // Modifiers alone, repeated or half written.
        "Ctrl",
        "Shift",
        "Ctrl+",
        "Ctrl+Shift+",
        "Shift+Shift+a",
        "Ctrl+Shift+Ctrl+a",
        // Empty, upper case letters, symbols, aliases, other cases, numeric codes.
        "",
        "A",
        "Shift+A",
        "+",
        "-",
        "space",
        "RETURN",
        "return",
        "Enter",
        "Backspace",
        "Esc",
        "Page_Up",
        "Pgup",
        "KP_Enter",
        "F1",
        "XF86AudioMute",
        "0x41",
        "65",
        "ab",
        // Spaces, controls, non-ASCII, a second key and a sequence.
        " a",
        "a ",
        "Ctrl+ a",
        "Ctrl+a\n",
        "\u{0000}",
        "é",
        "Ctrl+é",
        "a+b",
        "Ctrl+a+b",
        "Ctrl+a Ctrl+b",
        &"a".repeat(33),
    ] {
        assert_eq!(
            key(bad).validate(),
            Err(ArgError { field: "chord" }),
            "{bad:?}"
        );
    }
}

#[test]
fn a_text_is_1_to_2000_scalars_and_8000_bytes_without_controls() {
    for good in [
        "a",
        "hello world",
        "-",
        "--help",
        "-- -",
        "quotes \"'`$\\ ok",
        "héllo wörld",
        "日本語のテキスト",
        // Supplementary characters and combining characters count as scalars.
        "😀",
        "e\u{0301}",
        // Spaces and no-break space are not controls; U+00A0 is above U+009F.
        "  leading and trailing  ",
        "\u{00A0}",
        // The last scalar before and after the forbidden C1 and line ranges.
        "\u{00A0}\u{2027}\u{202A}",
    ] {
        assert_eq!(text(good).validate(), Ok(()), "{good:?}");
    }
    // The scalar limit: 2,000 pass, 2,001 fail.
    assert_eq!(text(&"a".repeat(2000)).validate(), Ok(()));
    assert_eq!(
        text(&"a".repeat(2001)).validate(),
        Err(ArgError { field: "text" })
    );
    // Scalars, not UTF-16 code units: 2,000 supplementary characters are 2,000
    // scalars (4,000 code units) and 8,000 bytes, so they pass exactly at both
    // limits; one more scalar fails.
    assert_eq!(text(&"😀".repeat(2000)).validate(), Ok(()));
    assert_eq!(
        text(&"😀".repeat(2001)).validate(),
        Err(ArgError { field: "text" })
    );
    // The byte limit of 8,000 is 4 bytes for each of 2,000 scalars, so the
    // scalar limit already implies it: the longest text is exactly 8,000 bytes.
    let longest = "😀".repeat(2000);
    assert_eq!(longest.len(), TEXT_MAX_BYTES);
    assert_eq!(TEXT_MAX_SCALARS * 4, TEXT_MAX_BYTES);
    let mixed = format!("{}{}", "😀".repeat(1999), "€");
    assert_eq!(mixed.len(), 7996 + 3);
    assert_eq!(text(&mixed).validate(), Ok(()));
    // Empty is refused.
    assert_eq!(text("").validate(), Err(ArgError { field: "text" }));
}

#[test]
fn every_forbidden_character_of_a_text_is_refused() {
    let forbidden = ('\u{0000}'..='\u{001F}')
        .chain('\u{007F}'..='\u{009F}')
        .chain(['\u{2028}', '\u{2029}']);
    let mut count = 0;
    for c in forbidden {
        count += 1;
        for t in [
            c.to_string(),
            format!("ab{c}"),
            format!("{c}ab"),
            format!("a{c}b"),
        ] {
            assert_eq!(
                text(&t).validate(),
                Err(ArgError { field: "text" }),
                "U+{:04X}",
                u32::from(c)
            );
        }
    }
    // 32 C0 controls, DEL and 32 C1 controls (U+007F to U+009F), and two
    // line separators.
    assert_eq!(count, 32 + 33 + 2);
    // Newline, carriage return, tab, Escape, NUL and backspace by name (§5.2).
    for t in ["a\nb", "a\r\nb", "a\tb", "\u{1b}", "a\0b", "a\u{8}"] {
        assert_eq!(text(t).validate(), Err(ArgError { field: "text" }), "{t:?}");
    }
    // The JSON escape of a lone surrogate is not valid text (§4).
    for lone in [r#""\ud800""#, r#""\udc00""#, r#""a\ud83dz""#] {
        assert!(serde_json::from_str::<String>(lone).is_err(), "{lone}");
    }
}

#[test]
fn text_is_kept_exactly_and_the_error_never_holds_the_value() {
    let sentinel = "SENTINEL-\u{0007}-text";
    let err = text(sentinel).validate().unwrap_err();
    assert_eq!(err.field, "text");
    assert!(!format!("{err:?}").contains("SENTINEL"));
    // Accepted text is not trimmed, normalized or changed by a round trip.
    let exact = "  e\u{0301}  --  ";
    let a = text(exact);
    assert_eq!(a.validate(), Ok(()));
    let back: TypeTextArguments =
        serde_json::from_value(serde_json::to_value(&a).unwrap()).unwrap();
    assert_eq!(back.text, exact);
    // The error for a chord and a window does not hold the value either.
    let err = key("SECRET+chord").validate().unwrap_err();
    assert!(!format!("{err:?}").contains("SECRET"));
    let mut w = key("a");
    w.window_id = "SECRETWINDOW".into();
    let err = w.validate().unwrap_err();
    assert!(!format!("{err:?}").contains("SECRET"));
}

#[test]
fn pointer_coordinates_are_integers_from_0_to_32767() {
    for (x, y) in [(0, 0), (1, 1), (32767, 32767), (0, 32767), (32767, 0)] {
        assert_eq!(point(x, y).validate(), Ok(()), "{x} {y}");
    }
    assert_eq!(point(32768, 0).validate(), Err(ArgError { field: "x" }));
    assert_eq!(point(0, 32768).validate(), Err(ArgError { field: "y" }));
    assert_eq!(point(u32::MAX, 0).validate(), Err(ArgError { field: "x" }));
    assert_eq!(point(0, u32::MAX).validate(), Err(ArgError { field: "y" }));
    // The first failing field is named: `x` before `y`.
    assert_eq!(point(32768, 32768).validate(), Err(ArgError { field: "x" }));
}

#[test]
fn the_fields_are_checked_in_the_order_of_the_schema() {
    let mut a = key("bad chord");
    a.request_id = String::new();
    a.window_id = String::new();
    assert_eq!(
        a.validate(),
        Err(ArgError {
            field: "request_id"
        })
    );
    a.request_id = "r".into();
    a.station = Some(String::new());
    assert_eq!(a.validate(), Err(ArgError { field: "station" }));
    a.station = None;
    assert_eq!(a.validate(), Err(ArgError { field: "window_id" }));
    a.window_id = WINDOW.into();
    assert_eq!(a.validate(), Err(ArgError { field: "chord" }));
}
