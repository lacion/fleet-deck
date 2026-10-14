//! The history retention (NV-26) and the native session ids (NV-23).

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_testkit::Det;

use super::tests::{DAY, new_session, store};
use super::*;

// --- history retention (NV-26) and native ids (NV-23) ----------------------------

async fn column(s: &Store, sql: &'static str) -> Vec<String> {
    call(&s.inner.db, move |conn| {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
    .unwrap()
}

fn started(refs: &[(&str, &str)]) -> NewEvent {
    let mut provider_refs = fleetdeck_proto::event::ProviderRefs::new();
    for (k, v) in refs {
        provider_refs.insert((*k).to_owned(), serde_json::Value::from(*v));
    }
    Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
        resumed: None,
        provider_refs,
        title: None,
        branch: None,
        repo: None,
    }
    .into()
}

async fn native_of(s: &Store, session: &'static str) -> Option<String> {
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT native_session_id FROM sessions WHERE session_id = ?1",
            [session],
            |r| r.get::<_, Option<String>>(0),
        )?)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_native_session_id_that_is_not_valid_is_not_stored() {
    let det = Det::new();
    let s = store(&det).await;
    for id in ["s1", "s2", "s3"] {
        s.create_session(new_session(id)).await.unwrap();
    }
    s.append("s1".into(), vec![started(&[("session_id", "-c x")])])
        .await
        .unwrap();
    s.append("s2".into(), vec![started(&[("session_id", "abc-1")])])
        .await
        .unwrap();
    s.append("s3".into(), vec![started(&[("thread_id", "--yolo")])])
        .await
        .unwrap();
    assert_eq!(native_of(&s, "s1").await, None);
    assert_eq!(native_of(&s, "s2").await.as_deref(), Some("abc-1"));
    assert_eq!(native_of(&s, "s3").await, None);
}

