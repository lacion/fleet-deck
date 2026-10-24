use super::*;

fn files(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn the_inert_csp_has_no_allow_scripts_and_the_exact_text() {
    assert_eq!(
        INERT_CSP,
        "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src data:; font-src data:"
    );
    assert!(!INERT_CSP.contains("allow-"));
}

#[test]
fn every_kind_gets_the_inert_headers() {
    for path in ["a.html", "a.md", "a.png", "a.svg", "a.pdf"] {
        let h = display_headers(path);
        let get = |name: header::HeaderName| h.get(name).unwrap().to_str().unwrap().to_owned();
        assert_eq!(get(header::CONTENT_SECURITY_POLICY), INERT_CSP, "{path}");
        assert_eq!(get(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
        assert_eq!(get(header::REFERRER_POLICY), "no-referrer");
        assert_eq!(get(header::CACHE_CONTROL), "no-store");
        assert_eq!(get(header::CONTENT_TYPE), content_type_of(path));
        assert!(h.get(header::CONTENT_DISPOSITION).is_none());
        assert_eq!(h.len(), 5);
    }
}

#[test]
fn one_file_of_a_showable_kind_is_displayable() {
    for name in ["a.html", "a.htm", "a.md", "a.png", "a.svg", "a.pdf"] {
        assert!(displayable(&files(&[name])), "{name}");
    }
    for name in ["a.txt", "a.mp4", "a.json", "noext"] {
        assert!(!displayable(&files(&[name])), "{name}");
    }
    assert!(!displayable(&files(&[])));
    assert!(!displayable(&files(&["a.html", "img/a.png"])));
}

#[test]
fn a_station_error_becomes_a_fixed_answer_with_no_station_text() {
    let leak = "SENTINEL station alpha /srv/secret";
    for (code, status) in [
        ("unknown_artifact", 404),
        ("not_found", 404),
        ("unknown_station", 404),
        ("invalid_path", 404),
        ("station_unreachable", 409),
        ("timeout", 504),
        ("station_error", 502),
        ("unsupported", 502),
        ("outside_roots", 502),
    ] {
        let e = failure(ApiError::new(StatusCode::BAD_GATEWAY, code, leak));
        assert_eq!(e.status.as_u16(), status, "{code}");
        assert!(!e.message.contains("SENTINEL"), "{code}: {}", e.message);
        assert!(!e.message.contains("alpha"), "{code}: {}", e.message);
    }
    let internal = failure(ApiError::internal("disk"));
    assert_eq!(internal.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(failure(miss()).code, "unknown_artifact");
}

fn authority(generation: u64, app_flag: bool, allow_apps: bool, stations: &[&str]) -> Authority {
    Authority {
        generation,
        app_flag,
        allow_apps,
        scope: EffectiveScope {
            stations: stations.iter().map(|s| StationId::new(*s)).collect(),
            repo: None,
        },
    }
}

/// B17 §3.4.2: an app answer chose its policy on one directive generation.
/// The owner replaces `app: true` with `app: false` for the same artifact,
/// or turns `allow_apps` off and sends the same directive again: each moves
/// the generation, and the last check or the next chunk ends the answer.
#[test]
fn an_app_answer_needs_the_generation_and_the_permission_that_chose_it() {
    let decided = authority(7, true, true, &["s"]);
    assert!(decided.app());
    assert!(decided.holds(&authority(7, true, true, &["s"]), true));
    // The same artifact again with `app: false`: a new generation.
    assert!(!decided.holds(&authority(8, false, true, &["s"]), true));
    // `allow_apps` off, then the same directive again: a new generation
    // with the flag on and the setting off.
    assert!(!decided.holds(&authority(9, true, false, &["s"]), true));
    // The same directive with the flag or the setting off (a read of the
    // setting that does not match the generation) is refused too.
    assert!(!decided.holds(&authority(7, true, false, &["s"]), true));
    assert!(!decided.holds(&authority(7, false, true, &["s"]), true));
    // An inert answer needs the generation only.
    let inert = authority(7, false, false, &["s"]);
    assert!(inert.holds(&authority(7, false, false, &["s"]), false));
    assert!(!inert.holds(&authority(8, false, false, &["s"]), false));
}

/// B17 §3.4.3: the feed filters with the scope of its last check, and that
/// check must see the generation of the first one. The owner sends the same
/// app artifact again with a narrower station filter: a new generation, so
/// the cards of the wider scope never leave.
#[test]
fn a_feed_is_bound_to_the_scope_of_its_generation() {
    let first = authority(3, true, true, &["s", "far"]);
    let narrowed = authority(4, true, true, &["s"]);
    assert!(!first.holds(&narrowed, true));
    let same = authority(3, true, true, &["s", "far"]);
    assert!(first.holds(&same, true));
    assert_eq!(same.scope, first.scope);
}
