//! The `page` directive, the display page route and the display document
//! policy over loopback HTTPS (B17 §3.2.4 to §3.2.6): a wall gets the origin
//! of the one page that it shows now, and nobody else gets it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::board::{BoardClient, Http, board_with_assets};
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::http::assets::BoardAssets;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::wall_pages::CheckedPage;
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::wall_pages::PageFit;
use serde_json::json;
use share_support::*;

const FEATURES: (&str, &str) = ("X-FleetDeck-Features", "wall_pages.v1");

struct Rig {
    _node: Arc<DeckNode>,
    _tmp: tempfile::TempDir,
    c: BoardClient,
    h: Headers,
    st: String,
}

/// A deck with board files (for `/display`), one station that holds `s1`
/// and the owner logged in.
async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let dist = tmp.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("index.html"), "<p>board</p>").unwrap();
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    let store = DeckStore::open(&tmp.path().join("deck.db"), clock, ids)
        .await
        .unwrap();
    let node = DeckNode::start(
        store,
        Arc::new(Identity::generate().unwrap()),
        &["127.0.0.1:0".parse().unwrap()],
        BackoffPolicy::default(),
    )
    .await
    .unwrap();
    let c = board_with_assets(Arc::clone(&node), BoardAssets::Dir(dist)).await;
    let rec = c
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    c.store
        .apply_snapshot(
            rec.station_id.clone(),
            IncarnationId::from("i1"),
            0,
            vec![card("s1")],
        )
        .await
        .unwrap();
    let h = owner(&c).await;
    Rig {
        _node: node,
        _tmp: tmp,
        c,
        h,
        st: rec.station_id.as_str().to_owned(),
    }
}

impl Rig {
    /// An entry of the allow-list; answers its id.
    async fn page(&self, label: &str, origin: &str) -> String {
        self.c
            .store
            .wall_page_add(CheckedPage {
                label: label.to_owned(),
                url: format!("{origin}/dash?x=1"),
                origin: origin.to_owned(),
                fit: PageFit::Long,
            })
            .await
            .unwrap()
            .unwrap()
            .page_id
    }

    /// A display and its login cookie.
    async fn display(&self, name: &str) -> (String, String) {
        display_login(&self.c, name, &[]).await
    }

