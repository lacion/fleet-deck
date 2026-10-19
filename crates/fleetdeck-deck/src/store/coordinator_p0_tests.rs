//! MCP P0 on the deck store (design §4 tests 6, 8, 9): the mode ceiling
//! values, a coordinator `spawn` that carries a coordinator grant, and the
//! grant scope and class rule of `command_status`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_proto::command::{CoordinatorSpec, ReceiptState};
use fleetdeck_proto::mutation::{CoordinatorOp, CoordinatorQuery};
use fleetdeck_proto::{CommandId, RejectCode};
use serde_json::json;

use super::tests::{ask, code, decide, env, grant, grant_with, query, spawn, spawn_used, world};
use crate::store::coordinator_ops::valid_ceiling;
use crate::store::grants::GrantClass;

/// Test 6: every ceiling of §2.2 rule 5, and the values that are not valid.
#[test]
fn valid_ceiling_takes_the_non_bypass_modes_of_a_known_family_only() {
    for mode in ["default", "plan", "acceptEdits", "dontAsk", "auto"] {
        assert!(valid_ceiling("claude", mode), "claude {mode}");
    }
    for policy in ["untrusted", "on-request", "never"] {
        for sandbox in ["read-only", "workspace-write", "danger-full-access"] {
            let ceiling = format!("{policy}/{sandbox}");
            let bypass = ceiling == "never/danger-full-access";
            assert_eq!(valid_ceiling("codex", &ceiling), !bypass, "codex {ceiling}");
        }
    }
    for (family, ceiling) in [
        ("claude", "bypassPermissions"),
        ("codex", "never/danger-full-access"),
        ("claude", "default "),
        ("codex", "untrusted"),
        ("codex", "on-request/full"),
        ("gemini", "default"),
        ("gemini", "untrusted/read-only"),
    ] {
        assert!(!valid_ceiling(family, ceiling), "{family} {ceiling}");
    }
}

fn coordinator_spec(extra: serde_json::Value) -> Box<CoordinatorSpec> {
    let mut spec = json!({"grant_id": "", "classes": ["spawn"], "idle_ttl_ms": 0,
        "expires_at": 0, "control_skill": {"version": "", "text": ""}});
    spec.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    Box::new(serde_json::from_value(spec).unwrap())
}

/// Test 8 (store part): a coordinator `spawn` whose spec has a coordinator
/// grant is a stored `forbidden`, and no start intent is written.
#[tokio::test]
async fn a_coordinator_spawn_with_a_coordinator_grant_is_a_stored_forbidden() {
    let w = world().await;
    let modes = [("claude", "acceptEdits")];
    let b = grant(&w, &[GrantClass::Spawn], 5, &modes).await;
    let specs = [
        json!({}),
        json!({"spawn_cap": 7, "max_modes": {"claude": "acceptEdits"}}),
    ];
    for (i, extra) in specs.into_iter().enumerate() {
        let mut op = spawn(&w.alpha, "claude", None);
        let CoordinatorOp::Spawn { spec, .. } = &mut op else {
            panic!("a spawn op");
        };
        spec.coordinator = Some(coordinator_spec(extra));
        let r = decide(&w, env(&w, &b, &format!("k{i}"), op)).await;
        assert_eq!(r.state, ReceiptState::Rejected, "{i}: {r:?}");
        assert_eq!(code(&r), Some(RejectCode::Forbidden), "{i}");
        assert_eq!(
            r.reject.unwrap().message,
            "a coordinator never starts a session with a coordinator grant",
            "{i}"
        );
    }
    // Both rejections are stored; nothing was started or counted.
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 2);
    assert_eq!(w.s.coordinator_starts().await.unwrap(), 0);
    assert_eq!(spawn_used(&w, &b).await, 0);
    assert!(w.s.outbox().await.unwrap().is_empty());
}

fn status(id: &str) -> CoordinatorQuery {
    CoordinatorQuery::CommandStatus {
        command_id: CommandId::from(id),
    }
}

/// Test 9 (scope): the status of a command belongs to the grant that made
/// it.
#[tokio::test]
async fn a_command_status_belongs_to_the_grant_that_made_the_command() {
    let w = world().await;
    let a = grant(&w, &[GrantClass::Spawn], 5, &[("claude", "default")]).await;
    let first = decide(&w, env(&w, &a, "kA", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(first.state, ReceiptState::Accepted, "{first:?}");
    // The grant A reads its own receipt while it is active.
    let v = ask(&w, query(&w, &a, status("kA"), 1000)).await.unwrap();
    assert_eq!(v["receipt"]["state"], "accepted");
    // Another session with its own grant gets no receipt.
    let other = grant_with(&w, &[GrantClass::Mail], 3, &[], |g| {
        g.session_id = "worker-1".into();
    })
    .await;
    let mut e = query(&w, &other, status("kA"), 1000);
    e.caller = "worker-1".into();
    let v = ask(&w, e).await.unwrap();
    assert!(v["receipt"].is_null(), "{v}");
    assert_eq!(v["deliveries"], json!([]));
    // A newer grant B of the same session gets no receipt of A either.
    w.det.advance(Duration::from_secs(1));
    let b = grant(&w, &[GrantClass::Mail], 3, &[]).await;
    let v = ask(&w, query(&w, &b, status("kA"), 1000)).await.unwrap();
    assert!(v["receipt"].is_null(), "{v}");
    assert_eq!(v["deliveries"], json!([]));
}

/// Test 9 (classes): the grants with only `read` or `inspect` are refused;
/// every mutating class alone is allowed.
#[tokio::test]
async fn a_command_status_needs_a_mutating_class() {
    let refused: [&[GrantClass]; 3] = [
        &[GrantClass::Read],
        &[GrantClass::Inspect],
        &[GrantClass::Read, GrantClass::Inspect],
    ];
    for classes in refused {
        let w = world().await;
        let b = grant(&w, classes, 3, &[]).await;
        let r = ask(&w, query(&w, &b, status("k9"), 1000)).await;
        assert_eq!(r, Err(RejectCode::Forbidden), "{classes:?}");
    }
    let allowed = [
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
        GrantClass::Manage,
        GrantClass::Answer,
        GrantClass::Desktop,
        GrantClass::Capture,
        GrantClass::Input,
    ];
    for class in allowed {
        let w = world().await;
        let b = grant(&w, &[class], 3, &[]).await;
        let v = ask(&w, query(&w, &b, status("k9"), 1000)).await;
        let v = v.unwrap_or_else(|e| panic!("{class:?}: {e:?}"));
        assert!(v["receipt"].is_null(), "{class:?}");
    }
}
