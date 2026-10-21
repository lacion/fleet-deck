#![allow(clippy::unwrap_used, clippy::expect_used)]

//! MCP P4b §7.1 and §7.2 (test 10, the outbox part): a narrowing or a revoke
//! of a grant stops its fleet commands at the deck's answer to the station's
//! pre-execution check, and fails its pending outbox items. The land and
//! move parts are in `session_moves_narrowing_tests.rs`.

use fleetdeck_proto::command::DeckCommandState;
use fleetdeck_proto::mutation::{AuthorityCheck, LaunchMode};
use rusqlite::params;

use super::*;
use crate::store::NewCommand;
use crate::store::coordinator::grant_narrowing::{Removal, fail_removed_tx, required_hold};
use crate::store::outbox::OutboxState;

/// Writes `tokens.classes` of the grant (the change route is P4b-3b).
async fn set_classes(w: &World, grant_id: &str, classes: &[GrantClass]) {
    let text = serde_json::to_string(&classes.iter().copied().collect::<BTreeSet<_>>()).unwrap();
    let id = grant_id.to_owned();
    w.s.db()
        .call(move |conn| {
            conn.execute(
                "UPDATE tokens SET classes = ?2 WHERE grant_id = ?1",
                params![id, text],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// `ALL` without the classes of `gone`.
fn without(gone: &[GrantClass]) -> Vec<GrantClass> {
    ALL.iter().copied().filter(|c| !gone.contains(c)).collect()
}

/// The stored `required_classes` of an outbox item.
async fn required_of(w: &World, outbox_id: &str) -> Option<String> {
    let id = outbox_id.to_owned();
    w.s.db()
        .call(move |conn| {
            Ok(conn.query_row(
                "SELECT required_classes FROM outbox WHERE outbox_id = ?1",
                [id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

/// The outbox item that the call `call_id` made.
async fn item_of(w: &World, call_id: &str) -> OutboxItem {
    let all = w.s.outbox().await.unwrap();
    all.into_iter()
        .find(|i| i.call_id.as_deref() == Some(call_id))
        .unwrap()
}

/// Starts one attempt of `item`: its command is `pending`, the item `sending`.
async fn send(w: &World, item: &OutboxItem) -> CommandId {
    let id = CommandId::from(format!("c-{}", item.outbox_id));
    let cmd = NewCommand {
        author: None,
        command_id: id.clone(),
        station_id: w.alpha.clone(),
        method: "session.command".to_owned(),
        origin: "coordinator".to_owned(),
        envelope: json!({}),
        retry_until: item.expires_at,
    };
    assert!(
        w.s.start_attempt(item.outbox_id.clone(), cmd)
            .await
            .unwrap()
            .is_some()
    );
    id
}

/// The deck's answer to the station's check of the command `id`.
async fn verdict(
    w: &World,
    b: &CoordinatorBinding,
    id: &CommandId,
    launch: Option<LaunchMode>,
) -> Result<(), &'static str> {
    let check = AuthorityCheck {
        grant_id: b.grant_id.clone(),
        generation: 1,
        caller_station: w.alpha.clone(),
        caller_session: "coord-1".into(),
        target_session: "w1".into(),
        command_id: id.clone(),
        launch,
    };
    let local = w.s.authority_local(w.alpha.clone(), check).await.unwrap()?;
    assert!(!local.target_excluded);
    Ok(())
}

/// Runs `fail_removed_tx` in its own transaction.
async fn fail_removed(w: &World, grant_id: &str, classes: &[GrantClass]) {
    let id = grant_id.to_owned();
    let classes: BTreeSet<GrantClass> = classes.iter().copied().collect();
    let now = w.s.now_ms();
    crate::store::call(&w.s.db(), move |conn| {
        let tx = conn.transaction()?;
        fail_removed_tx(&tx, &id, &classes, Removal::Narrowed, now)?;
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
}

async fn state_of(w: &World, outbox_id: &str) -> (OutboxState, Option<String>) {
    let item =
        w.s.outbox_item(outbox_id.to_owned())
            .await
            .unwrap()
            .unwrap();
    (item.state, item.reason)
}

fn claude_default() -> Option<LaunchMode> {
    Some(LaunchMode {
        provider: ProviderKind::Claude,
        mode: None,
    })
}

/// MCP P4b test 10, the outbox part: the station's check, the compound
/// `project_start` set, the `NULL` of an item before V46, `fail_removed_tx`
/// and a revoke.
#[tokio::test]
async fn a_narrowing_or_a_revoke_stops_fleet_commands_before_execution() {
    let w = world().await;
    let plain = grant_of(&w, "coord-2", &[GrantClass::Mail], |_| {}).await;
    let b = grant_of(&w, "coord-1", &ALL, |g| g.scope = GrantScope::Fleet).await;
    let project = ProjectInput {
        name: "p".to_owned(),
        station: w.alpha.0.clone(),
        path: "/w/app".to_owned(),
        forge: None,
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    };
    let pa =
        w.s.create_project(w.alpha.0.clone(), project)
            .await
            .unwrap();
    let rename = |t: &str| CoordinatorOp::Rename {
        station: w.alpha.clone(),
        session_id: "w1".into(),
        title: t.to_owned(),
    };
    let start = CoordinatorOp::ProjectStart {
        project_id: pa.project_id.clone(),
        prompt: "do it".to_owned(),
        provider_instance: None,
        worktree: false,
    };
    // Six calls: two sent `manage` commands, a queued `manage` command, a
    // sent and a queued `mail` command of the fleet grant, and a mail of a
    // grant without fleet control.
    for (id, op) in [
        ("rn", rename("a")),
        ("st", start),
        ("rq", rename("b")),
        ("ml", mail(&w, "w1")),
        ("nl", mail(&w, "w1")),
    ] {
        let r = call(&w, &b, id, op).await;
        assert!(code(&r).is_none(), "{id}: {r:?}");
    }
    assert!(code(&call(&w, &plain, "pm", mail(&w, "w1")).await).is_none());
    let (rn, st, rq, ml, nl, pm) = (
        item_of(&w, "rn").await,
        item_of(&w, "st").await,
        item_of(&w, "rq").await,
        item_of(&w, "ml").await,
        item_of(&w, "nl").await,
        item_of(&w, "pm").await,
    );
    // The admitting call stores the full class set, in the order of
    // `tokens.classes`; an item without authority stores none.
    assert_eq!(
        required_of(&w, &rn.outbox_id).await.as_deref(),
        Some(r#"["manage"]"#)
    );
    assert_eq!(
        required_of(&w, &st.outbox_id).await.as_deref(),
        Some(r#"["spawn","manage"]"#)
    );
    assert_eq!(
        required_of(&w, &ml.outbox_id).await.as_deref(),
        Some(r#"["mail"]"#)
    );
    assert!(pm.authority.is_none());
    assert_eq!(required_of(&w, &pm.outbox_id).await, None);
    // An item admitted before V46 has no set.
    let nl_id = nl.outbox_id.clone();
    w.s.db()
        .call(move |conn| {
            conn.execute(
                "UPDATE outbox SET required_classes = NULL WHERE outbox_id = ?1",
                [nl_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (rn_cmd, st_cmd, nl_cmd, ml_cmd) = (
        send(&w, &rn).await,
        send(&w, &st).await,
        send(&w, &nl).await,
        send(&w, &ml).await,
    );
    // Before a narrowing every check passes.
    assert_eq!(verdict(&w, &b, &rn_cmd, None).await, Ok(()));
    assert_eq!(verdict(&w, &b, &st_cmd, claude_default()).await, Ok(()));
    assert_eq!(verdict(&w, &b, &nl_cmd, None).await, Ok(()));

    // `manage` goes while `inspect` stays: both commands that needed it fail,
    // the pre-V46 item is checked as today.
    set_classes(&w, &b.grant_id, &without(&[GrantClass::Manage])).await;
    assert_eq!(verdict(&w, &b, &rn_cmd, None).await, Err("class_removed"));
    assert_eq!(
        verdict(&w, &b, &st_cmd, claude_default()).await,
        Err("class_removed")
    );
    assert_eq!(verdict(&w, &b, &nl_cmd, None).await, Ok(()));
    // `project_start` needs `spawn` and `manage`: either one is enough to fail.
    set_classes(&w, &b.grant_id, &without(&[GrantClass::Spawn])).await;
    assert_eq!(verdict(&w, &b, &rn_cmd, None).await, Ok(()));
    assert_eq!(
        verdict(&w, &b, &st_cmd, claude_default()).await,
        Err("class_removed")
    );
    set_classes(
        &w,
        &b.grant_id,
        &without(&[GrantClass::Spawn, GrantClass::Manage]),
    )
    .await;
    assert_eq!(
        verdict(&w, &b, &st_cmd, claude_default()).await,
        Err("class_removed")
    );
    // Both stay (`inspect` and the rest were never touched): allowed.
    set_classes(&w, &b.grant_id, &ALL).await;
    assert_eq!(verdict(&w, &b, &rn_cmd, None).await, Ok(()));
    assert_eq!(verdict(&w, &b, &st_cmd, claude_default()).await, Ok(()));

    // A narrowing fails the queued and the sent items that lost a class, and
    // their pending commands; nothing else changes.
    let narrowed = without(&[GrantClass::Manage]);
    fail_removed(&w, &b.grant_id, &narrowed).await;
    let removed = (OutboxState::Dead, Some("authority_removed".to_owned()));
    for item in [&rn, &st, &rq] {
        assert_eq!(state_of(&w, &item.outbox_id).await, removed);
    }
    for (command, expect) in [(&rn_cmd, true), (&st_cmd, true), (&nl_cmd, false)] {
        let c = w.s.command(command.clone()).await.unwrap().unwrap();
        assert_eq!(
            c.state == DeckCommandState::Failed,
            expect,
            "{}",
            command.as_str()
        );
        if expect {
            assert_eq!(c.reason.as_deref(), Some("authority_removed"));
        }
    }
    // Intact classes, no set (before V46) and no authority are untouched.
    assert_eq!(state_of(&w, &ml.outbox_id).await.0, OutboxState::Sending);
    assert_eq!(state_of(&w, &nl.outbox_id).await.0, OutboxState::Sending);
    assert_eq!(state_of(&w, &pm.outbox_id).await.0, OutboxState::Queued);

    // A revoke allows nothing: every pending fleet item of the grant fails
    // with the reason of the existing gates (`authority_lost`)
    // (the pre-V46 item has no set and keeps its state), the mail of a grant
    // without fleet control stays queued.
    assert!(
        w.s.revoke_grant(b.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    let lost = (OutboxState::Dead, Some("authority_lost".to_owned()));
    assert_eq!(state_of(&w, &ml.outbox_id).await, lost);
    let c = w.s.command(ml_cmd.clone()).await.unwrap().unwrap();
    assert_eq!(c.reason.as_deref(), Some("authority_lost"));
    assert_eq!(state_of(&w, &nl.outbox_id).await.0, OutboxState::Sending);
    assert_eq!(state_of(&w, &pm.outbox_id).await.0, OutboxState::Queued);
    // A revoke of a grant that is revoked already changes nothing again.
    assert!(
        !w.s.revoke_grant(b.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
}

/// MCP P4b test 14, the deck part: an upgraded database has
/// `session_owner_grant` and the three `required_classes` columns; the rows
/// of V45 read `NULL`.
#[test]
fn the_v46_migration_adds_the_table_and_the_columns_and_old_rows_read_null() {
    use rusqlite::Connection;
    let migrations = crate::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 45).unwrap();
    let has = |conn: &Connection, table: &str, column: &str| -> bool {
        let sql = format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1");
        conn.query_row(&sql, [column], |r| r.get::<_, i64>(0))
            .unwrap()
            == 1
    };
    for table in ["outbox", "land_queue", "session_moves"] {
        assert!(!has(&conn, table, "required_classes"), "{table}");
    }
    conn.execute(
        "INSERT INTO outbox (outbox_id, station_id, kind, origin, payload, state, next_at,
             expires_at, created_at, updated_at, authority)
         VALUES ('o1', 'st', 'command', 'coordinator', '{}', 'queued', 1, 2, 1, 1, '{}')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO land_queue (land_id, command_id, requested_by, request_command_id,
             authority, project_id, lock_key, station_id, session_id, state, created_at)
         VALUES ('l1', 'lc1', 'coordinator:g', 'k1', '{}', 'p', 'r', 'st', 's', 'queued', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_moves (move_id, request_command_id, src_station, src_session,
             dst_station, kind, state, out_command_id, in_command_id, finish_command_id,
             requested_by, authority, created_at)
         VALUES ('m1', 'k1', 'a', 's1', 'b', 'native', 'holding', 'o1', 'i1', 'f1',
                 'coordinator:g', '{}', 1)",
        [],
    )
    .unwrap();
    migrations.to_latest(&mut conn).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 47);
    for table in ["outbox", "land_queue", "session_moves"] {
        assert!(has(&conn, table, "required_classes"), "{table}");
        let value: Option<String> = conn
            .query_row(&format!("SELECT required_classes FROM {table}"), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(value, None, "{table}");
    }
    for column in ["station_id", "session_id", "grant_id", "seq", "station_seq"] {
        assert!(has(&conn, "session_owner_grant", column), "{column}");
    }
    // One desired grant for each session; `station_seq` starts at 0.
    let put = |grant: &str| {
        conn.execute(
            "INSERT INTO session_owner_grant (station_id, session_id, grant_id, seq)
             VALUES ('st', 's', ?1, 1)",
            [grant],
        )
    };
    put("g1").unwrap();
    assert!(put("g2").is_err());
    let station_seq: i64 = conn
        .query_row("SELECT station_seq FROM session_owner_grant", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(station_seq, 0);
}

/// The rule of the check: no set holds, a set within the grant holds, a set
/// that needs a class the grant lost or that does not parse does not.
#[test]
fn a_class_set_holds_only_when_every_class_is_still_in_the_grant() {
    let grant: BTreeSet<GrantClass> =
        [GrantClass::Spawn, GrantClass::Manage, GrantClass::Inspect].into();
    assert!(required_hold(None, &grant));
    assert!(required_hold(Some("[]"), &BTreeSet::new()));
    assert!(required_hold(Some(r#"["spawn","manage"]"#), &grant));
    assert!(!required_hold(Some(r#"["spawn","manage","mail"]"#), &grant));
    assert!(!required_hold(Some(r#"["manage"]"#), &BTreeSet::new()));
    assert!(!required_hold(Some("not json"), &grant));
    assert!(!required_hold(Some(r#"["nonsense"]"#), &grant));
}

/// PRE-PR R1 BLOCKER 1: a sent command that a narrowing canceled stays
/// canceled when the class comes back before the station's final check.
#[tokio::test]
async fn a_canceled_command_stays_canceled_when_its_class_comes_back() {
    let w = world().await;
    let b = grant_of(&w, "coord-1", &ALL, |g| g.scope = GrantScope::Fleet).await;
    let rename = CoordinatorOp::Rename {
        station: w.alpha.clone(),
        session_id: "w1".into(),
        title: "a".to_owned(),
    };
    assert!(code(&call(&w, &b, "rn", rename).await).is_none());
    let item = item_of(&w, "rn").await;
    let cmd = send(&w, &item).await;
    assert_eq!(verdict(&w, &b, &cmd, None).await, Ok(()));
    // The narrowing cancels the sent item.
    set_classes(&w, &b.grant_id, &without(&[GrantClass::Manage])).await;
    fail_removed(&w, &b.grant_id, &without(&[GrantClass::Manage])).await;
    assert_eq!(state_of(&w, &item.outbox_id).await.0, OutboxState::Dead);
    // `manage` comes back before the final check: the item stays canceled.
    set_classes(&w, &b.grant_id, &ALL).await;
    assert_eq!(verdict(&w, &b, &cmd, None).await, Err("command_canceled"));
}
