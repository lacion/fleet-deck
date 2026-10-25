//! The grammar of the input actions (R13 P7 §4, §5.1, §5.2): the fixed argv of
//! every kind of chord, the refusals, and the plan of text and pointer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::desktop::DesktopAction as A;

use super::*;

const WINDOW: &str = "0123456789abcdef0123456789abcdef";

fn argv(chord: &str) -> Vec<String> {
    chord_argv(chord).unwrap()
}

fn strs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn key(chord: &str) -> A {
    A::InputKeyPress {
        window_id: WINDOW.into(),
        chord: chord.into(),
    }
}

fn text(text: &str) -> A {
    A::InputTypeText {
        window_id: WINDOW.into(),
        text: text.into(),
    }
}

#[test]
fn a_plain_key_is_one_k_argument() {
    assert_eq!(argv("a"), strs(&["-k", "a"]));
    assert_eq!(argv("7"), strs(&["-k", "7"]));
    assert_eq!(argv("Return"), strs(&["-k", "Return"]));
    assert_eq!(argv("BackSpace"), strs(&["-k", "BackSpace"]));
}

#[test]
fn space_and_the_page_keys_have_their_xkb_names() {
    assert_eq!(argv("Space"), strs(&["-k", "space"]));
    assert_eq!(argv("PageUp"), strs(&["-k", "Page_Up"]));
    assert_eq!(argv("PageDown"), strs(&["-k", "Page_Down"]));
    for named in [
        "Tab", "Escape", "Delete", "Insert", "Home", "End", "Left", "Right", "Up", "Down",
    ] {
        assert_eq!(argv(named), strs(&["-k", named]), "{named}");
    }
}

#[test]
fn modifiers_are_pressed_in_order_and_released_in_reverse() {
    assert_eq!(
        argv("Ctrl+a"),
        strs(&["-M", "ctrl", "-k", "a", "-m", "ctrl"])
    );
    assert_eq!(
        argv("Shift+Tab"),
        strs(&["-M", "shift", "-k", "Tab", "-m", "shift"])
    );
    assert_eq!(
        argv("Ctrl+Shift+a"),
        strs(&[
            "-M", "ctrl", "-M", "shift", "-k", "a", "-m", "shift", "-m", "ctrl"
        ])
    );
    assert_eq!(
        argv("Ctrl+Home"),
        strs(&["-M", "ctrl", "-k", "Home", "-m", "ctrl"])
    );
}

#[test]
fn every_chord_outside_the_grammar_is_refused() {
    for chord in [
        "CTRL+A",
        "Control+a",
        "Shift+Ctrl+a",
        "Ctrl+Ctrl+a",
        "Alt+Tab",
        "Super+Return",
        "F5",
        "Ctrl++",
        "a,b",
        "A",
        "ab",
        "",
        "Ctrl+",
        "Shift+",
        "Ctrl",
        "return",
        "-k a",
        "Ctrl+a ",
    ] {
        assert!(chord_argv(chord).is_none(), "{chord}");
        assert!(check(&key(chord)).is_err(), "{chord}");
    }
}

#[test]
fn a_bad_action_is_invalid_params_and_never_shows_the_value() {
    let e = check(&key("Alt+x")).unwrap_err();
    assert_eq!(e.code, "invalid_params");
    assert_eq!(e.message, "The desktop tool arguments are not valid.");
    assert!(!e.message.contains("Alt"));
    let bad_window = A::InputKeyPress {
        window_id: "xyz".into(),
        chord: "a".into(),
    };
    assert_eq!(check(&bad_window).unwrap_err().code, "invalid_params");
    let far = A::InputPointerMove {
        window_id: WINDOW.into(),
        x: 32_768,
        y: 0,
    };
    assert_eq!(check(&far).unwrap_err().code, "invalid_params");
    // An action that is not an input action passes.
    assert!(check(&A::ScreenLock).is_ok());
}

#[test]
fn the_text_keeps_every_byte_and_its_argv_is_a_dash() {
    for sample in [
        "plain",
        "-rf --help -- -",
        "  spaces and trailing  ",
        "quotes \"'`$HOME $(x)",
        "caf\u{e9} \u{65e5}\u{672c}\u{8a9e} e\u{301}",
        "\u{1f600}",
    ] {
        let (window, plan) = plan(&text(sample)).unwrap();
        assert_eq!(window, WINDOW);
        assert_eq!(
            plan,
            Plan::Text {
                bytes: sample.as_bytes().to_vec()
            }
        );
    }
    assert_eq!(text_argv(), strs(&["-"]));
}

