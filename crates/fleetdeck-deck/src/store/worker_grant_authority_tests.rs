//! MCP P4a deck tests 11, 12 and 13 and the equality of the core copy of
//! `mode_within`: rule 3, the confinement of a worker's authority and the
//! migrations. They share the world of `worker_grant_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

/// Binds the worker grant of start `id` to `session`.
async fn bind_worker(w: &World, id: &str, session: &str, seq: u64) -> (Grant, CoordinatorBinding) {
    let g = grants_by(w)
        .await
        .into_iter()
        .find(|(c, _)| c == id)
        .unwrap()
        .1;
    let e = Event::CoordinatorBound {
        binding_id: format!("b-{session}"),
        grant_id: g.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: format!("native-{session}"),
    };
    apply(w, &w.a, seq, session, e).await;
    let g = w.s.grant(g.grant_id).await.unwrap().unwrap();
    let b = g.binding.clone().expect("bound");
    (g, b)
}

async fn call_as(w: &World, b: &CoordinatorBinding, id: &str, op: CoordinatorOp) -> Receipt {
    let envelope = CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: w.s.incarnation().clone(),
        retry_until: w.s.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: b.native_id.strip_prefix("native-").unwrap().into(),
            binding: b.clone(),
            idempotency_key: None,
            op,
        },
    };
    w.s.decide_coordinator(w.a.clone(), envelope).await.unwrap()
}

fn code(r: &Receipt) -> Option<RejectCode> {
    r.reject.as_ref().map(|j| j.code)
}

fn fleet_grant(w: &World, session: &str) -> NewGrant {
    let mut g = NewGrant::new(w.a.clone(), session.into());
    g.classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
        GrantClass::Inspect,
        GrantClass::Manage,
        GrantClass::Answer,
    ]);
    g.scope = GrantScope::Fleet;
    g.spawn_cap = 5;
    g.max_modes = [("claude".to_owned(), "acceptEdits".to_owned())].into();
    g
}

async fn fleet_binding(w: &World, session: &str) -> CoordinatorBinding {
    let stored = w.s.create_grant(fleet_grant(w, session)).await.unwrap();
    let b = CoordinatorBinding {
        binding_id: format!("b-{session}"),
        grant_id: stored.grant_id,
        generation: 1,
        provider_instance: "claude".into(),
        native_id: format!("native-{session}"),
    };
    assert!(w.s.bind_grant(b.clone()).await.unwrap());
    b
}

/// Test 11: a fleet coordinator may act on a session that holds a worker
/// grant, and never on a session that holds an owner grant.
#[tokio::test]
async fn rule_3_ignores_worker_grants_and_keeps_owner_grants() {
    let w = world().await;
    submit(&w, "k1", &on()).await;
    let (_, _) = bind_worker(&w, "k1", "worker-1", 1).await;
    let owner =
        w.s.create_grant(NewGrant::new(w.a.clone(), "owner-1".into()))
            .await
            .unwrap();
    w.s.bind_grant(CoordinatorBinding {
        binding_id: "b-owner".to_owned(),
        grant_id: owner.grant_id,
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-owner-1".to_owned(),
    })
    .await
    .unwrap();
    let fleet = fleet_binding(&w, "coord-1").await;
    let interrupt = |s: &str| CoordinatorOp::Interrupt {
        station: w.a.clone(),
        session_id: s.into(),
    };
    let r = call_as(&w, &fleet, "k-ok", interrupt("worker-1")).await;
    assert!(code(&r).is_none(), "{r:?}");
    let r = call_as(&w, &fleet, "k-no", interrupt("owner-1")).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    assert!(r.reject.unwrap().message.contains("coordinator"));
    // The station-facing flag still says that a worker binding is bound.
    let g = caller(&w, "worker-1").await;
    assert!(g.binding.is_some());
}

