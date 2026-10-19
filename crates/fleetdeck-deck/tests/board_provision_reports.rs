//! The setup report routes of the board against a real deck node (BG-3,
//! `docs/v2/design/board-gaps.md` section 4.4): owner only, the reporting
//! window with elevation, the list, the retry permission with its binding and
//! its single use, the dismissal, and audit rows without the reported name.
//! The deck listener is a loopback socket that has the rules of a tailnet
//! listener.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod pairing_support;
mod settings_support;

use std::collections::BTreeSet;

use common::board::{BoardClient, Http};
use fleetdeck_core::identity::{Fingerprint, Identity};
use fleetdeck_deck::elevation::ELEVATION_TTL_MS;
use fleetdeck_deck::http::artifact_delete::ELEVATION_HEADER;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::auth::secret_hash;
use fleetdeck_link::hello::ReportStatus;
use fleetdeck_link::node::ListenerKind;
use fleetdeck_link::{ReportSink, ValidReport, send_report};
use fleetdeck_proto::link::Scope;
use pairing_support::{Deck, deck, station_config};
use serde_json::{Value, json};
use settings_support::{Headers, elevate, expect, get, owner, post, refs};

const MIN: i64 = 60 * 1000;
const NAME: &str = "desk-west-secret-name";

fn terminal(c: char) -> String {
    c.to_string().repeat(32)
}

fn digest(c: char) -> String {
    c.to_string().repeat(64)
}

fn report(t: char, code: &str) -> ValidReport {
    ValidReport::validate(&terminal(t), &digest(t), code, "link", NAME).unwrap()
}

/// A DELETE with the elevation header (or without it).
async fn delete_with(c: &BoardClient, h: &Headers, path: &str, elevation: Option<&str>) -> Http {
    let mut headers = refs(h);
    if let Some(token) = elevation {
        headers.push((ELEVATION_HEADER, token));
    }
    c.req("DELETE", path, &headers, None).await
}

/// A DELETE with a fresh elevation of the login `h`.
async fn delete(c: &BoardClient, h: &Headers, path: &str) -> Http {
    let elevation = elevate(c, h).await;
    delete_with(c, h, path, Some(&elevation)).await
}

/// An elevation of the login `h` that is already expired.
fn expired_elevation(c: &BoardClient, h: &Headers) -> String {
    let (_, value) = h[0].1.split_once('=').unwrap();
    let session_hash = secret_hash(value);
    let now = c.store.now_ms();
    let (token, expires) = c
        .state
        .elevations
        .issue(&session_hash, now - ELEVATION_TTL_MS - 1)
        .unwrap();
    assert!(expires <= now);
    token.expose().clone()
}

fn key() -> Fingerprint {
    Identity::generate().unwrap().fingerprint()
}