#[test]
fn the_text_limits_and_forbidden_controls_are_refused() {
    let exact = "a".repeat(2000);
    assert!(check(&text(&exact)).is_ok());
    assert!(check(&text(&"a".repeat(2001))).is_err());
    assert!(check(&text("")).is_err());
    // 2,000 scalars of 4 bytes is 8,000 bytes: allowed. 2,001 is not.
    let wide = "\u{1f600}".repeat(2000);
    assert_eq!(wide.len(), 8000);
    assert!(check(&text(&wide)).is_ok());
    for bad in [
        "a\nb",
        "a\rb",
        "a\tb",
        "a\u{1b}b",
        "a\0b",
        "a\u{8}b",
        "a\u{7f}b",
        "a\u{85}b",
        "a\u{9f}b",
        "a\u{2028}b",
        "a\u{2029}b",
    ] {
        assert!(check(&text(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn a_pointer_plan_and_the_disclosure_texts() {
    let pointer = A::InputPointerMove {
        window_id: WINDOW.into(),
        x: 3,
        y: 4,
    };
    let (_, plan) = plan(&pointer).unwrap();
    assert_eq!(plan, Plan::Pointer { x: 3, y: 4 });
    assert_eq!(
        plan.disclosure("bold-fox", false),
        "Fleet Deck is moving the pointer for bold-fox in an agent preview."
    );
    assert_eq!(
        plan.disclosure("bold-fox", true),
        "Fleet Deck is moving the pointer for bold-fox in an application window."
    );
    let (_, keys) = super::plan(&key("a")).unwrap();
    assert_eq!(
        keys.disclosure("bold-fox", false),
        "Fleet Deck is sending keyboard input for bold-fox to an agent preview."
    );
    assert_eq!(
        keys.disclosure("bold-fox", true),
        "Fleet Deck is sending keyboard input for bold-fox to an application window."
    );
    let (_, typed) = super::plan(&text("hi")).unwrap();
    assert_eq!(
        typed.disclosure("bold-fox", true),
        keys.disclosure("bold-fox", true)
    );
    assert!(super::plan(&A::ScreenLock).is_err());
}

#[test]
fn the_any_variants_have_the_grammar_and_the_plan_of_the_legacy_ones() {
    let any_key = |chord: &str| A::InputAnyKeyPress {
        window_id: WINDOW.into(),
        chord: chord.into(),
    };
    let any_text = |t: &str| A::InputAnyTypeText {
        window_id: WINDOW.into(),
        text: t.into(),
    };
    let any_move = |x: u32| A::InputAnyPointerMove {
        window_id: WINDOW.into(),
        x,
        y: 4,
    };
    assert_eq!(
        super::plan(&any_key("Ctrl+a")).unwrap(),
        super::plan(&key("Ctrl+a")).unwrap()
    );
    assert_eq!(
        super::plan(&any_text("hi")).unwrap(),
        super::plan(&text("hi")).unwrap()
    );
    assert_eq!(
        super::plan(&any_move(3)).unwrap().1,
        Plan::Pointer { x: 3, y: 4 }
    );
    for bad in [any_key("Alt+Tab"), any_text(""), any_move(40_000)] {
        assert_eq!(check(&bad).unwrap_err().code, "invalid_params");
        assert!(super::plan(&bad).is_err());
    }
    // The classification of the eight input variants.
    let click = A::InputPointerClick {
        window_id: WINDOW.into(),
        x: 1,
        y: 1,
    };
    let any_click = A::InputAnyPointerClick {
        window_id: WINDOW.into(),
        x: 1,
        y: 1,
    };
    let all = [
        key("a"),
        text("a"),
        A::InputPointerMove {
            window_id: WINDOW.into(),
            x: 1,
            y: 1,
        },
        click.clone(),
        any_key("a"),
        any_text("a"),
        any_move(1),
        any_click.clone(),
    ];
    assert!(all.iter().all(is_input));
    assert!(!is_input(&A::ScreenLock));
    let any: Vec<bool> = all.iter().map(is_any).collect();
    assert_eq!(any, [false, false, false, false, true, true, true, true]);
    let clicks: Vec<bool> = all.iter().map(is_click).collect();
    assert_eq!(
        clicks,
        [false, false, false, true, false, false, false, true]
    );
    // A click has the grammar of a pointer movement and its own plan.
    assert_eq!(
        super::plan(&click).unwrap(),
        (WINDOW.to_owned(), Plan::Click { x: 1, y: 1 })
    );
    assert_eq!(
        super::plan(&any_click).unwrap(),
        super::plan(&click).unwrap()
    );
    for bad in [
        A::InputPointerClick {
            window_id: "x".into(),
            x: 1,
            y: 1,
        },
        A::InputPointerClick {
            window_id: WINDOW.into(),
            x: 32_768,
            y: 1,
        },
        A::InputAnyPointerClick {
            window_id: WINDOW.into(),
            x: 1,
            y: 40_000,
        },
    ] {
        assert_eq!(check(&bad).unwrap_err().code, "invalid_params");
        assert!(super::plan(&bad).is_err());
    }
    // The fixed disclosure texts of a click.
    let (_, plan) = super::plan(&click).unwrap();
    assert_eq!(
        plan.disclosure("bold-fox", false),
        "Fleet Deck is clicking for bold-fox in an agent preview."
    );
    assert_eq!(
        plan.disclosure("bold-fox", true),
        "Fleet Deck is clicking for bold-fox in an application window."
    );
}
