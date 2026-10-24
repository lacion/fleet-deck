//! Tests of the audit import of provider updates (B28 §3.11).

use fleetdeck_proto::cli_update::{CliUpdateAttempt, InstallMethod, UpdateOutcome, UpdateTrigger};
use fleetdeck_proto::event::ProviderKind;
use rusqlite::Connection;

use super::{AUDIT_ACTION, SEEN_KEEP, audit_detail, audit_result, cursor, import};
use crate::store::schema::migrations;

fn latest() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    migrations().to_latest(&mut conn).unwrap();
    conn
}

fn attempt(seq: u64, id: &str) -> CliUpdateAttempt {
    CliUpdateAttempt {
        seq,
        attempt_id: id.to_owned(),
        provider: ProviderKind::Codex,
        trigger: UpdateTrigger::Schedule,
        method: InstallMethod::Bun,
        channel: "stable".to_owned(),
        from_version: Some("0.50.0".to_owned()),
        target_version: Some("0.51.0".to_owned()),
        to_version: Some("0.51.0".to_owned()),
        outcome: UpdateOutcome::Updated,
        reason: None,
        started_at: 1,
        ended_at: Some(2),
    }
}

fn audit_rows(conn: &Connection) -> Vec<(String, String, String, String, String)> {
    let mut stmt = conn
        .prepare("SELECT actor, action, target, result, detail FROM audit ORDER BY audit_id")
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn an_import_writes_one_audit_row_for_each_new_attempt() {
    let mut conn = latest();
    let rows = [attempt(1, "a1"), attempt(2, "a2")];
    assert_eq!(
        import(&mut conn, "st", "Lighting", "A", &rows, 100).unwrap(),
        2
    );
    let got = audit_rows(&conn);
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        (
            "station:Lighting".to_owned(),
            AUDIT_ACTION.to_owned(),
            "Lighting/codex".to_owned(),
            "ok".to_owned(),
            "schedule bun 0.50.0 -> 0.51.0: updated".to_owned(),
        )
    );
    assert_eq!(cursor(&conn, "st").unwrap(), Some(("A".to_owned(), 2)));
}

#[test]
fn the_same_rows_a_second_time_write_nothing() {
    let mut conn = latest();
    let rows = [attempt(1, "a1"), attempt(2, "a2")];
    import(&mut conn, "st", "Lighting", "A", &rows, 100).unwrap();
    assert_eq!(
        import(&mut conn, "st", "Lighting", "A", &rows, 200).unwrap(),
        0
    );
    assert_eq!(count(&conn, "audit"), 2);
    assert_eq!(cursor(&conn, "st").unwrap(), Some(("A".to_owned(), 2)));
}

#[test]
fn a_new_incarnation_restarts_at_zero_and_skips_the_attempts_it_saw() {
    let mut conn = latest();
    let first: Vec<_> = (1..=10).map(|n| attempt(n, &format!("a{n}"))).collect();
    import(&mut conn, "st", "Lighting", "A", &first, 100).unwrap();
    assert_eq!(cursor(&conn, "st").unwrap(), Some(("A".to_owned(), 10)));
    // A restored station: a lower `seq`; `a1` was imported before.
    let restored = [attempt(1, "a1"), attempt(2, "b2"), attempt(3, "b3")];
    assert_eq!(
        import(&mut conn, "st", "Lighting", "B", &restored, 200).unwrap(),
        2
    );
    assert_eq!(count(&conn, "audit"), 12);
    assert_eq!(cursor(&conn, "st").unwrap(), Some(("B".to_owned(), 3)));
}

#[test]
fn a_failing_insert_leaves_the_cursor_and_the_seen_rows() {
    let mut conn = latest();
    import(&mut conn, "st", "Lighting", "A", &[attempt(1, "a1")], 100).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER boom BEFORE INSERT ON audit WHEN NEW.detail LIKE '%0.99.0%'
         BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    )
    .unwrap();
    let mut bad = attempt(3, "a3");
    bad.to_version = Some("0.99.0".to_owned());
    let rows = [attempt(2, "a2"), bad];
    assert!(import(&mut conn, "st", "Lighting", "A", &rows, 200).is_err());
    assert_eq!(cursor(&conn, "st").unwrap(), Some(("A".to_owned(), 1)));
    assert_eq!(count(&conn, "cli_update_seen"), 1);
    assert_eq!(count(&conn, "audit"), 1);
}

#[test]
fn a_version_that_is_not_a_release_shows_as_a_question_mark() {
    let mut a = attempt(1, "a1");
    a.from_version = Some("1.0.0-rc1\nroot".to_owned());
    a.to_version = None;
    a.target_version = None;
    a.reason = Some("not a code\nwith text".to_owned());
    a.outcome = UpdateOutcome::Refused;
    assert_eq!(audit_detail(&a), "schedule bun ? -> -: refused (?)");
    a.reason = Some("known_broken".to_owned());
    assert_eq!(
        audit_detail(&a),
        "schedule bun ? -> -: refused (known_broken)"
    );
}

#[test]
fn the_detail_uses_the_target_when_there_is_no_result_version() {
    let mut a = attempt(1, "a1");
    a.to_version = None;
    a.outcome = UpdateOutcome::Failed;
    assert_eq!(audit_detail(&a), "schedule bun 0.50.0 -> 0.51.0: failed");
}

#[test]
fn the_audit_result_follows_the_outcome() {
    for (outcome, want) in [
        (UpdateOutcome::Updated, "ok"),
        (UpdateOutcome::UpToDate, "ok"),
        (UpdateOutcome::ReportOnly, "ok"),
        (UpdateOutcome::SkippedBusy, "ok"),
        (UpdateOutcome::Verified, "ok"),
        (UpdateOutcome::Refused, "refused"),
        (UpdateOutcome::Failed, "failed"),
        (UpdateOutcome::RolledBack, "failed"),
        (UpdateOutcome::RollbackFailed, "failed"),
        (UpdateOutcome::Interrupted, "failed"),
        (UpdateOutcome::Unknown, "failed"),
    ] {
        assert_eq!(audit_result(outcome), want, "{outcome:?}");
    }
}

#[test]
fn the_detail_has_at_most_300_characters() {
    let mut a = attempt(1, "a1");
    a.reason = Some("x".repeat(500));
    assert!(audit_detail(&a).chars().count() <= 300);
}

#[test]
fn the_seen_table_keeps_2000_rows_for_each_station() {
    let mut conn = latest();
    let n = u64::try_from(SEEN_KEEP).unwrap() + 50;
    for chunk in (1..=n).collect::<Vec<_>>().chunks(100) {
        let rows: Vec<_> = chunk
            .iter()
            .map(|&s| attempt(s, &format!("a{s}")))
            .collect();
        import(
            &mut conn,
            "st",
            "Lighting",
            "A",
            &rows,
            i64::try_from(chunk[0]).unwrap(),
        )
        .unwrap();
    }
    import(&mut conn, "other", "Cube", "A", &[attempt(1, "z1")], 1).unwrap();
    let kept: i64 = conn
        .query_row(
            "SELECT count(*) FROM cli_update_seen WHERE station_id = 'st'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, SEEN_KEEP);
    let other: i64 = conn
        .query_row(
            "SELECT count(*) FROM cli_update_seen WHERE station_id = 'other'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(other, 1);
    // The oldest attempt went; the newest stays.
    let has = |id: &str| -> bool {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM cli_update_seen WHERE attempt_id = ?1)",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(!has("a1"));
    assert!(has(&format!("a{n}")));
}