/// Opens the reporting window through the route.
async fn open_reports(c: &BoardClient, h: &Headers) -> Value {
    let elevation = elevate(c, h).await;
    let r = post(
        c,
        h,
        "/api/provision-reports/window",
        json!({ "open": true, "elevation": elevation }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

/// A report from `fp`, as the link would hand it to the sink.
async fn receive(d: &Deck, fp: Fingerprint, r: ValidReport, after_ms: i64) -> ReportStatus {
    let now = d.board.store.now_ms() + after_ms;
    d.node.provision_reports().report(fp, r, now).await.status
}

fn retry_body(t: char, request: &str, elevation: &str) -> Value {
    json!({
        "terminal_id": terminal(t),
        "state_digest": digest(t),
        "request_id": request,
        "elevation": elevation,
    })
}

#[tokio::test]
async fn provision_report_routes_are_owner_only_and_need_a_login_and_csrf() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    let fp = key().to_string();
    let routes = [
        ("GET", "/api/provision-reports".to_owned()),
        ("POST", "/api/provision-reports/window".to_owned()),
        ("POST", format!("/api/provision-reports/{fp}/retry")),
        ("DELETE", format!("/api/provision-reports/{fp}/retry")),
        ("DELETE", format!("/api/provision-reports/{fp}")),
    ];
    let (_, t) = c
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", t.expose());
    for (method, path) in &routes {
        let body = (*method == "POST").then(|| json!({ "open": false }));
        let r = c
            .req(
                method,
                path,
                &[("Authorization", auth.as_str())],
                body.clone(),
            )
            .await;
        expect(&r, 403, "owner_only");
        let r = c.req(method, path, &[], body).await;
        assert_eq!(r.status, 401, "{method} {path}");
    }
    // The owner without the CSRF header: refused.
    let cookie = h[0].1.clone();
    let r = c
        .req(
            "DELETE",
            &format!("/api/provision-reports/{fp}"),
            &[("Cookie", cookie.as_str())],
            None,
        )
        .await;
    expect(&r, 403, "csrf");
}

#[tokio::test]
async fn provision_report_window_needs_elevation_to_open_and_never_opens_pairing() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    let r = get(c, &h, "/api/provision-reports").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body,
        json!({ "window": { "open": false, "deadline_ms": null }, "reports": [] })
    );
    let path = "/api/provision-reports/window";
    expect(
        &post(c, &h, path, json!({ "open": true })).await,
        400,
        "elevation_required",
    );
    expect(
        &post(c, &h, path, json!({ "open": true, "elevation": "nope" })).await,
        400,
        "elevation_required",
    );
    expect(
        &post(c, &h, path, json!({ "open": false, "x": 1 })).await,
        400,
        "invalid_request",
    );
    expect(
        &post(c, &h, path, json!({ "open": 3 })).await,
        400,
        "invalid_request",
    );
    assert!(!d.node.provision_reports().window().open);

    let now = d.board.store.now_ms();
    let w = open_reports(c, &h).await;
    assert_eq!(w["open"], true);
    assert_eq!(w["deadline_ms"].as_i64().unwrap(), now + 60 * MIN);
    // The pairing window stays closed.
    assert_eq!(get(c, &h, "/api/pairing/window").await.body["open"], false);
    assert_eq!(
        get(c, &h, "/api/provision-reports").await.body["window"]["open"],
        true
    );
    // Closing needs the elevation too: the window stays open without it.
    let elevation = elevate(c, &h).await;
    let r = post(
        c,
        &h,
        path,
        json!({ "open": false, "elevation": elevation }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "open": false, "deadline_ms": null }));
    let audited = |result| c.audited("provision_reports.window", result);
    assert_eq!(
        audited(AuditResult::Ok).await,
        vec![Some("close".to_owned()), Some("open".to_owned())]
    );
    assert_eq!(audited(AuditResult::Failed).await.len(), 2);
}

