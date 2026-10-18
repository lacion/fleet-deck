//! The board-window barrier (R13 P7 §7.2): the classes, the titles, the deck
//! hosts as whole tokens, the wall record, and the host of a URL.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn hosts(list: &[&str]) -> Vec<String> {
    list.iter().map(|h| (*h).to_owned()).collect()
}

fn board(names: &Names<'_>, deck: &[&str]) -> bool {
    is_board(Kind::Preview, names, &hosts(deck))
}

fn class(class: &str) -> Names<'_> {
    Names {
        class,
        ..Names::default()
    }
}

fn title(title: &str) -> Names<'_> {
    Names {
        title,
        ..Names::default()
    }
}

#[test]
fn the_wall_and_board_classes_are_refused_with_their_suffix() {
    for c in [
        "fleetdeck-wall",
        "fleetdeck-wall-abc",
        "fleetdeck-wall-",
        "fleetdeck-board",
        "fleetdeck-board-1",
    ] {
        assert!(board(&class(c), &[]), "{c}");
        let initial = Names {
            initial_class: c,
            ..Names::default()
        };
        assert!(board(&initial, &[]), "initial {c}");
    }
    for c in [
        "fleetdeck-preview-abc",
        "fleetdeck-walls",
        "fleetdeck-wallpaper",
        "fleetdeck-boarding",
        "xfleetdeck-wall",
        "chromium",
        "",
    ] {
        assert!(!board(&class(c), &[]), "{c}");
    }
}

#[test]
fn a_wall_record_is_a_board_window_whatever_its_names() {
    assert!(is_board(Kind::Wall, &class("chromium"), &[]));
    assert!(!is_board(Kind::Preview, &class("chromium"), &[]));
}

#[test]
fn a_title_with_fleet_deck_or_fleetdeck_is_refused_without_case() {
    for t in [
        "Fleet Deck",
        "fleet deck - board",
        "FLEET DECK",
        "My FleetDeck",
        "fleetdeck",
        "x fLeEtDeCk y",
    ] {
        assert!(board(&title(t), &[]), "{t}");
        let initial = Names {
            initial_title: t,
            ..Names::default()
        };
        assert!(board(&initial, &[]), "initial {t}");
    }
    for t in [
        "Fleet  Deck",
        "Fleet-Deck",
        "Fleet",
        "deck",
        "Fleet\u{a0}Deck",
        "",
    ] {
        assert!(!board(&title(t), &[]), "{t}");
    }
}

#[test]
fn a_deck_host_is_matched_as_a_whole_token_only() {
    let deck = ["deck.example"];
    for t in [
        "deck.example",
        "Deck.Example",
        "Board at deck.example:8443",
        "https://deck.example/board",
        "(deck.example)",
        "deck.example.",
        "deck.example, home",
    ] {
        assert!(board(&title(t), &deck), "{t}");
    }
    for t in [
        "deck.example.evil",
        "deck.example.evil.org",
        "xdeck.example",
        "a.deck.example",
        "deck-example",
        "deck.examples",
        "deck_example",
        "",
    ] {
        assert!(!board(&title(t), &deck), "{t}");
    }
    // No known host: nothing to match.
    assert!(!board(&title("deck.example"), &[]));
    // A host in the initial title counts too.
    let initial = Names {
        initial_title: "deck.example",
        ..Names::default()
    };
    assert!(board(&initial, &deck));
}

#[test]
fn an_ipv6_deck_host_matches_in_brackets() {
    let deck = ["fd7a:115c::1"];
    assert!(board(&title("[fd7a:115c::1]:8443"), &deck));
    assert!(board(&title("Board [FD7A:115C::1]"), &deck));
    assert!(!board(&title("fd7a:115c::1"), &deck));
    assert!(!board(&title("[fd7a:115c::12]"), &deck));
}

#[test]
fn the_host_of_a_url_is_lower_case_without_port_or_user_info() {
    for (url, host) in [
        ("https://Deck.Example/path", Some("deck.example")),
        ("https://deck.example:8443", Some("deck.example")),
        ("http://user:pw@deck.example:1/x?y#z", Some("deck.example")),
        ("wss://[FD7A::1]:443/ws", Some("fd7a::1")),
        ("https://localhost", Some("localhost")),
        ("deck.example", None),
        ("https:///x", None),
    ] {
        assert_eq!(host_of_url(url).as_deref(), host, "{url}");
    }
}

#[test]
fn a_launch_url_on_a_known_deck_host_is_a_deck_origin() {
    let deck = hosts(&["deck.example", "fd7a::1"]);
    for url in [
        "https://deck.example/",
        "https://DECK.example:8443/board?x=1",
        "http://deck.example/any/route#frag",
        "https://[fd7a::1]:8443/",
    ] {
        assert!(is_deck_origin(url, &deck), "{url}");
    }
    for url in [
        "https://deck.example.evil/",
        "https://xdeck.example/",
        "http://localhost:3000/",
        "http://127.0.0.1/",
        "nothing",
    ] {
        assert!(!is_deck_origin(url, &deck), "{url}");
    }
    assert!(!is_deck_origin("https://deck.example/", &[]));
}

#[test]
fn a_marked_handle_stays_marked_and_the_oldest_mark_goes_first() {
    let marks = Marks::default();
    assert!(!marks.is_marked("a"));
    marks.mark("a");
    marks.mark("a");
    assert!(marks.is_marked("a") && !marks.is_marked("b"));
    for n in 0..MAX_MARKS {
        marks.mark(&format!("id{n}"));
    }
    // One more than the limit: the oldest, `a`, is gone.
    assert!(!marks.is_marked("a"));
    assert!(marks.is_marked("id0") && marks.is_marked(&format!("id{}", MAX_MARKS - 1)));
}