    async fn show(&self, display: &str, page_id: &str) {
        let r = post(
            &self.c,
            &self.h,
            &format!("/api/displays/{display}/directive"),
            json!({ "mode": "page", "page": { "page_id": page_id }, "pin": true, "filter": null }),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }

    async fn directive(&self, cookie: &str, features: bool) -> Http {
        let mut headers = vec![("Cookie", cookie)];
        if features {
            headers.push(FEATURES);
        }
        self.c.get("/api/displays/self/directive", &headers).await
    }

    async fn page_of(&self, cookie: &str, page_id: &str) -> Http {
        get_with(
            &self.c,
            &format!("/api/displays/self/page/{page_id}"),
            cookie,
        )
        .await
    }

    async fn document(&self, cookie: Option<&str>) -> Http {
        match cookie {
            Some(cookie) => self.c.get("/display", &[("Cookie", cookie)]).await,
            None => self.c.get("/display", &[]).await,
        }
    }

    /// The `frame-src` directive of the policy of the document.
    fn frame_src(doc: &Http) -> String {
        let csp = doc.header("content-security-policy").unwrap();
        csp.split("; ")
            .find(|d| d.starts_with("frame-src"))
            .unwrap_or_else(|| panic!("no frame-src in {csp}"))
            .trim_end_matches(';')
            .to_owned()
    }

    /// The `frame-src` of a document without a page origin.
    fn base(&self) -> String {
        format!(
            "frame-src https://{}/api/displays/self/artifact/",
            self.c.addr
        )
    }

    /// The `frame-src` of a document with the page origin `origin`.
    fn with_page(&self, origin: &str) -> String {
        format!("{} {origin}", self.base())
    }

    fn assert_base(&self, doc: &Http) {
        assert_eq!(doc.status, 200);
        assert_eq!(Self::frame_src(doc), self.base());
        assert_eq!(doc.header("cache-control"), Some("no-cache"));
    }
}

#[tokio::test]
async fn self_page_answers_only_for_the_current_directive() {
    let r = rig().await;
    let (display, cookie) = r.display("Wall").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    // No page directive yet.
    expect(&r.page_of(&cookie, &id).await, 404, "unknown_page");
    r.show(&display, &id).await;
    let ok = r.page_of(&cookie, &id).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(
        ok.body,
        json!({
            "page_id": id,
            "label": "Grafana",
            "url": "https://grafana.example/dash?x=1",
            "fit": "long",
        })
    );
    assert_eq!(ok.header("cache-control"), Some("no-store"));
    // Another entry is not the current directive.
    let other = r.page("Other", "https://other.example").await;
    expect(&r.page_of(&cookie, &other).await, 404, "unknown_page");
    // Without a display login: the owner's login is not one.
    expect(
        &get(&r.c, &r.h, &format!("/api/displays/self/page/{id}")).await,
        401,
        "login_required",
    );
    expect(
        &r.c.get(&format!("/api/displays/self/page/{id}"), &[]).await,
        401,
        "login_required",
    );
}

#[tokio::test]
async fn every_miss_is_the_same_404() {
    let r = rig().await;
    let (shows_page, page_cookie) = r.display("Page").await;
    let (_, mission_cookie) = r.display("Mission").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    let other = r.page("Other", "https://other.example").await;
    r.show(&shows_page, &id).await;
    let misses = [
        // The display shows another page.
        r.page_of(&page_cookie, &other).await,
        // An id that names no entry, and ids that are not even ids.
        r.page_of(&page_cookie, "01J0000000000000000000000A").await,
        r.page_of(&page_cookie, "x").await,
        r.page_of(&page_cookie, "%00").await,
        // A display that shows Mission.
        r.page_of(&mission_cookie, &id).await,
        r.page_of(&mission_cookie, "01J0000000000000000000000A")
            .await,
    ];
    for miss in &misses {
        expect(miss, 404, "unknown_page");
        assert_eq!(miss.body, misses[0].body);
    }
    assert_eq!(misses[0].body["message"], "No such page.");
    // A deleted entry is the same miss.
    let gone = post(
        &r.c,
        &r.h,
        &format!("/api/wall-pages/{id}/delete"),
        json!({}),
    )
    .await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    let after = r.page_of(&page_cookie, &id).await;
    expect(&after, 404, "unknown_page");
    assert_eq!(after.body, misses[0].body);
}

#[tokio::test]
async fn an_older_wall_reads_mission_for_a_page_directive() {
    let r = rig().await;
    let (display, cookie) = r.display("Wall").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    r.show(&display, &id).await;
    let current = r.directive(&cookie, true).await;
    assert_eq!(current.status, 200, "{}", current.body);
    assert_eq!(current.body["generation"], 1);
    assert_eq!(current.body["directive"]["mode"], "page");
    assert_eq!(current.body["directive"]["page"]["page_id"], id);
    // No features header: Mission, with the same generation.
    let older = r.directive(&cookie, false).await;
    assert_eq!(older.status, 200, "{}", older.body);
    assert_eq!(older.body["generation"], 1);
    assert_eq!(
        older.body["directive"],
        json!({ "mode": "mission", "pin": false, "filter": null })
    );
    // Another feature token does not count.
    let other =
        r.c.get(
            "/api/displays/self/directive",
            &[
                ("Cookie", &cookie),
                ("X-FleetDeck-Features", "artifacts_video.v1"),
            ],
        )
        .await;
    assert_eq!(other.body["directive"]["mode"], "mission");
    // Another mode is the same for both.
    let r2 = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{display}/directive"),
        json!({ "mode": "ambient" }),
    )
    .await;
    assert_eq!(r2.status, 200, "{}", r2.body);
    for features in [true, false] {
        let read = r.directive(&cookie, features).await;
        assert_eq!(read.body["directive"]["mode"], "ambient");
        assert_eq!(read.body["generation"], 2);
    }
}