/// Seeds one old row, or one young row, in each history table. `s1` holds rows
/// that go; `s2` has a claimed effect; `s3` has an `outcome_unknown` effect and
/// its dead process; `s4` has a live process; `s5` has a shim.
async fn seed_history(s: &Store, young: i64) {
    for id in ["s1", "s2", "s3", "s4", "s5"] {
        s.create_session(new_session(id)).await.unwrap();
    }
    s.shim_starting(NewShim {
        shim_id: "shim-1".to_owned(),
        session_id: "s5".into(),
        socket_path: std::path::PathBuf::from("/run/x/shim-1.sock"),
        launch_boot_id: None,
        native_target: None,
    })
    .await
    .unwrap();
    call(&s.inner.db, move |conn| {
        let old = 1_000_i64;
        conn.execute_batch(&format!(
            "
            INSERT INTO processes (process_incarnation, session_id, state, started_at, ended_at)
            VALUES ('p-old', 's1', 'dead', {old}, {old}),
                   ('p-young', 's1', 'dead', {young}, {young}),
                   ('p-kept', 's3', 'dead', {old}, {old}),
                   ('p-live', 's4', 'live', {old}, NULL);
            INSERT INTO effects (effect_id, session_id, process_incarnation, kind, payload, state,
                                 created_at, updated_at)
            VALUES ('e-confirmed', 's1', NULL, 'k', '{{}}', 'confirmed', {old}, {old}),
                   ('e-retired', 's1', NULL, 'k', '{{}}', 'retired_not_dispatched', {old}, {old}),
                   ('e-young', 's1', NULL, 'k', '{{}}', 'confirmed', {young}, {young}),
                   ('e-claimed', 's2', NULL, 'k', '{{}}', 'claimed', {old}, {old}),
                   ('e-unknown', 's3', 'p-kept', 'k', '{{}}', 'outcome_unknown', {old}, {old}),
                   ('e-write', 's5', NULL, 'k', '{{}}', 'confirmed', {old}, {old}),
                   ('e-trans', 's5', NULL, 'k', '{{}}', 'confirmed', {old}, {old}),
                   ('e-queued', 's5', NULL, 'k', '{{}}', 'queued', {old}, {old});
            INSERT INTO shim_writes (shim_id, write_seq, write_id, effect_id, ordinal, generation,
                                     created_at)
            VALUES ('shim-1', 1, 'w1', 'e-write', 0, 1, {old});
            INSERT INTO shim_transitions (shim_id, seq, effect_id, frames, created_at)
            VALUES ('shim-1', 1, 'e-trans', 1, {old});
            INSERT INTO runtime_requests (request_id, session_id, process_incarnation, kind,
                                          options, response_channel, state, requested_at, updated_at)
            VALUES ('r-resolved', 's1', 'p-old', 'k', '[]', 'c', 'resolved', {old}, {old}),
                   ('r-expired', 's1', NULL, 'k', '[]', 'c', 'expired', {old}, {old}),
                   ('r-young', 's1', 'p-young', 'k', '[]', 'c', 'resolved', {young}, {young}),
                   ('r-open', 's1', NULL, 'k', '[]', 'c', 'open', {old}, {old}),
                   ('r-claimed', 's2', NULL, 'k', '[]', 'c', 'resolved', {old}, {old}),
                   ('r-live', 's4', 'p-live', 'k', '[]', 'c', 'resolved', {old}, {old});
            INSERT INTO shell_effects (effect_id, command_id, shell_id, op, change, state,
                                       created_at, updated_at)
            VALUES ('x-confirmed', 'c1', 'sh', 'open', '{{}}', 'confirmed', {old}, {old}),
                   ('x-failed', 'c2', 'sh', 'open', '{{}}', 'failed', {old}, {old}),
                   ('x-unknown', 'c3', 'sh', 'open', '{{}}', 'outcome_unknown', {old}, {old}),
                   ('x-queued', 'c4', 'sh', 'open', '{{}}', 'queued', {old}, {old}),
                   ('x-young', 'c5', 'sh', 'open', '{{}}', 'confirmed', {young}, {young}),
                   ('x-handover', 'c6', 'sh', 'handover', '{{}}', 'outcome_unknown', {old}, {old});
            INSERT INTO coordinator_outbox (command_id, binding_id, generation, args_digest,
                                            envelope, state, retry_until, created_at, updated_at)
            VALUES ('o-done', 'b', 1, 'd', '{{}}', 'done', {old}, {old}, {old}),
                   ('o-failed', 'b', 1, 'd', '{{}}', 'failed', {old}, {old}, {old}),
                   ('o-ambiguous', 'b', 1, 'd', '{{}}', 'ambiguous', {old}, {old}, {old}),
                   ('o-pending', 'b', 1, 'd', '{{}}', 'pending', {old}, {old}, {old}),
                   ('o-young', 'b', 1, 'd', '{{}}', 'done', {young}, {young}, {young});
            INSERT INTO artifact_suggestions (session_id, path, at)
            VALUES ('s1', 'old.md', {old}), ('s1', 'young.md', {young});
            "
        ))?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn the_prune_removes_finished_history_rows_older_than_the_history_retention() {
    let det = Det::new();
    let s = store(&det).await;
    let young = s.now_ms();
    seed_history(&s, young).await;
    let report = s.prune().await.unwrap();
    // Outbox 3, shell effects 3, effects 2, requests 2, process 1, suggestion 1.
    assert_eq!(report.history, 12);
    assert_eq!(
        column(&s, "SELECT command_id FROM coordinator_outbox ORDER BY 1").await,
        ["o-pending", "o-young"]
    );
    assert_eq!(
        column(&s, "SELECT effect_id FROM shell_effects ORDER BY 1").await,
        ["x-handover", "x-queued", "x-young"]
    );
    // An `outcome_unknown` effect, an open effect and an effect that a shim row
    // names stay.
    assert_eq!(
        column(&s, "SELECT effect_id FROM effects ORDER BY 1").await,
        [
            "e-claimed",
            "e-queued",
            "e-trans",
            "e-unknown",
            "e-write",
            "e-young"
        ]
    );
    // A request of a session with a claimed effect, and one of a live process, stay.
    assert_eq!(
        column(&s, "SELECT request_id FROM runtime_requests ORDER BY 1").await,
        ["r-claimed", "r-live", "r-open", "r-young"]
    );
    // A dead process that a kept effect names stays.
    assert_eq!(
        column(&s, "SELECT process_incarnation FROM processes ORDER BY 1").await,
        ["p-kept", "p-live", "p-young"]
    );
    assert_eq!(
        column(&s, "SELECT path FROM artifact_suggestions ORDER BY 1").await,
        ["young.md"]
    );
    // The tables that the prune never touches keep their rows.
    assert_eq!(column(&s, "SELECT shim_id FROM shims").await, ["shim-1"]);
    assert_eq!(
        column(&s, "SELECT session_id FROM sessions ORDER BY 1").await,
        ["s1", "s2", "s3", "s4", "s5"]
    );
}

#[tokio::test]
async fn a_second_history_prune_has_nothing_left_to_remove() {
    let det = Det::new();
    let s = store(&det).await;
    let young = s.now_ms();
    seed_history(&s, young).await;
    assert_eq!(s.prune().await.unwrap().history, 12);
    assert_eq!(s.prune().await.unwrap().history, 0);
}

#[tokio::test]
async fn a_young_history_row_goes_when_the_history_retention_passes() {
    let det = Det::new();
    let s = store(&det).await;
    let young = s.now_ms();
    seed_history(&s, young).await;
    s.prune().await.unwrap();
    det.advance(91 * DAY);
    // Young outbox 1, shell effect 1, effect 1, request 1, process 1, suggestion 1.
    assert_eq!(s.prune().await.unwrap().history, 6);
    assert_eq!(
        column(&s, "SELECT effect_id FROM effects ORDER BY 1").await,
        ["e-claimed", "e-queued", "e-trans", "e-unknown", "e-write"]
    );
    assert_eq!(
        column(&s, "SELECT process_incarnation FROM processes ORDER BY 1").await,
        ["p-kept", "p-live"]
    );
    assert_eq!(
        column(&s, "SELECT request_id FROM runtime_requests ORDER BY 1").await,
        ["r-claimed", "r-live", "r-open"]
    );
    assert_eq!(
        column(&s, "SELECT command_id FROM coordinator_outbox ORDER BY 1").await,
        ["o-pending"]
    );
    assert_eq!(
        column(&s, "SELECT effect_id FROM shell_effects ORDER BY 1").await,
        ["x-handover", "x-queued"]
    );
}
