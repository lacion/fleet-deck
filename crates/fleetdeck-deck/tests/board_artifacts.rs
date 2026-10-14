//! Artifacts over a real link on loopback (R12, DL-165): publish through the
//! deck, serve the raw files under the sandbox policy, share links that
//! work without a login and while the station is off, revocation, expiry,
//! the owner-only rule and the audit rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board_with_node};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::artifacts::NewShare;
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::{ShellConfig, Shells};
use fleetdeck_station::store::{NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_station::workspace::PrivatePaths;
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(10);
const CSP: &str = "sandbox allow-scripts allow-popups allow-forms allow-modals; default-src * data: blob: 'unsafe-inline' 'unsafe-eval'";

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

fn clock_ids() -> (SharedClock, SharedIds) {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    (clock, ids)
}

async fn station(dir: &Path, root: PathBuf, cwd: &Path) -> Arc<StationNode> {
    let (clock, ids) = clock_ids();
    let store = Store::open(
        &dir.join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    store
        .create_session(NewSession {
            session_id: "s1".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: cwd.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let policy = Policy::new(vec![root], 2, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::new(),
        ids,
        Arc::new(PrivatePaths::default()),
    )
    .await
    .unwrap();
    StationNode::start_with_dirs(
        Arc::new(manager),
        Arc::new(Identity::generate().unwrap()),
        "alpha".to_owned(),
        &[],
        fast(),
        Arc::new(Shells::new(ShellConfig::default())),
        None,
        Some(dir.join("artifacts")),
    )
    .await
    .unwrap()
}

async fn deck(dir: &Path) -> (Arc<DeckNode>, BoardClient) {
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.join("deck.db"), clock, ids)
        .await
        .unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let node = DeckNode::start(store, identity, &["127.0.0.1:0".parse().unwrap()], fast())
        .await
        .unwrap();
    let board = board_with_node(Arc::clone(&node)).await;
    (node, board)
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.text);
}

/// Posts until the deck has a live link to `alpha`.
async fn publish(c: &BoardClient, h: &[(&str, &str)], body: Value) -> Http {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.post("/api/artifacts/publish", h, body.clone()).await;
        let waiting = [(404, "unknown_station"), (409, "station_unreachable")];
        if !waiting.contains(&(r.status, r.code())) {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.text);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn token_of(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn an_artifact_is_published_served_under_the_sandbox_and_shared_while_the_station_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects").canonicalize_or_create();
    let work = root.join("app");
    std::fs::create_dir_all(work.join("img")).unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    std::fs::write(
        work.join("page.html"),
        r#"<img src="img/a.png"><script src="https://cdn.example/lib.js"></script>"#,
    )
    .unwrap();
    std::fs::write(work.join("img/a.png"), b"PNGDATA").unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone(), &work).await;
    st.join(&node.invite().unwrap()).await.unwrap();
    let (cookie, csrf) = board.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    // Publish through the deck.
    let r = publish(
        &board,
        &h,
        json!({"station": "alpha", "session_id": "s1", "path": "page.html", "title": "Report"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["created"], true);
    assert_eq!(r.body["artifact"]["author"], "owner");
    assert_eq!(
        r.body["version"]["files"],
        json!(["page.html", "img/a.png"])
    );
    let id = r.body["artifact"]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The index lists it; a search filters by title.
    let list = board.get("/api/artifacts", &h).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1, "{}", list.text);
    assert_eq!(list.body[0]["title"], "Report");
    assert_eq!(list.body[0]["kind"], "html");
    assert_eq!(list.body[0]["station_name"], "alpha");
    let found = board
        .get("/api/artifacts?q=repo&station=alpha&session=s1", &h)
        .await;
    assert_eq!(found.body.as_array().unwrap().len(), 1);
    let none = board.get("/api/artifacts?q=zzz", &h).await;
    assert!(none.body.as_array().unwrap().is_empty());
    let wild = board.get("/api/artifacts?q=%25", &h).await;
    assert!(
        wild.body.as_array().unwrap().is_empty(),
        "a wildcard is literal"
    );

    // The raw route serves the bytes under the sandbox policy.
    let raw = format!("/api/artifacts/alpha/{id}/1/raw");
    let page = board.get(&format!("{raw}/page.html"), &h).await;
    assert_eq!(page.status, 200, "{}", page.text);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(page.header("content-security-policy"), Some(CSP));
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    assert!(page.text.contains("img/a.png"));
    let png = board.get(&format!("{raw}/img/a.png"), &h).await;
    assert_eq!(png.header("content-type"), Some("image/png"));
    assert_eq!(png.header("content-security-policy"), None);
    assert_eq!(png.text, "PNGDATA");
    expect(
        &board.get(&format!("{raw}/..%2Fx"), &h).await,
        400,
        "invalid_path",
    );
    expect(
        &board.get(&format!("{raw}/nope.png"), &h).await,
        404,
        "not_found",
    );
    expect(
        &board.get(&format!("{raw}/page.html"), &[]).await,
        401,
        "login_required",
    );
    expect(
        &board
            .get("/api/artifacts/alpha/ghost/1/raw/page.html", &h)
            .await,
        404,
        "unknown_artifact",
    );

    // The detail has the version and no share yet.
    let detail = board.get(&format!("/api/artifacts/alpha/{id}"), &h).await;
    assert_eq!(detail.status, 200, "{}", detail.text);
    assert_eq!(
        detail.body["versions"][0]["files"],
        json!(["page.html", "img/a.png"])
    );
    assert!(detail.body["shares"].as_array().unwrap().is_empty());

    // A token is not the owner (DL-138): no share link, whatever its scopes.
    let token = board
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let r = board
        .post(
            &format!("/api/artifacts/alpha/{id}/shares"),
            &[("Authorization", bearer.as_str())],
            json!({"expires_in_days": 7}),
        )
        .await;
    expect(&r, 403, "owner_only");
    for bad in [
        json!({"expires_in_days": 0}),
        json!({"expires_in_days": 366}),
        json!({}),
    ] {
        let r = board
            .post(&format!("/api/artifacts/alpha/{id}/shares"), &h, bad)
            .await;
        assert_eq!(r.status, 400, "{}", r.text);
    }
    let r = board
        .post(
            &format!("/api/artifacts/alpha/{id}/shares"),
            &h,
            json!({"version": 9, "expires_in_days": 7}),
        )
        .await;
    expect(&r, 404, "unknown_version");

    // The owner makes a share; it opens without a login.
    let r = board
        .post(
            &format!("/api/artifacts/alpha/{id}/shares"),
            &h,
            json!({"expires_in_days": 7}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let url = r.body["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("https://") && url.contains("/a/") && url.ends_with('/'));
    let share_id = r.body["share_id"].as_str().unwrap().to_owned();
    let tok = token_of(&url);
    assert_eq!(tok.len(), 64);
    let public = |path: &str| format!("/a/{tok}/{path}");
    let main = board.get(&public(""), &[]).await;
    assert_eq!(main.status, 200, "{}", main.text);
    assert_eq!(main.header("content-security-policy"), Some(CSP));
    assert_eq!(main.header("referrer-policy"), Some("no-referrer"));
    assert!(main.text.contains("img/a.png"));
    assert_eq!(board.get(&public("img/a.png"), &[]).await.text, "PNGDATA");
    let slash = board.get(&format!("/a/{tok}"), &[]).await;
    assert_eq!(slash.status, 308);
    assert_eq!(board.get(&public("nope.png"), &[]).await.status, 404);
    assert_eq!(board.get(&public("..%2Fpage.html"), &[]).await.status, 404);
    assert_eq!(
        board
            .get(&format!("/a/{}/", "0".repeat(64)), &[])
            .await
            .status,
        404
    );
    assert_eq!(board.get("/a/short/", &[]).await.status, 404);

    // The share works while the station is off.
    st.shutdown();
    drop(st);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let main = board.get(&public(""), &[]).await;
    assert_eq!(main.status, 200, "{}", main.text);
    assert_eq!(board.get(&public("img/a.png"), &[]).await.text, "PNGDATA");

    // The owner sees the share and its views; the audit has each view.
    let detail = board.get(&format!("/api/artifacts/alpha/{id}"), &h).await;
    assert_eq!(detail.body["shares"][0]["share_id"], share_id.as_str());
    assert_eq!(detail.body["shares"][0]["views"], 2);
    assert_eq!(detail.body["shares"][0]["revoked"], false);
    let rows = board.store.audit_log(100).await.unwrap();
    assert_eq!(
        rows.iter()
            .filter(|e| e.action == "artifacts.share_view")
            .count(),
        2
    );
    assert!(
        rows.iter()
            .any(|e| e.action == "artifacts.share" && e.result == AuditResult::Ok)
    );
    assert!(
        !format!("{rows:?}").contains(&tok),
        "no row holds the token"
    );

    // The board's own viewer gets a short link that loads the bundle.
    let v = board
        .post(&format!("/api/artifacts/alpha/{id}/view"), &h, json!({}))
        .await;
    assert_eq!(v.status, 200, "{}", v.text);
    let view_url = v.body["url"].as_str().unwrap().to_owned();
    assert!(view_url.starts_with("/a/"));
    assert_eq!(board.get(&view_url, &[]).await.status, 200);
    assert_eq!(
        board.get(&format!("{view_url}img/a.png"), &[]).await.text,
        "PNGDATA"
    );
    let detail = board.get(&format!("/api/artifacts/alpha/{id}"), &h).await;
    assert_eq!(
        detail.body["shares"].as_array().unwrap().len(),
        1,
        "a view link is not listed"
    );

    // A revoke ends the share at once; a second revoke finds it revoked.
    let del = format!("/api/artifacts/alpha/{id}/shares/{share_id}");
    let r = board
        .req("DELETE", &del, &[("Authorization", bearer.as_str())], None)
        .await;
    expect(&r, 403, "owner_only");
    let r = board.req("DELETE", &del, &h, None).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(board.get(&public(""), &[]).await.status, 404);
    assert_eq!(board.get(&public("img/a.png"), &[]).await.status, 404);
    let r = board
        .req(
            "DELETE",
            &format!("/api/artifacts/alpha/{id}/shares/nope"),
            &h,
            None,
        )
        .await;
    expect(&r, 404, "unknown_share");
    let detail = board.get(&format!("/api/artifacts/alpha/{id}"), &h).await;
    assert_eq!(detail.body["shares"][0]["revoked"], true);

    // An expired share is gone too.
    let station_id = board
        .store
        .find_station("alpha".into())
        .await
        .unwrap()
        .unwrap()
        .station_id;
    let expired_token = "ab".repeat(32);
    let expired = fleetdeck_deck::store::auth::secret_hash(&expired_token);
    board
        .store
        .create_artifact_share(NewShare {
            token_hash: expired,
            station_id,
            artifact_id: id.clone(),
            version: Some(1),
            view: false,
            created_by: "owner".to_owned(),
            expires_at: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        board.get(&format!("/a/{expired_token}/"), &[]).await.status,
        404
    );
}

trait CanonicalOrCreate {
    fn canonicalize_or_create(self) -> PathBuf;
}

impl CanonicalOrCreate for PathBuf {
    fn canonicalize_or_create(self) -> PathBuf {
        std::fs::create_dir_all(&self).unwrap();
        self.canonicalize().unwrap()
    }
}

#[tokio::test]
async fn a_publish_outside_the_workspace_and_without_a_station_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects").canonicalize_or_create();
    let work = root.join("app");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    std::fs::write(tmp.path().join("outside.html"), "x").unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root, &work).await;
    st.join(&node.invite().unwrap()).await.unwrap();
    let (cookie, csrf) = board.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let outside = tmp.path().join("outside.html").display().to_string();
    let r = publish(
        &board,
        &h,
        json!({"station": "alpha", "session_id": "s1", "path": outside}),
    )
    .await;
    expect(&r, 400, "outside_roots");
    let r = board
        .post(
            "/api/artifacts/publish",
            &h,
            json!({"station": "ghost", "path": "/x.html"}),
        )
        .await;
    expect(&r, 404, "unknown_station");
    let r = board
        .post("/api/artifacts/publish", &h, json!({"nope": 1}))
        .await;
    expect(&r, 400, "invalid_body");
    // The raw route needs the cache or a live station.
    let r = board
        .get("/api/artifacts/alpha/ghost/1/raw/a.html", &h)
        .await;
    expect(&r, 404, "unknown_artifact");
    let rows = board.store.audit_log(100).await.unwrap();
    assert!(
        rows.iter()
            .any(|e| e.action == "artifacts.publish" && e.result == AuditResult::Failed)
    );
}