#[tokio::test]
async fn page_delete_resets_the_display() {
    let r = rig().await;
    let (display, cookie) = r.display("Wall").await;
    let (_, mission_cookie) = r.display("Quiet").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    r.show(&display, &id).await;
    let gone = post(
        &r.c,
        &r.h,
        &format!("/api/wall-pages/{id}/delete"),
        json!({}),
    )
    .await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    assert_eq!(gone.body, json!({ "displays_reset": 1 }));
    let read = r.directive(&cookie, true).await;
    assert_eq!(read.body["directive"]["mode"], "mission");
    assert_eq!(read.body["generation"], 2);
    expect(&r.page_of(&cookie, &id).await, 404, "unknown_page");
    // The other display never showed it.
    assert_eq!(
        r.directive(&mission_cookie, true).await.body["generation"],
        0
    );
    // A new page directive for a deleted entry is refused.
    let refused = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{display}/directive"),
        json!({ "mode": "page", "page": { "page_id": id }, "pin": false, "filter": null }),
    )
    .await;
    expect(&refused, 400, "invalid_directive");
    // A body with a URL is a parse error.
    let url = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{display}/directive"),
        json!({ "mode": "page", "page": { "page_id": id }, "url": "https://x.example" }),
    )
    .await;
    expect(&url, 400, "invalid_directive");
}

#[tokio::test]
async fn anonymous_display_document_has_no_page_origin() {
    let r = rig().await;
    let (display, _cookie) = r.display("Wall").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    r.show(&display, &id).await;
    let doc = r.document(None).await;
    r.assert_base(&doc);
    assert_eq!(doc.text, "<p>board</p>");
    let csp = doc.header("content-security-policy").unwrap();
    assert!(!csp.contains("grafana.example"), "{csp}");
    // A cookie that names no login is the same as no cookie.
    let junk = r.document(Some("fd_display=fddisp1_nosuchlogin")).await;
    r.assert_base(&junk);
    let empty = r.document(Some("fd_display=")).await;
    r.assert_base(&empty);
}

#[tokio::test]
async fn owner_and_member_logins_get_no_page_origin() {
    let r = rig().await;
    let (display, _cookie) = r.display("Wall").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    r.show(&display, &id).await;
    let share = json!({ "kind": "session", "target": format!("{}/s1", r.st), "role": "viewer" });
    let (member, _, _) = join(&r.c, &r.h, share, "Alice").await;
    for (who, headers) in [("owner", &r.h), ("member", &member)] {
        let cookie = &headers[0].1;
        let doc = r.document(Some(cookie)).await;
        assert_eq!(doc.status, 200, "{who}");
        assert_eq!(Rig::frame_src(&doc), r.base(), "{who}");
        assert_eq!(doc.header("cache-control"), Some("no-cache"), "{who}");
    }
}