#[tokio::test]
async fn provision_report_close_cancel_and_dismiss_need_the_owners_elevation() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_reports(c, &h).await;
    let fp = key();
    receive(&d, fp, report('a', "denied"), 0).await;
    let granted = elevate(c, &h).await;
    let retry_path = format!("/api/provision-reports/{fp}/retry");
    let report_path = format!("/api/provision-reports/{fp}");
    let window_path = "/api/provision-reports/window";
    let queued = post(c, &h, &retry_path, retry_body('a', "req-1", &granted)).await;
    assert_eq!(queued.status, 200, "{}", queued.body);

    // The elevation of another login, an expired one, a wrong one and none.
    let other = owner(c).await;
    let foreign = elevate(c, &other).await;
    let expired = expired_elevation(c, &h);
    for (what, token) in [
        ("none", None),
        ("wrong", Some("nope".to_owned())),
        ("expired", Some(expired)),
        ("another login", Some(foreign)),
    ] {
        let token = token.as_deref();
        let mut close = json!({ "open": false });
        if let Some(t) = token {
            close["elevation"] = json!(t);
        }
        expect(
            &post(c, &h, window_path, close).await,
            400,
            "elevation_required",
        );
        expect(
            &delete_with(c, &h, &retry_path, token).await,
            400,
            "elevation_required",
        );
        expect(
            &delete_with(c, &h, &report_path, token).await,
            400,
            "elevation_required",
        );
        // Nothing changed: the window is open, the retry is queued, the
        // report is still listed.
        let list = get(c, &h, "/api/provision-reports").await;
        assert_eq!(list.body["window"]["open"], true, "{what}");
        let rows = list.body["reports"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{what}");
        assert_eq!(rows[0]["retry"]["state"], "queued", "{what}");
        assert!(d.node.provision_reports().window().open, "{what}");
    }

    // With a valid elevation each one works.
    let r = post(
        c,
        &h,
        window_path,
        json!({ "open": false, "elevation": granted }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "open": false, "deadline_ms": null }));
    let r = delete_with(c, &h, &retry_path, Some(&granted)).await;
    assert_eq!(r.status, 204, "{}", r.text);
    let r = delete_with(c, &h, &report_path, Some(&granted)).await;
    assert_eq!(r.status, 204, "{}", r.text);
    let list = get(c, &h, "/api/provision-reports").await;
    assert!(list.body["reports"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn provision_report_window_does_not_open_without_a_tailnet_listener() {
    let d = deck(None).await;
    let c = &d.board;
    let h = owner(c).await;
    let elevation = elevate(c, &h).await;
    let r = post(
        c,
        &h,
        "/api/provision-reports/window",
        json!({ "open": true, "elevation": elevation }),
    )
    .await;
    expect(&r, 409, "no_tailnet_listener");
    assert!(!d.node.provision_reports().window().open);
}

#[tokio::test]
async fn provision_report_a_station_key_reports_over_the_link_and_the_owner_lists_it() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    let station = station_config();
    let limit = std::time::Duration::from_secs(10);
    let r = report('a', "denied");
    // The window is closed: TLS refuses the unpaired key.
    let closed = send_report(&station, d.listen, d.identity.fingerprint(), &r, limit).await;
    assert_eq!(closed.unwrap_err().reason(), Some("not_accepted"));

    open_reports(c, &h).await;
    let reply = send_report(&station, d.listen, d.identity.fingerprint(), &r, limit)
        .await
        .unwrap();
    assert_eq!(
        (reply.status, reply.retry_after_seconds),
        (ReportStatus::Stored, 600)
    );
    let list = get(c, &h, "/api/provision-reports").await;
    assert_eq!(list.status, 200, "{}", list.body);
    let rows = list.body["reports"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        row["fingerprint"],
        station.identity.fingerprint().to_string()
    );
    assert_eq!(row["name"], NAME);
    assert_eq!(
        (row["code"].as_str(), row["phase"].as_str()),
        (Some("denied"), Some("link"))
    );
    assert_eq!(row["terminal_id"], terminal('a'));
    assert_eq!(row["state_digest"], digest('a'));
    assert!(row["received_at"].as_i64().unwrap() > 0);
    assert!(row["last_contact"].as_i64().unwrap() > 0);
    assert!(row["retry"].is_null());
    // A report creates no station, grant or pairing window.
    assert!(c.store.stations().await.unwrap().is_empty());
    assert_eq!(get(c, &h, "/api/pairing/window").await.body["open"], false);
}

#[tokio::test]
async fn provision_report_list_is_newest_first() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_reports(c, &h).await;
    let (first, second) = (key(), key());
    assert_eq!(
        receive(&d, first, report('a', "denied"), 0).await,
        ReportStatus::Stored
    );
    assert_eq!(
        receive(&d, second, report('b', "cancelled"), 1000).await,
        ReportStatus::Stored
    );
    let list = get(c, &h, "/api/provision-reports").await;
    let rows = list.body["reports"].as_array().unwrap();
    assert_eq!(rows[0]["fingerprint"], second.to_string());
    assert_eq!(rows[1]["fingerprint"], first.to_string());
}

#[tokio::test]
async fn provision_report_retry_needs_elevation_the_exact_binding_and_a_code_with_a_repair() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_reports(c, &h).await;
    let (ok_key, disabled_key, unknown) = (key(), key(), key());
    receive(&d, ok_key, report('a', "denied"), 0).await;
    receive(&d, disabled_key, report('b', "service_conflict"), 0).await;
    let elevation = elevate(c, &h).await;
    let path = |k: &Fingerprint| format!("/api/provision-reports/{k}/retry");

    // No elevation.
    let mut no_elevation = retry_body('a', "req-1", &elevation);
    no_elevation.as_object_mut().unwrap().remove("elevation");
    expect(
        &post(c, &h, &path(&ok_key), no_elevation).await,
        400,
        "elevation_required",
    );
    // No report for the key.
    expect(
        &post(c, &h, &path(&unknown), retry_body('a', "req-1", &elevation)).await,
        404,
        "report_not_found",
    );
    // A stale binding.
    let r = post(c, &h, &path(&ok_key), retry_body('c', "req-1", &elevation)).await;
    expect(&r, 409, "report_changed");
    assert_eq!(
        r.body["message"],
        "The reported setup condition changed. Review the latest report."
    );
    // A disabled code.
    let r = post(
        c,
        &h,
        &path(&disabled_key),
        retry_body('b', "req-1", &elevation),
    )
    .await;
    expect(&r, 409, "retry_not_allowed");
    assert_eq!(
        r.body["message"],
        "Retrying cannot repair this condition. Fleet Deck has no board repair for it yet."
    );
    // Forms.
    for bad in [
        json!({ "terminal_id": "x", "state_digest": digest('a'), "request_id": "r" }),
        json!({ "terminal_id": terminal('a'), "state_digest": "x", "request_id": "r" }),
        json!({ "terminal_id": terminal('a'), "state_digest": digest('a'), "request_id": "" }),
        json!({ "terminal_id": terminal('a'), "state_digest": digest('a'), "request_id": "a b" }),
        json!({ "terminal_id": terminal('a'), "state_digest": digest('a'), "request_id": "a".repeat(65) }),
        json!({ "terminal_id": terminal('a'), "state_digest": digest('a') }),
        json!({ "terminal_id": terminal('a'), "state_digest": digest('a'), "request_id": "r", "x": 1 }),
    ] {
        expect(
            &post(c, &h, &path(&ok_key), bad).await,
            400,
            "invalid_request",
        );
    }
    expect(
        &post(
            c,
            &h,
            "/api/provision-reports/ABC/retry",
            retry_body('a', "r", &elevation),
        )
        .await,
        400,
        "invalid_request",
    );
    // Nothing was queued by the refusals.
    let list = get(c, &h, "/api/provision-reports").await;
    assert!(
        list.body["reports"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["retry"].is_null())
    );
}

