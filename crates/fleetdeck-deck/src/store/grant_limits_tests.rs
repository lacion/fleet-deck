//! B27 T3 at the store level: the creation rules of a grant under the
//! `[grant_limits]` of the deck (security-options §3.2, §3.3, §3.4), tests 1,
//! 5, 6, 7, 8, 11, 12 and 13. (The worker attach, the routes and the V52
//! migration are in `tests/grant_limits.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use fleetdeck_core::config_worker_grant::{CLAUDE_BYPASS, CODEX_BYPASS};
use fleetdeck_proto::command::GrantKind;
use fleetdeck_proto::{SessionId, StationId};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::DeckStore;
use crate::store::grant_spawn_caps::spawn_caps;
use crate::store::grants::{
    Grant, MAX_ANSWER_CAP, MAX_IDLE_TTL_MS, MAX_SPAWN_CAP, MAX_TTL_MS, NewGrant,
};
use crate::store::worker_grant::{MAX_WORKER_SPAWN_CAP, WORKER_GRANT_CAP};

const HOUR: i64 = 60 * 60 * 1000;

fn limits(change: impl FnOnce(&mut GrantLimits)) -> GrantLimits {
    let mut l = GrantLimits::default();
    change(&mut l);
    l
}

/// An owner grant with the classes read and spawn.
fn owner(change: impl FnOnce(&mut NewGrant)) -> NewGrant {
    let mut g = NewGrant::new(StationId::from("alpha"), SessionId::from("coord"));
    g.classes = BTreeSet::from([GrantClass::Read, GrantClass::Spawn]);
    change(&mut g);
    g
}

/// A valid worker grant request.
fn worker(change: impl FnOnce(&mut NewGrant)) -> NewGrant {
    let mut g = NewGrant::new(StationId::from("alpha"), SessionId::from("coord"));
    g.kind = GrantKind::Worker;
    g.classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
    ]);
    g.spawn_cap = 2;
    change(&mut g);
    g
}

fn refusal(got: Result<NewGrant>) -> String {
    match got.err() {
        Some(DeckError::Invalid(m)) => m,
        other => panic!("a refusal was expected, got {other:?}"),
    }
}

fn modes(list: &[(&str, &str)]) -> BTreeMap<String, String> {
    list.iter()
        .map(|(f, m)| ((*f).to_owned(), (*m).to_owned()))
        .collect()
}

/// A stored owner grant with the spawn class, as `grant_spawn_caps_tests` makes one.
async fn stored(kind: GrantKind) -> Grant {
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let mut stored = store.create_grant(owner(|_| {})).await.unwrap();
    stored.kind = kind;
    stored
}

#[test]
fn the_default_limits_equal_the_old_constants() {
    let d = GrantLimits::default();
    assert_eq!(
        d.worker_grants,
        Some(u32::try_from(WORKER_GRANT_CAP).unwrap())
    );
    assert_eq!(d.spawn_cap, Some(MAX_SPAWN_CAP));
    assert_eq!(d.worker_spawn_cap, Some(MAX_WORKER_SPAWN_CAP));
    assert_eq!(
        d.idle_hours.map(|h| i64::from(h) * HOUR),
        Some(MAX_IDLE_TTL_MS)
    );
    assert_eq!(d.answer_cap, Some(MAX_ANSWER_CAP));
    assert_eq!(MAX_TTL_MS, 7 * 24 * HOUR);
}

#[test]
fn an_owner_grant_above_a_limit_is_refused_with_the_new_text() {
    let l = GrantLimits::default();
    assert_eq!(
        refusal(prepare_grant(owner(|g| g.spawn_cap = 51), &l, false)),
        OVER_SPAWN_LIMIT
    );
    assert!(prepare_grant(owner(|g| g.spawn_cap = 50), &l, false).is_ok());
    let wide = limits(|l| l.spawn_cap = Some(100));
    assert!(prepare_grant(owner(|g| g.spawn_cap = 51), &wide, false).is_ok());
    // NO_LIMIT needs the limit "none".
    assert_eq!(
        refusal(prepare_grant(
            owner(|g| g.spawn_cap = NO_LIMIT),
            &wide,
            false
        )),
        NO_LIMIT_REFUSED
    );
    let none = limits(|l| l.spawn_cap = None);
    let got = prepare_grant(owner(|g| g.spawn_cap = NO_LIMIT), &none, false).unwrap();
    assert_eq!(got.spawn_cap, NO_LIMIT);
}