#[tokio::test]
async fn a_display_document_has_only_its_own_page_origin() {
    let r = rig().await;
    let (a, cookie_a) = r.display("A").await;
    let (b, cookie_b) = r.display("B").await;
    let (_c, cookie_c) = r.display("C").await;
    let x = r.page("X", "https://x.example").await;
    let y = r.page("Y", "http://localhost:3000").await;
    r.show(&a, &x).await;
    r.show(&b, &y).await;
    // C shows Mission.
    let doc_a = r.document(Some(&cookie_a)).await;
    assert_eq!(doc_a.status, 200);
    assert_eq!(Rig::frame_src(&doc_a), r.with_page("https://x.example"));
    assert_eq!(doc_a.header("cache-control"), Some("no-store"));
    let doc_b = r.document(Some(&cookie_b)).await;
    assert_eq!(Rig::frame_src(&doc_b), r.with_page("http://localhost:3000"));
    assert_eq!(doc_b.header("cache-control"), Some("no-store"));
    let doc_c = r.document(Some(&cookie_c)).await;
    r.assert_base(&doc_c);
    // Neither document names the other page.
    assert!(
        !doc_a
            .header("content-security-policy")
            .unwrap()
            .contains("localhost")
    );
    assert!(
        !doc_b
            .header("content-security-policy")
            .unwrap()
            .contains("x.example")
    );
    // Each page answer is `no-store`, and only for its own display.
    let page_a = r.page_of(&cookie_a, &x).await;
    assert_eq!(page_a.status, 200, "{}", page_a.body);
    assert_eq!(page_a.header("cache-control"), Some("no-store"));
    let page_b = r.page_of(&cookie_b, &y).await;
    assert_eq!(page_b.status, 200, "{}", page_b.body);
    assert_eq!(page_b.header("cache-control"), Some("no-store"));
    expect(&r.page_of(&cookie_a, &y).await, 404, "unknown_page");
    expect(&r.page_of(&cookie_b, &x).await, 404, "unknown_page");
    expect(&r.page_of(&cookie_c, &x).await, 404, "unknown_page");
    // A display with both its own cookie and the owner's cookie is still
    // the display of the cookie `fd_display`.
    let both = format!("{}; {}", r.h[0].1, cookie_a);
    let doc_both = r.document(Some(&both)).await;
    assert_eq!(Rig::frame_src(&doc_both), r.with_page("https://x.example"));
}

#[tokio::test]
async fn a_revoked_display_gets_no_page_origin() {
    let r = rig().await;
    let (display, cookie) = r.display("Wall").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    r.show(&display, &id).await;
    let before = r.document(Some(&cookie)).await;
    assert_eq!(
        Rig::frame_src(&before),
        r.with_page("https://grafana.example")
    );
    let revoked = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{display}/revoke"),
        json!({}),
    )
    .await;
    assert_eq!(revoked.status, 204, "{}", revoked.body);
    r.assert_base(&r.document(Some(&cookie)).await);
}

#[tokio::test]
async fn a_deleted_entry_gives_no_page_origin() {
    let r = rig().await;
    let (display, cookie) = r.display("Wall").await;
    let id = r.page("Grafana", "https://grafana.example").await;
    r.show(&display, &id).await;
    let before = r.document(Some(&cookie)).await;
    assert_eq!(
        Rig::frame_src(&before),
        r.with_page("https://grafana.example")
    );
    let gone = post(
        &r.c,
        &r.h,
        &format!("/api/wall-pages/{id}/delete"),
        json!({}),
    )
    .await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    r.assert_base(&r.document(Some(&cookie)).await);
    // Another mode gives no page origin either.
    let mode = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{display}/directive"),
        json!({ "mode": "ambient" }),
    )
    .await;
    assert_eq!(mode.status, 200, "{}", mode.body);
    r.assert_base(&r.document(Some(&cookie)).await);
}

#[tokio::test]
async fn an_invalid_stored_origin_is_left_out_of_the_header() {
    let r = rig().await;
    for origin in [
        "https://x.example; script-src *",
        "https://a b.example",
        "https://UPPER.example",
        "ftp://x.example",
        "https://x.example/path",
        "https://x.example:123456",
        "https://[::g]",
        "https://",
        "x.example",
    ] {
        let (display, cookie) = r.display("Wall").await;
        let id = r.page("Odd", origin).await;
        r.show(&display, &id).await;
        let doc = r.document(Some(&cookie)).await;
        assert_eq!(doc.status, 200, "{origin}");
        assert_eq!(Rig::frame_src(&doc), r.base(), "{origin}");
        assert_eq!(doc.header("cache-control"), Some("no-cache"), "{origin}");
        let csp = doc.header("content-security-policy").unwrap();
        assert!(!csp.contains("script-src *"), "{origin}: {csp}");
    }
    // The grammar accepts a bracketed IPv6 host and a port.
    let (display, cookie) = r.display("Wall").await;
    let id = r.page("Six", "http://[::1]:8080").await;
    r.show(&display, &id).await;
    let doc = r.document(Some(&cookie)).await;
    assert_eq!(Rig::frame_src(&doc), r.with_page("http://[::1]:8080"));
}