#[tokio::test]
async fn provision_report_one_request_id_makes_one_permission_and_it_is_delivered_once() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_reports(c, &h).await;
    let fp = key();
    receive(&d, fp, report('a', "pairing_failed"), 0).await;
    let elevation = elevate(c, &h).await;
    let path = format!("/api/provision-reports/{fp}/retry");

    let first = post(c, &h, &path, retry_body('a', "req-1", &elevation)).await;
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(first.body["retry"]["state"], "queued");
    let expires = first.body["retry"]["expires_at"].as_i64().unwrap();
    assert_eq!(expires, d.board.store.now_ms() + 20 * MIN);
    // The same request id again: the same permission.
    let again = post(c, &h, &path, retry_body('a', "req-1", &elevation)).await;
    assert_eq!(again.body, first.body);
    // Another request id: refused while the permission is live.
    let other = post(c, &h, &path, retry_body('a', "req-2", &elevation)).await;
    expect(&other, 409, "retry_queued");
    assert_eq!(
        other.body["message"],
        "A retry is already allowed for this key."
    );
    let list = get(c, &h, "/api/provision-reports").await;
    assert_eq!(list.body["reports"][0]["retry"]["state"], "queued");

    // The station reports its exact binding: it gets the permission once.
    let reply = d
        .node
        .provision_reports()
        .report(
            fp,
            report('a', "pairing_failed"),
            d.board.store.now_ms() + MIN,
        )
        .await;
    assert_eq!(
        (reply.status, reply.retry_after_seconds),
        (ReportStatus::Retry, 0)
    );
    assert_eq!(reply.terminal_id, Some(terminal('a')));
    assert_eq!(reply.state_digest, Some(digest('a')));
    assert_eq!(reply.retry_id.as_deref().map(str::len), Some(32));
    let list = get(c, &h, "/api/provision-reports").await;
    assert_eq!(list.body["reports"][0]["retry"]["state"], "delivered");
    // The repeated click now shows the real state; another id is refused.
    let again = post(c, &h, &path, retry_body('a', "req-1", &elevation)).await;
    assert_eq!(again.body["retry"]["state"], "delivered");
    expect(
        &post(c, &h, &path, retry_body('a', "req-3", &elevation)).await,
        409,
        "retry_queued",
    );
    // A delivered permission cannot be cancelled.
    expect(&delete(c, &h, &path).await, 409, "retry_delivered");
    // It is delivered once.
    let reply = d
        .node
        .provision_reports()
        .report(
            fp,
            report('a', "pairing_failed"),
            d.board.store.now_ms() + 7 * MIN,
        )
        .await;
    assert_eq!(reply.status, ReportStatus::Stored);
}

