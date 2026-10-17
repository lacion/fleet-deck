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