/// Test 12: a worker's authority stays inside its grant.
#[tokio::test]
async fn fleet_control_approval_of_a_worker_spawn_stays_inside_the_worker_grant() {
    let w = world().await;
    let narrow = WorkerGrantConfig {
        stations: vec!["alpha".to_owned()],
        spawn_cap: 1,
        ..on()
    };
    submit(&w, "w12", &narrow).await;
    let (g, wb) = bind_worker(&w, "w12", "worker-1", 1).await;
    assert_eq!(g.stations, BTreeSet::from([w.a.clone()]));
    let spawn = |station: &StationId, mode: Option<&str>| CoordinatorOp::Spawn {
        station: station.clone(),
        spec: SessionSpec {
            effort: None,
            provider_instance: "claude".into(),
            cwd: "/w/app".to_owned(),
            mode: mode.map(str::to_owned),
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        },
        prompt: Some(UserInput::new("do")),
    };
    // The ceiling and the grant's stations hold.
    let r = call_as(&w, &wb, "sp-auto", spawn(&w.a, Some("auto"))).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    let r = call_as(&w, &wb, "sp-beta", spawn(&w.b, None)).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    // One spawn charges the worker's cap and the child gets no grant.
    let r = call_as(&w, &wb, "sp-1", spawn(&w.a, Some("acceptEdits"))).await;
    assert!(code(&r).is_none(), "{r:?}");
    let r = call_as(&w, &wb, "sp-2", spawn(&w.a, None)).await;
    assert_eq!(code(&r), Some(RejectCode::SpawnCapReached), "{r:?}");
    let g = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(g.spawn_used, 1);
    let starts = |items: Vec<OutboxItem>| -> Vec<SessionCommand> {
        items
            .into_iter()
            .filter_map(|i| match serde_json::from_value(i.payload) {
                Ok(CommandKind::Session { command }) => Some(command),
                _ => None,
            })
            .filter(|c| matches!(c, SessionCommand::Start { .. }))
            .collect()
    };
    let child = starts(w.s.outbox().await.unwrap());
    assert_eq!(child.len(), 1);
    assert!(matches!(&child[0], SessionCommand::Start { spec, .. } if spec.coordinator.is_none()));
    // A fleet coordinator acts on the worker with its own grant: its spawns
    // charge its own cap, and the child has no grant either.
    let fleet = fleet_binding(&w, "coord-1").await;
    let assign = CoordinatorOp::AssignAuto {
        task: "t".to_owned(),
        requirements: fleetdeck_proto::mutation::AssignRequirements::default(),
    };
    let r = call_as(&w, &fleet, "fa-1", assign).await;
    assert!(code(&r).is_none(), "{r:?}");
    let fleet_grant = caller(&w, "coord-1").await;
    let worker_after = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(
        worker_after.spawn_used, 1,
        "the worker's cap is not charged"
    );
    for c in starts(w.s.outbox().await.unwrap()) {
        assert!(matches!(&c, SessionCommand::Start { spec, .. } if spec.coordinator.is_none()));
    }
    assert!(fleet_grant.spawn_used <= fleet_grant.spawn_cap);
    // An owner grant's session stays a refused target.
    let owner =
        w.s.create_grant(NewGrant::new(w.a.clone(), "owner-1".into()))
            .await
            .unwrap();
    w.s.bind_grant(CoordinatorBinding {
        binding_id: "b-o".to_owned(),
        grant_id: owner.grant_id,
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-owner-1".to_owned(),
    })
    .await
    .unwrap();
    let r = call_as(
        &w,
        &fleet,
        "fa-owner2",
        CoordinatorOp::Interrupt {
            station: w.a.clone(),
            session_id: "owner-1".into(),
        },
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    // After the revocation the worker's calls are refused.
    assert!(
        w.s.revoke_grant(g.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    let r = call_as(&w, &wb, "sp-3", spawn(&w.a, None)).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
}

/// The core copy of `mode_within` equals the deck's for every pair.
#[test]
fn the_core_copy_of_mode_within_equals_the_deck_function() {
    use fleetdeck_core::config_worker_grant as core;
    let claude: Vec<&str> = vec![
        "default",
        "plan",
        "acceptEdits",
        "dontAsk",
        "auto",
        "bypassPermissions",
        "junk",
    ];
    for a in &claude {
        for b in &claude {
            assert_eq!(
                mode_within("claude", Some(a), b),
                core::mode_within("claude", Some(a), b),
                "{a} {b}"
            );
        }
        assert_eq!(
            mode_within("claude", None, a),
            core::mode_within("claude", None, a)
        );
    }
    let policies = ["untrusted", "on-request", "never", "x"];
    let sandboxes = ["read-only", "workspace-write", "danger-full-access", "y"];
    let mut codex: Vec<String> = Vec::new();
    for p in policies {
        for s in sandboxes {
            codex.push(format!("{p}/{s}"));
        }
    }
    codex.push("junk".to_owned());
    for a in &codex {
        for b in &codex {
            assert_eq!(
                mode_within("codex", Some(a), b),
                core::mode_within("codex", Some(a), b),
                "{a} {b}"
            );
        }
        assert_eq!(
            mode_within("codex", None, a),
            core::mode_within("codex", None, a)
        );
    }
    for family in ["claude", "codex", "other"] {
        assert_eq!(
            mode_within(family, Some("plan"), "plan"),
            core::mode_within(family, Some("plan"), "plan")
        );
    }
    // The registry options are exactly the valid ceilings.
    for m in core::CLAUDE_CEILINGS {
        assert!(crate::store::coordinator_ops::valid_ceiling("claude", m));
    }
    for m in core::CODEX_CEILINGS {
        assert!(crate::store::coordinator_ops::valid_ceiling("codex", m));
    }
}

/// Test 13: an upgraded database.
#[test]
fn the_migrations_add_the_grant_kind_the_schedule_author_and_the_attach_table() {
    use rusqlite::Connection;
    let migrations = crate::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 42).unwrap();
    let has = |conn: &Connection, table: &str, column: &str| -> bool {
        let sql = format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1");
        conn.query_row(&sql, [column], |r| r.get::<_, i64>(0))
            .unwrap()
            == 1
    };
    assert!(!has(&conn, "tokens", "kind") && !has(&conn, "schedules", "author"));
    conn.execute(
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
             max_modes, idle_ttl_ms, created_at, expires_at, state)
         VALUES ('g1', 's', 'x', '[]', '[]', 0, '{}', 1000, 1, 2, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO schedules (schedule_id, name, enabled, cron, timezone, station_id, path,
             provider_instance, prompt, reuse_session, created_at, armed_at)
         VALUES ('sc1', 'n', 1, '* * * * *', '+00:00', 'st', '/w', 'claude', 'p', 0, 1, 1)",
        [],
    )
    .unwrap();
    migrations.to_latest(&mut conn).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 47);
    let kind: String = conn
        .query_row("SELECT kind FROM tokens WHERE grant_id = 'g1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, "coordinator");
    let author: String = conn
        .query_row(
            "SELECT author FROM schedules WHERE schedule_id = 'sc1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(author, "unknown");
    assert!(has(&conn, "worker_attach", "original_sha256"));
    // The checks of the columns and the table.
    assert!(
        conn.execute("UPDATE tokens SET kind = 'other'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE schedules SET author = 'x'", [])
            .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO worker_attach (command_id, original_sha256, decision, reason, created_at)
             VALUES ('c', 'h', 'maybe', 'r', 1)",
            [],
        )
        .is_err()
    );
    let index: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'worker_attach_by_created'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1);
}