#[test]
fn the_answer_and_idle_limits_of_an_owner_grant() {
    let l = GrantLimits::default();
    let answers = |cap: u32| {
        owner(|g| {
            g.classes.insert(GrantClass::Answer);
            g.answer_cap = Some(cap);
        })
    };
    assert_eq!(
        refusal(prepare_grant(answers(5001), &l, false)),
        OVER_ANSWER_LIMIT
    );
    assert!(prepare_grant(answers(5000), &l, false).is_ok());
    let wide = limits(|l| l.answer_cap = Some(6000));
    assert!(prepare_grant(answers(5001), &wide, false).is_ok());
    assert_eq!(
        refusal(prepare_grant(answers(NO_LIMIT), &wide, false)),
        NO_LIMIT_REFUSED
    );
    let none = limits(|l| l.answer_cap = None);
    let got = prepare_grant(answers(NO_LIMIT), &none, false).unwrap();
    assert_eq!(got.answer_cap, Some(NO_LIMIT));
    // The idle time: 25 h with a limit of 24 h; NO_LIMIT with none is stored
    // as the lifetime.
    let idle = |ms: i64| owner(|g| g.idle_ttl_ms = ms);
    assert_eq!(
        refusal(prepare_grant(idle(25 * HOUR), &l, false)),
        OVER_IDLE_LIMIT
    );
    assert_eq!(
        refusal(prepare_grant(idle(i64::from(NO_LIMIT)), &l, false)),
        NO_LIMIT_REFUSED
    );
    let none = limits(|l| l.idle_hours = None);
    let got = prepare_grant(idle(i64::from(NO_LIMIT)), &none, false).unwrap();
    assert_eq!(got.idle_ttl_ms, got.ttl_ms);
    // A number above the lifetime is stored as the lifetime.
    let got = prepare_grant(idle(20 * HOUR), &l, false).unwrap();
    assert_eq!(got.idle_ttl_ms, got.ttl_ms);
    let got = prepare_grant(idle(10 * HOUR), &l, false).unwrap();
    assert_eq!(got.idle_ttl_ms, 10 * HOUR);
}

#[tokio::test]
async fn a_lower_limit_keeps_existing_grants() {
    // A grant with a spawn cap of 40 exists; the limit goes down to 10.
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let g = store
        .create_grant(owner(|g| g.spawn_cap = 40))
        .await
        .unwrap();
    let mut knobs = store.knobs();
    knobs.grant.spawn_cap = Some(10);
    store.set_knobs(knobs);
    let kept = store.grant(g.grant_id).await.unwrap().unwrap();
    assert_eq!(kept.spawn_cap, 40);
    // A new grant with 40 is refused; one with 10 is accepted.
    let mut other = owner(|g| g.spawn_cap = 40);
    other.session_id = SessionId::from("coord-2");
    let err = store.create_grant(other).await.unwrap_err();
    assert!(matches!(err, DeckError::Invalid(m) if m == OVER_SPAWN_LIMIT));
    let mut ok = owner(|g| g.spawn_cap = 10);
    ok.session_id = SessionId::from("coord-3");
    store.create_grant(ok).await.unwrap();
}

#[tokio::test]
async fn a_cap_change_keeps_a_stored_bypass_ceiling_and_never_adds_one() {
    let l = GrantLimits::default();
    // Creation: a bypass ceiling needs `allow_bypass`, for both families.
    for (family, bypass) in [("claude", CLAUDE_BYPASS), ("codex", CODEX_BYPASS)] {
        let request = || worker(|g| g.max_modes = modes(&[(family, bypass)]));
        assert_eq!(
            refusal(prepare_grant(request(), &l, false)),
            "a worker grant needs valid mode ceilings"
        );
        assert!(prepare_grant(request(), &l, true).is_ok(), "{family}");
    }
    // The cap change: a worker grant with a stored bypass Claude ceiling.
    let mut grant = stored(GrantKind::Worker).await;
    grant.max_modes = modes(&[
        ("claude", CLAUDE_BYPASS),
        ("codex", "on-request/workspace-write"),
    ]);
    let same = grant.max_modes.clone();
    assert!(spawn_caps(&grant, Some(1), Some(&same), &l).is_ok());
    // A change that sets a bypass Codex ceiling is refused.
    let mut codex = same.clone();
    codex.insert("codex".to_owned(), CODEX_BYPASS.to_owned());
    assert_eq!(
        spawn_caps(&grant, None, Some(&codex), &l).err(),
        Some("a mode ceiling is not valid for its family")
    );
    // An owner grant never keeps one.
    let mut owner_grant = stored(GrantKind::Coordinator).await;
    owner_grant.max_modes = modes(&[("claude", CLAUDE_BYPASS)]);
    assert_eq!(
        spawn_caps(&owner_grant, None, Some(&owner_grant.max_modes.clone()), &l).err(),
        Some("a mode ceiling is not valid for its family")
    );
}

