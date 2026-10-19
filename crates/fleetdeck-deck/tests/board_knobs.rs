//! The deck reads its owner knobs (K2, knobs design section 2) over loopback
//! HTTPS: a save that lowers a login limit ends the session over it and its
//! sockets, the login cookie lives as long as the stored expiry, the
//! elevation, the invitation and the mail delivery use the configured times.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;
use std::time::Duration;

use common::board::{Extras, board, board_with_extras, board_with_node};
use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::board_fence::registry::{SessionKind, SocketKind};
use fleetdeck_deck::knobs::DeckKnobs;
use fleetdeck_deck::live::DeckLive;
use fleetdeck_deck::store::auth::secret_hash;
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{ProviderKind, SessionState};
use serde_json::json;
use settings_support::*;

const DECK_TOML: &str = "name = \"alpha\"\nlisten = [\"127.0.0.1:7420\"]\n";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[tokio::test]
async fn a_save_that_lowers_the_login_limits_ends_the_session_over_them_and_its_sockets() {
    let tmp = tempfile::tempdir().unwrap();
    let f = fixture().await;
    let c = board_with_extras(
        Arc::clone(&f.ctx),
        Extras {
            settings: Some(deck_settings(tmp.path(), DECK_TOML)),
            ..Extras::default()
        },
    )
    .await;
    let h = owner(&c).await;
    let hash = secret_hash(h[0].1.split_once('=').unwrap().1);
    f.det.advance(DAY * 2);
    let fresh = owner(&c).await;
    let fresh_hash = secret_hash(fresh[0].1.split_once('=').unwrap().1);
    let socket =
        c.store
            .registry()
            .register(&hash, SessionKind::Owner, SocketKind::Board, "10.0.0.1");
    let kept = c.store.registry().register(
        &fresh_hash,
        SessionKind::Owner,
        SocketKind::Board,
        "10.0.0.2",
    );
    let revision = get(&c, &fresh, "/api/settings/deck").await.body["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    // A login that is 2 days old is over a limit of 1 day; the new one is not.
    let r = post(
        &c,
        &fresh,
        "/api/settings/deck",
        json!({
            "changes": { "security.login_idle_days": 1, "security.login_max_days": 1 },
            "revision": revision,
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let live = c.state.live.get();
    assert_eq!(live.knobs.login_max_ms, DAY_MS);
    assert_eq!(c.store.knobs(), live.knobs);
    assert!(socket.is_revoked());
    assert!(!kept.is_revoked());
    expect(
        &get(&c, &h, "/api/settings/deck").await,
        401,
        "login_required",
    );
    assert_eq!(get(&c, &fresh, "/api/settings/deck").await.status, 200);
}

#[tokio::test]
async fn the_login_cookie_lives_as_long_as_the_stored_expiry() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    c.store.set_knobs(DeckKnobs {
        login_max_ms: 3 * DAY_MS,
        ..DeckKnobs::default()
    });
    let token = c.store.create_login_link().await.unwrap();
    let r = c
        .post("/api/auth/link", &[], json!({ "token": token.expose() }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["expires_at"].as_i64().unwrap(),
        c.store.now_ms() + 3 * DAY_MS
    );
    let set = r.header("set-cookie").unwrap();
    assert!(set.contains("Max-Age=259200"), "{set}");
}

#[tokio::test]
async fn an_elevation_lives_as_long_as_the_configured_minutes() {
    let tmp = tempfile::tempdir().unwrap();
    let f = fixture().await;
    let c = board_with_extras(
        Arc::clone(&f.ctx),
        Extras {
            settings: Some(deck_settings(tmp.path(), DECK_TOML)),
            ..Extras::default()
        },
    )
    .await;
    set_password(&c).await;
    let h = owner(&c).await;
    c.state.live.publish(DeckLive {
        knobs: DeckKnobs {
            elevation_ms: 3 * 60 * 1000,
            ..DeckKnobs::default()
        },
        ..(*c.state.live.get()).clone()
    });
    let r = post(&c, &h, "/api/auth/elevate", json!({ "password": PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["expires_at"].as_i64().unwrap(),
        c.store.now_ms() + 3 * 60 * 1000
    );
}

fn clock_ids() -> (SharedClock, SharedIds) {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    (clock, ids)
}

#[tokio::test]
async fn the_board_invitation_expires_after_the_configured_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.path().join("deck.db"), clock, ids)
        .await
        .unwrap();
    let fast = BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    };
    let node = DeckNode::start(
        store,
        Arc::new(Identity::generate().unwrap()),
        &["127.0.0.1:0".parse().unwrap()],
        fast,
    )
    .await
    .unwrap();
    let c = board_with_node(Arc::clone(&node)).await;
    c.store.set_knobs(DeckKnobs {
        invite: Duration::from_secs(5 * 60),
        ..DeckKnobs::default()
    });
    let h = owner(&c).await;
    let before = c.store.now_ms();
    let r = post(&c, &h, "/api/stations/invite", json!({})).await;
    let after = c.store.now_ms();
    assert_eq!(r.status, 200, "{}", r.body);
    let expires = r.body["expires_at"].as_i64().unwrap();
    assert!(
        (before + 5 * 60 * 1000..=after + 5 * 60 * 1000).contains(&expires),
        "{expires}"
    );
}

#[tokio::test]
async fn a_mail_expires_after_the_configured_delivery_hours() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    let mut card = SessionSummary::new("s1".into());
    card.state = SessionState::Running;
    card.cwd = Some("/p/app".to_owned());
    card.provider = Some(ProviderKind::Claude);
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card])
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    c.store.set_knobs(DeckKnobs {
        delivery_ms: 2 * 60 * 60 * 1000,
        ..DeckKnobs::default()
    });
    let now = c.store.now_ms();
    let body = json!({ "mail_id": "m1", "station": f.station.0.clone(),
                       "session_id": "s1", "text": "hello" });
    let r = post(&c, &h, "/api/mail", body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let expires: i64 = c
        .store
        .db()
        .call(|conn| Ok(conn.query_row("SELECT expires_at FROM outbox", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(expires, now + 2 * 60 * 60 * 1000);
}
