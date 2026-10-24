//! The pure tests of the MCP App routes: the token map, the rate limit, the
//! CSP builder, the bootstrap, the audit rows, the error table and the method
//! rows. The tests through HTTP are in `tests/board_apps.rs` and
//! `tests/board_apps_more.rs`.

use axum::http::{Method, StatusCode};
use fleetdeck_proto::StationId;
use fleetdeck_proto::apps::AppCsp;
use fleetdeck_proto::link::Scope;

use super::*;
use crate::http::app_csp;
use crate::http::app_public::{
    APP_BOOTSTRAP, AppTokens, CALLS_PER_WINDOW, MAX_VIEW_TOKENS, ViewEntry, token_key,
    with_bootstrap,
};
use crate::http::methods;

const ULID: &str = "01HZX3K5M7Q9R2S4T6V8W0Y1Z3";

fn entry(n: usize, expires_at: i64) -> ViewEntry {
    ViewEntry {
        station_id: StationId::from("st"),
        session_id: "s1".to_owned(),
        app_id: format!("app{n}"),
        csp: AppCsp::default(),
        doc_sha256: "00".repeat(32),
        login_session_hash: "login".to_owned(),
        expires_at,
    }
}

fn csp(connect: &[&str], resource: &[&str]) -> AppCsp {
    AppCsp {
        connect_domains: connect.iter().map(|s| (*s).to_owned()).collect(),
        resource_domains: resource.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn token_map_evicts_oldest_over_512() {
    let tokens = AppTokens::default();
    for n in 0..=MAX_VIEW_TOKENS {
        tokens.insert(format!("k{n}"), entry(n, 1_000_000), 0);
    }
    assert_eq!(tokens.view_count(), MAX_VIEW_TOKENS);
    assert!(tokens.lookup("k0", 1).is_none(), "the oldest entry went");
    assert!(tokens.lookup("k1", 1).is_some());
    assert!(tokens.lookup(&format!("k{MAX_VIEW_TOKENS}"), 1).is_some());
}

#[test]
fn token_map_forgets_an_expired_entry() {
    let tokens = AppTokens::default();
    tokens.insert("k".to_owned(), entry(1, 1000), 0);
    assert!(tokens.lookup("k", 999).is_some());
    assert!(tokens.lookup("k", 1000).is_none());
    assert_eq!(tokens.view_count(), 0);
    assert_ne!(token_key("a"), token_key("b"));
}

#[test]
fn app_call_rate_limit_is_60_per_minute_per_login() {
    let tokens = AppTokens::default();
    for _ in 0..CALLS_PER_WINDOW {
        assert!(tokens.allow_call("a", 1000));
    }
    assert!(!tokens.allow_call("a", 1000));
    assert!(
        tokens.allow_call("b", 1000),
        "another login has its own count"
    );
    assert!(!tokens.allow_call("a", 60_999));
    assert!(tokens.allow_call("a", 61_000), "the window moved on");
}

#[test]
fn app_csp_header_for_empty_domains() {
    assert_eq!(
        app_csp::header(&AppCsp::default()),
        "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; \
         style-src 'unsafe-inline'; img-src data: blob:; font-src data:; \
         media-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'self'"
    );
}

#[test]
fn app_csp_header_for_domains_has_no_self_in_a_fetch_directive() {
    let header = app_csp::header(&csp(
        &["https://api.example.com", "wss://live.example.com:8443"],
        &["https://cdn.example.com", "https://*.fonts.example.com"],
    ));
    assert_eq!(
        header,
        "sandbox allow-scripts; default-src 'none'; \
         script-src 'unsafe-inline' https://cdn.example.com https://*.fonts.example.com; \
         style-src 'unsafe-inline' https://cdn.example.com https://*.fonts.example.com; \
         img-src data: blob: https://cdn.example.com https://*.fonts.example.com; \
         font-src data: https://cdn.example.com https://*.fonts.example.com; \
         media-src data: blob: https://cdn.example.com https://*.fonts.example.com; \
         connect-src https://api.example.com wss://live.example.com:8443; \
         frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; \
         frame-ancestors 'self'"
    );
    for directive in header.split("; ") {
        if directive.starts_with("frame-ancestors") {
            continue;
        }
        assert!(!directive.contains("'self'"), "{directive}");
    }
}

#[test]
fn app_csp_sanitize_keeps_only_origins() {
    let dirty = csp(
        &[
            "https://ok.example.com",
            "https://ok.example.com",
            "javascript:alert(1)",
            "https://a.com/path",
            "https://a.com; script-src *",
            "HTTP://UPPER.EXAMPLE.COM:80",
            "https://host:123456",
            "https://*.wild.example.com",
            "",
        ],
        &["ws://x.io", "ftp://x.io"],
    );
    let clean = app_csp::sanitize(&dirty);
    assert_eq!(
        clean.connect_domains,
        [
            "https://ok.example.com",
            "HTTP://UPPER.EXAMPLE.COM:80",
            "https://*.wild.example.com"
        ]
    );
    assert_eq!(clean.resource_domains, ["ws://x.io"]);
    let many: Vec<String> = (0..40)
        .map(|n| format!("https://h{n}.example.com"))
        .collect();
    let capped = app_csp::sanitize(&AppCsp {
        connect_domains: many,
        resource_domains: vec![format!("https://{}.com", "a".repeat(260))],
    });
    assert_eq!(capped.connect_domains.len(), 32);
    assert!(capped.resource_domains.is_empty());
}

#[test]
fn bootstrap_follows_doctype_and_is_first() {
    let doc = "<!doctype html><html><head><script>var a=1</script></head></html>";
    let out = with_bootstrap(doc);
    assert!(out.starts_with(&format!("<!doctype html>{APP_BOOTSTRAP}<html>")));
    assert_eq!(out.find("<script>"), Some("<!doctype html>".len()));
    assert!(out.ends_with("var a=1</script></head></html>"));

    let upper = with_bootstrap("<!DOCTYPE HTML PUBLIC \"x\">\n<p>hi</p>");
    assert!(upper.starts_with(&format!("<!DOCTYPE HTML PUBLIC \"x\">{APP_BOOTSTRAP}\n<p>")));

    let bom = with_bootstrap("\u{feff}\n <!doctype html><p>x</p>");
    assert!(bom.starts_with(&format!(
        "\u{feff}\n <!doctype html>{APP_BOOTSTRAP}<p>x</p>"
    )));

    let none = with_bootstrap("<p>no doctype</p><script>1</script>");
    assert!(none.starts_with(APP_BOOTSTRAP));
    assert!(none.ends_with("<p>no doctype</p><script>1</script>"));

    assert_eq!(with_bootstrap(""), APP_BOOTSTRAP);
    // An unfinished doctype goes in front.
    assert!(with_bootstrap("<!doctype html").starts_with(APP_BOOTSTRAP));
}

#[test]
fn bootstrap_constant_is_the_design_source() {
    assert!(APP_BOOTSTRAP.starts_with("<script>(()=>{\"use strict\";\n"));
    assert!(APP_BOOTSTRAP.ends_with(
        "host.postMessage({\"fleet-deck-app-hello\":1},\"*\",[ch.port2]);\n})();</script>"
    ));
    assert_eq!(APP_BOOTSTRAP.matches("<script>").count(), 1);
    assert_eq!(APP_BOOTSTRAP.lines().count(), 12);
}

#[test]
fn app_audit_result_and_detail_follow_the_mapping() {
    for status in [400, 403, 404] {
        let s = StatusCode::from_u16(status).unwrap();
        assert_eq!(audit_result_of(s), AuditResult::Refused, "{status}");
    }
    for status in [409, 410, 413, 429, 502, 504] {
        let s = StatusCode::from_u16(status).unwrap();
        assert_eq!(audit_result_of(s), AuditResult::Failed, "{status}");
    }
    assert_eq!(call_detail(ULID, "bump", "ok"), format!("{ULID}/bump ok"));
    assert_eq!(call_detail("not-a-ulid", "bump", "busy"), "-/bump busy");
    let long = format!("a\u{7}b\n{}", "x".repeat(200));
    let detail = call_detail(ULID, &long, "confirm");
    let tool = detail
        .strip_prefix(&format!("{ULID}/"))
        .and_then(|d| d.strip_suffix(" confirm"))
        .unwrap();
    assert_eq!(tool.chars().count(), 128);
    assert!(tool.starts_with("abxxx"));
    assert_eq!(release_detail(ULID, "not_open"), format!("{ULID} not_open"));
    assert_eq!(release_detail("../etc", "ok"), "- invalid_params");
    assert!(is_ulid(ULID));
    assert!(!is_ulid("01HZX3K5M7Q9R2S4T6V8W0Y1Z"));
    assert!(!is_ulid("01HZX3K5M7Q9R2S4T6V8W0Y1ZI"));
    assert!(!is_ulid("01hzx3k5m7q9r2s4t6v8w0y1z3"));
}

#[test]
fn app_error_table_maps_each_code() {
    let cases = [
        ("unknown_session", 404),
        ("unknown_app", 404),
        ("unknown_call", 404),
        ("session_stopped", 409),
        ("call_running", 409),
        ("app_gone", 410),
        ("invalid_params", 400),
        ("tool_not_callable", 403),
        ("too_large", 413),
        ("busy", 429),
        ("provider_error", 502),
        ("timeout", 504),
    ];
    for (code, status) in cases {
        let e = map_station_error(code, "The station text.");
        assert_eq!((e.status.as_u16(), e.code), (status, code), "{code}");
        assert_eq!(e.message, "The station text.", "{code}");
    }
    // The text loses control characters and is cut to 1000 characters.
    let e = map_station_error("provider_error", &format!("a\nb\u{1b}{}", "z".repeat(2000)));
    assert_eq!(e.message.chars().count(), 1000);
    assert!(e.message.starts_with("abzz"));
    // Any other code gives a fixed text.
    for code in ["internal", "boom", "outside_roots", ""] {
        let e = map_station_error(code, "STATION-TEXT-MUST-NOT-LEAK");
        assert_eq!(
            (e.status, e.code),
            (StatusCode::BAD_GATEWAY, "station_error")
        );
        assert!(!e.message.contains("STATION-TEXT"), "{code}");
    }
}

#[test]
fn app_method_rows_are_owner_only() {
    let rows: Vec<_> = methods::api()
        .filter(|a| a.name.starts_with("sessions.app_"))
        .collect();
    assert_eq!(rows.len(), 4);
    let row = |name: &str| rows.iter().find(|a| a.name == name).copied().unwrap();
    for a in &rows {
        assert!(!a.member && !a.display, "{}", a.name);
    }
    let view = row("sessions.app_view");
    assert_eq!((view.scope, view.mutating), (Some(Scope::Read), false));
    assert_eq!(view.method, Method::GET);
    let calls = row("sessions.app_calls");
    assert_eq!((calls.scope, calls.mutating), (Some(Scope::Read), false));
    assert_eq!(calls.method, Method::GET);
    let call = row("sessions.app_call");
    assert_eq!((call.scope, call.mutating), (Some(Scope::Drive), true));
    assert_eq!(call.method, Method::POST);
    let release = row("sessions.app_release");
    assert_eq!(
        (release.scope, release.mutating),
        (Some(Scope::Drive), true)
    );
    assert_eq!(release.method, Method::POST);
    assert_eq!(
        methods::find(&Method::POST, call.path).map(|a| a.name),
        Some("sessions.app_call")
    );
}