#[test]
fn an_omitted_answer_cap_is_lowered_to_the_limit() {
    let low = limits(|l| l.answer_cap = Some(1));
    let with_class = |cap: Option<u32>| {
        owner(|g| {
            g.classes.insert(GrantClass::Answer);
            g.answer_cap = cap;
        })
    };
    assert_eq!(default_answer_cap(&low), 1);
    let got = prepare_grant(with_class(None), &low, false).unwrap();
    assert_eq!(got.answer_cap, Some(1));
    assert_eq!(
        refusal(prepare_grant(with_class(Some(2)), &low, false)),
        OVER_ANSWER_LIMIT
    );
    // The default stays 500 under the default limit, and with no limit.
    let got = prepare_grant(with_class(None), &GrantLimits::default(), false).unwrap();
    assert_eq!(got.answer_cap, Some(500));
    let none = limits(|l| l.answer_cap = None);
    assert_eq!(default_answer_cap(&none), 500);
    // A cap without the class is refused.
    assert_eq!(
        refusal(prepare_grant(
            owner(|g| g.answer_cap = Some(1)),
            &GrantLimits::default(),
            false
        )),
        "answer_cap needs the answer class"
    );
}

#[test]
fn an_omitted_spawn_cap_is_lowered_to_the_limit() {
    let low = limits(|l| l.spawn_cap = Some(2));
    assert_eq!(default_spawn_cap(&low), 2);
    assert_eq!(default_spawn_cap(&GrantLimits::default()), 3);
    let none = limits(|l| l.spawn_cap = None);
    assert_eq!(default_spawn_cap(&none), 3);
    // The resolved value passes the check.
    let got = prepare_grant(
        owner(|g| g.spawn_cap = default_spawn_cap(&low)),
        &low,
        false,
    );
    assert_eq!(got.unwrap().spawn_cap, 2);
    // A grant without the spawn class is accepted: its unused cap (3 by
    // default) is not checked.
    let got = prepare_grant(
        owner(|g| {
            g.classes.remove(&GrantClass::Spawn);
        }),
        &low,
        false,
    );
    assert_eq!(got.unwrap().spawn_cap, 3);
}

#[tokio::test]
async fn the_spawn_limit_follows_the_grant_kind() {
    let wide = limits(|l| {
        l.spawn_cap = Some(50);
        l.worker_spawn_cap = Some(100);
    });
    assert_eq!(spawn_limit(GrantKind::Worker, &wide), Some(100));
    assert_eq!(spawn_limit(GrantKind::Coordinator, &wide), Some(50));
    // The worker attach stores 60; the owner grant with 60 is refused.
    let got = prepare_grant(worker(|g| g.spawn_cap = 60), &wide, false).unwrap();
    assert_eq!(got.spawn_cap, 60);
    assert_eq!(
        refusal(prepare_grant(owner(|g| g.spawn_cap = 60), &wide, false)),
        OVER_SPAWN_LIMIT
    );
    // A worker cap change: 80 passes, 101 does not.
    let grant = stored(GrantKind::Worker).await;
    assert!(spawn_caps(&grant, Some(80), None, &wide).is_ok());
    assert_eq!(
        spawn_caps(&grant, Some(101), None, &wide).err(),
        Some(OVER_WORKER_SPAWN_LIMIT)
    );
    // The limits are independent: a low worker limit with a high owner limit.
    let apart = limits(|l| {
        l.spawn_cap = Some(100);
        l.worker_spawn_cap = Some(10);
    });
    assert_eq!(
        spawn_caps(&grant, Some(20), None, &apart).err(),
        Some(OVER_WORKER_SPAWN_LIMIT)
    );
    let owner_grant = stored(GrantKind::Coordinator).await;
    assert!(spawn_caps(&owner_grant, Some(20), None, &apart).is_ok());
    assert_eq!(
        spawn_caps(&owner_grant, Some(101), None, &apart).err(),
        Some(OVER_SPAWN_LIMIT)
    );
    // NO_LIMIT in a cap change needs the limit of the kind to be none.
    assert_eq!(
        spawn_caps(&grant, Some(NO_LIMIT), None, &wide).err(),
        Some(NO_LIMIT_REFUSED)
    );
    let none = limits(|l| l.worker_spawn_cap = None);
    assert!(
        spawn_caps(&grant, Some(NO_LIMIT), None, &none)
            .unwrap()
            .widens
    );
}