#[tokio::test]
async fn provision_report_a_queued_retry_is_cancelled_and_a_missing_one_is_not_found() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_reports(c, &h).await;
    let fp = key();
    let path = format!("/api/provision-reports/{fp}/retry");
    expect(&delete(c, &h, &path).await, 404, "report_not_found");
    receive(&d, fp, report('a', "revoked"), 0).await;
    let r = delete(c, &h, &path).await;
    expect(&r, 404, "retry_not_found");
    assert_eq!(r.body["message"], "No retry is waiting for this key.");
    let elevation = elevate(c, &h).await;
    post(c, &h, &path, retry_body('a', "req-1", &elevation)).await;
    let r = delete(c, &h, &path).await;
    assert_eq!(r.status, 204, "{}", r.text);
    let list = get(c, &h, "/api/provision-reports").await;
    assert!(list.body["reports"][0]["retry"].is_null());
    // The station gets no retry after the cancel.
    let reply = d
        .node
        .provision_reports()
        .report(fp, report('a', "revoked"), d.board.store.now_ms() + MIN)
        .await;
    assert_eq!(reply.status, ReportStatus::RateLimited);
    expect(&delete(c, &h, &path).await, 404, "retry_not_found");
    expect(
        &delete(c, &h, "/api/provision-reports/zz/retry").await,
        400,
        "invalid_request",
    );
}

#[tokio::test]
async fn provision_report_dismiss_removes_the_report_and_its_permission() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_reports(c, &h).await;
    let fp = key();
    let path = format!("/api/provision-reports/{fp}");
    expect(&delete(c, &h, &path).await, 404, "report_not_found");
    receive(&d, fp, report('a', "denied"), 0).await;
    let elevation = elevate(c, &h).await;
    post(
        c,
        &h,
        &format!("{path}/retry"),
        retry_body('a', "req-1", &elevation),
    )
    .await;
    let r = delete(c, &h, &path).await;
    assert_eq!(r.status, 204, "{}", r.text);
    let list = get(c, &h, "/api/provision-reports").await;
    assert!(list.body["reports"].as_array().unwrap().is_empty());
    expect(
        &delete(c, &h, "/api/provision-reports/XYZ").await,
        400,
        "invalid_request",
    );
    // A new report of the key starts without a permission.
    receive(&d, fp, report('a', "denied"), 0).await;
    let list = get(c, &h, "/api/provision-reports").await;
    assert!(list.body["reports"][0]["retry"].is_null());
}

#[tokio::test]
async fn provision_report_audit_rows_hold_the_fingerprint_prefix_and_never_the_name() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_reports(c, &h).await;
    let fp = key();
    receive(&d, fp, report('a', "denied"), 0).await;
    // An incoming report writes no audit row.
    let before = c.store.audit_log(100).await.unwrap().len();
    receive(&d, key(), report('b', "denied"), 0).await;
    assert_eq!(c.store.audit_log(100).await.unwrap().len(), before);

    let elevation = elevate(c, &h).await;
    let path = format!("/api/provision-reports/{fp}");
    post(
        c,
        &h,
        &format!("{path}/retry"),
        retry_body('c', "req-1", &elevation),
    )
    .await;
    post(
        c,
        &h,
        &format!("{path}/retry"),
        retry_body('a', "req-1", &elevation),
    )
    .await;
    delete(c, &h, &format!("{path}/retry")).await;
    delete(c, &h, &path).await;

    let rows = c.store.audit_log(100).await.unwrap();
    let ours: Vec<_> = rows
        .iter()
        .filter(|e| e.action.starts_with("provision_reports."))
        .collect();
    assert!(!ours.is_empty());
    for e in &ours {
        let text = format!("{e:?}");
        assert!(!text.contains(NAME), "{text}");
        assert!(!text.contains(&fp.to_string()), "{text}");
    }
    let detail = |action: &str, result: AuditResult| -> Vec<Option<String>> {
        ours.iter()
            .filter(|e| e.action == action && e.result == result)
            .map(|e| e.detail.clone())
            .collect()
    };
    assert_eq!(
        detail("provision_reports.retry", AuditResult::Failed),
        vec![Some("report_changed: allowed".to_owned())]
    );
    assert_eq!(
        detail("provision_reports.retry", AuditResult::Ok),
        vec![Some("cancelled".to_owned()), Some("allowed".to_owned())]
    );
    assert_eq!(
        detail("provision_reports.dismiss", AuditResult::Ok),
        vec![Some("dismissed".to_owned())]
    );
    let targets: Vec<_> = ours
        .iter()
        .filter(|e| e.action != "provision_reports.window")
        .map(|e| e.target.clone())
        .collect();
    assert!(
        targets
            .iter()
            .all(|t| t.as_deref() == Some(fp.short().as_str()))
    );
}
