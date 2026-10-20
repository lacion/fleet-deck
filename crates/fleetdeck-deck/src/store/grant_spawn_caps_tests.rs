//! The rules and the widening test of a spawn cap and mode ceiling change
//! (MCP P4a §8).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::{SessionId, StationId};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::{DeckStore, NewGrant};

const CODEX_WRITE: &str = "on-request/workspace-write";

fn modes(list: &[(&str, &str)]) -> BTreeMap<String, String> {
    list.iter()
        .map(|(f, m)| ((*f).to_owned(), (*m).to_owned()))
        .collect()
}

/// A stored grant with `classes`, a spawn cap of 3 and the ceilings
/// `claude: acceptEdits` and `codex: on-request/workspace-write`.
async fn grant(classes: &[GrantClass]) -> Grant {
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let mut g = NewGrant::new(StationId::from("alpha"), SessionId::from("coord"));
    g.classes = classes.iter().copied().collect();
    let mut stored = store.create_grant(g).await.unwrap();
    stored.spawn_cap = 3;
    stored.max_modes = modes(&[("claude", "acceptEdits"), ("codex", CODEX_WRITE)]);
    stored
}

fn spawner() -> [GrantClass; 2] {
    [GrantClass::Read, GrantClass::Spawn]
}

fn text(got: Result<SpawnCaps, &'static str>) -> &'static str {
    got.err().expect("a refusal")
}

fn widens(grant: &Grant, cap: Option<u32>, new: Option<&BTreeMap<String, String>>) -> bool {
    spawn_caps(grant, cap, new).expect("accepted").widens
}

#[tokio::test]
async fn the_rules_refuse_in_order_with_the_fixed_texts() {
    let desktop = grant(&[GrantClass::Desktop, GrantClass::Capture]).await;
    // Rule a comes before rule d, and before every value rule.
    let bad = modes(&[("gemini", "x")]);
    assert_eq!(
        text(spawn_caps(&desktop, Some(99), Some(&bad))),
        "spawn_cap needs the spawn class"
    );
    assert_eq!(
        text(spawn_caps(&desktop, None, Some(&bad))),
        "max_modes needs the spawn or manage class"
    );

    let spawn = grant(&spawner()).await;
    // Rule b comes before rule c and before rule e.
    assert_eq!(
        text(spawn_caps(&spawn, Some(51), Some(&bad))),
        "spawn_cap is 0 to 50"
    );
    let mut worker = grant(&spawner()).await;
    worker.kind = GrantKind::Worker;
    assert_eq!(
        text(spawn_caps(&worker, Some(11), Some(&bad))),
        "a worker grant has a spawn cap of at most 10"
    );
    // Rule e comes before rule f.
    let mixed = modes(&[("claude", "bypassPermissions"), ("gemini", "x")]);
    assert_eq!(
        text(spawn_caps(&spawn, None, Some(&mixed))),
        "max_modes has only the families claude and codex"
    );
    assert_eq!(
        text(spawn_caps(
            &spawn,
            None,
            Some(&modes(&[("claude", "bypassPermissions")]))
        )),
        "a mode ceiling is not valid for its family"
    );
    // The edges that pass.
    assert!(spawn_caps(&spawn, Some(50), None).is_ok());
    assert!(spawn_caps(&worker, Some(10), None).is_ok());
    assert!(spawn_caps(&spawn, Some(0), Some(&BTreeMap::new())).is_ok());
    let manage = grant(&[GrantClass::Read, GrantClass::Manage]).await;
    assert!(spawn_caps(&manage, None, Some(&modes(&[("claude", "plan")]))).is_ok());
    assert_eq!(
        text(spawn_caps(&manage, Some(1), None)),
        "spawn_cap needs the spawn class"
    );
}

#[tokio::test]
async fn the_cap_widens_only_when_it_is_higher() {
    let g = grant(&spawner()).await;
    assert!(!widens(&g, Some(2), None), "lower");
    assert!(!widens(&g, Some(3), None), "equal");
    assert!(widens(&g, Some(4), None), "higher");
    assert!(!widens(&g, Some(0), None), "below spawn_used is allowed");
}

#[tokio::test]
async fn a_ceiling_widens_unless_it_is_within_the_stored_one() {
    let g = grant(&spawner()).await;
    let same = g.max_modes.clone();
    assert!(!widens(&g, None, Some(&same)), "same map");
    assert!(
        !widens(&g, None, Some(&BTreeMap::new())),
        "every family removed"
    );
    assert!(
        !widens(&g, None, Some(&modes(&[("claude", "acceptEdits")]))),
        "a removed family"
    );
    assert!(
        !widens(
            &g,
            None,
            Some(&modes(&[
                ("claude", "plan"),
                ("codex", "untrusted/read-only")
            ]))
        ),
        "narrower ceilings"
    );
    assert!(
        widens(&g, None, Some(&modes(&[("claude", "auto")]))),
        "a higher ceiling"
    );
    assert!(
        widens(&g, None, Some(&modes(&[("codex", "never/read-only")]))),
        "incomparable codex, one way"
    );
    assert!(
        widens(
            &g,
            None,
            Some(&modes(&[("codex", "never/workspace-write")]))
        ),
        "a higher codex policy"
    );
    let mut only_claude = g.clone();
    only_claude.max_modes = modes(&[("claude", "acceptEdits")]);
    assert!(
        widens(
            &only_claude,
            None,
            Some(&modes(&[("codex", "untrusted/read-only")]))
        ),
        "a new family"
    );
    let mut write = g.clone();
    write.max_modes = modes(&[("codex", "never/read-only")]);
    assert!(
        widens(&write, None, Some(&modes(&[("codex", CODEX_WRITE)]))),
        "incomparable codex, the other way"
    );
}

#[tokio::test]
async fn absent_fields_keep_the_stored_values_and_do_not_widen() {
    let g = grant(&spawner()).await;
    let got = spawn_caps(&g, None, None).expect("accepted");
    assert_eq!(
        (got.spawn_cap, got.max_modes, got.widens),
        (3, g.max_modes.clone(), false)
    );
    let got = spawn_caps(&g, Some(1), None).expect("accepted");
    assert_eq!(got.max_modes.len(), 2);
    let got = spawn_caps(&g, None, Some(&modes(&[("claude", "plan")]))).expect("accepted");
    assert_eq!(
        (got.spawn_cap, got.max_modes),
        (3, modes(&[("claude", "plan")]))
    );
}
