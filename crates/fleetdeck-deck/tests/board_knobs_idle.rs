//! A shortened idle limit is durable (knobs PRE-PR r2): a session that idled
//! past the short limit stays dead when the limit grows again, a save whose
//! clamp fails writes nothing, and the deck start clamps the stored deadlines.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use common::board::{Extras, board_with_extras};
use common::*;
use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_deck::knobs::DeckKnobs;
use fleetdeck_deck::serve::{RunningDeck, StartOptions};
use fleetdeck_deck::store::LoginMethod;
use fleetdeck_deck::store::auth::secret_hash;
use fleetdeck_deck::store::displays::NewDisplay;
use fleetdeck_link::reconnect::BackoffPolicy;
use serde_json::json;
use settings_support::*;

const DECK_TOML: &str = "name = \"alpha\"\nlisten = [\"127.0.0.1:7420\"]\n";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

async fn revision(c: &common::board::BoardClient, h: &Headers) -> String {
    get(c, h, "/api/settings/deck").await.body["revision"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn a_raised_idle_limit_does_not_revive_a_session_that_idled_past_the_short_limit() {
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
    // The sessions are fresh when the limits shrink.
    let idle = owner(&c).await;
    let busy = owner(&c).await;
    let admin = owner(&c).await;
    let display = c
        .store
        .create_display(NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![f.station.clone()],
        })
        .await
        .unwrap();
    let (wall, _) = c
        .store
        .create_display_session(display.display_id)
        .await
        .unwrap();
    let r = post(
        &c,
        &admin,
        "/api/settings/deck",
        json!({
            "changes": { "security.login_idle_days": 1, "security.display_idle_days": 1 },
            "revision": revision(&c, &admin).await,
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(c.store.knobs().login_idle_ms, DAY_MS);
    // One session is used within the short limit; then the idle ones pass it.
    f.det.advance(DAY / 2);
    assert_eq!(get(&c, &busy, "/api/settings/deck").await.status, 200);
    f.det.advance(DAY / 2 + DAY / 4);
    // The limits grow again, with a session that is new under the short limit.
    let admin = owner(&c).await;
    let elevation = elevate(&c, &admin).await;
    let r = post(
        &c,
        &admin,
        "/api/settings/deck",
        json!({
            "changes": { "security.login_idle_days": 7, "security.display_idle_days": 30 },
            "revision": revision(&c, &admin).await,
            "elevation": elevation,
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(c.store.knobs().login_idle_ms, 7 * DAY_MS);
    expect(
        &get(&c, &idle, "/api/settings/deck").await,
        401,
        "login_required",
    );
    assert!(c.store.display_session(&wall).await.unwrap().is_none());
    assert_eq!(get(&c, &busy, "/api/settings/deck").await.status, 200);
    assert_eq!(get(&c, &admin, "/api/settings/deck").await.status, 200);
}

#[tokio::test]
async fn a_save_whose_clamp_fails_writes_and_publishes_nothing() {
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
    let admin = owner(&c).await;
    let elevation = elevate(&c, &admin).await;
    let revision = revision(&c, &admin).await;
    let before = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    let knobs_before = c.store.knobs();
    // A database that refuses to shorten a deadline.
    c.store
        .db()
        .call(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER refuse_clamp BEFORE UPDATE OF idle_until ON login_sessions
                 WHEN NEW.idle_until < OLD.idle_until
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let r = post(
        &c,
        &admin,
        "/api/settings/deck",
        json!({
            "changes": { "security.login_idle_days": 1 },
            "revision": revision,
            "elevation": elevation,
        }),
    )
    .await;
    expect(&r, 400, "cannot_apply");
    assert!(
        r.body.to_string().contains("security.login_idle_days")
            && r.body.to_string().contains("nothing was saved"),
        "{}",
        r.body
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap(),
        before
    );
    assert_eq!(c.store.knobs(), knobs_before);
    assert_eq!(c.state.live.get().knobs, knobs_before);
    // A save that does not shorten a limit needs no clamp.
    let r = post(
        &c,
        &admin,
        "/api/settings/deck",
        json!({
            "changes": { "security.login_idle_days": 14 },
            "revision": revision,
            "elevation": elevation,
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(c.store.knobs().login_idle_ms, 14 * DAY_MS);
}

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn the_deck_start_clamps_the_stored_idle_deadlines_to_the_configured_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = NodeDirs {
        config: tmp.path().join("config"),
        data: tmp.path().join("data"),
    };
    std::fs::create_dir_all(&dirs.config).unwrap();
    let path = dirs.deck_config();
    let write = |extra: &str| {
        std::fs::write(
            &path,
            format!(
                "name = \"d\"\ntailnet_auto = false\nlisten = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:0\"]\n{extra}"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    let start = || async {
        RunningDeck::start(
            &dirs,
            &DeckConfig::load(&path).unwrap(),
            fast(),
            StartOptions::default(),
        )
        .await
        .unwrap()
    };
    // A first start with the default limits: a login with a 7-day deadline
    // that was last seen 2 days ago.
    write("");
    let first = start().await;
    let login = first.store.create_login(LoginMethod::Link).await.unwrap();
    let hash = secret_hash(login.cookie.expose());
    let two_days_ago = first.store.now_ms() - 2 * DAY_MS;
    let row = hash.clone();
    first
        .store
        .db()
        .call(move |c| {
            c.execute(
                "UPDATE login_sessions SET last_seen = ?2, idle_until = ?2 + ?3
                 WHERE session_hash = ?1",
                rusqlite::params![row, two_days_ago, 7 * DAY_MS],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    first.stop().await;
    // A start with a 1-day idle limit clamps the deadline.
    write("\n[security]\nlogin_idle_days = 1\n");
    let second = start().await;
    assert_eq!(second.store.knobs().login_idle_ms, DAY_MS);
    let row = hash.clone();
    let deadline: i64 = second
        .store
        .db()
        .call(move |c| {
            Ok(c.query_row(
                "SELECT idle_until FROM login_sessions WHERE session_hash = ?1",
                [row],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(deadline, two_days_ago + DAY_MS);
    assert_eq!(
        second.store.login_session(&login.cookie).await.unwrap(),
        None
    );
    second.stop().await;
    // The default limit comes back: the session stays dead.
    write("");
    let third = start().await;
    assert_eq!(third.store.knobs(), DeckKnobs::default());
    assert_eq!(
        third.store.login_session(&login.cookie).await.unwrap(),
        None
    );
    third.stop().await;
}
