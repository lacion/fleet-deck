use rusqlite_migration::M;

use super::*;

fn v1() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    )])
}

fn v2() -> Migrations<'static> {
    Migrations::new(vec![
        M::up("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);"),
        M::up(
            "CREATE TABLE tags (note_id INTEGER NOT NULL REFERENCES notes(id), tag TEXT NOT NULL);",
        ),
    ])
}

fn broken() -> Migrations<'static> {
    Migrations::new(vec![
        M::up("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);"),
        M::up("CREATE TABLE half (x INTEGER); THIS IS NOT SQL;"),
    ])
}

fn count(db: &Db, table: &'static str) -> i64 {
    db.call_blocking(move |c| {
        Ok(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
    })
    .unwrap()
}

fn user_version(db: &Db) -> i64 {
    db.call_blocking(|c| Ok(c.pragma_query_value(None, "user_version", |r| r.get(0))?))
        .unwrap()
}

#[test]
fn a_file_database_uses_wal_full_sync_and_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("x/station.db"), v2()).unwrap();
    let (journal, sync, fk): (String, i64, i64) = db
        .call_blocking(|c| {
            Ok((
                c.pragma_query_value(None, "journal_mode", |r| r.get(0))?,
                c.pragma_query_value(None, "synchronous", |r| r.get(0))?,
                c.pragma_query_value(None, "foreign_keys", |r| r.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(sync, 2, "FULL");
    assert_eq!(fk, 1);
    assert_eq!(user_version(&db), 2);
    let err = db
        .call_blocking(|c| {
            c.execute("INSERT INTO tags (note_id, tag) VALUES (99, 'x')", [])?;
            Ok(())
        })
        .unwrap_err();
    assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.path().join("x"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);
}

#[test]
fn migrations_upgrade_an_existing_database_and_keep_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    {
        let db = Db::open(&path, v1()).unwrap();
        db.call_blocking(|c| {
            c.execute("INSERT INTO notes (body) VALUES ('kept')", [])?;
            Ok(())
        })
        .unwrap();
        assert_eq!(user_version(&db), 1);
    }
    let db = Db::open(&path, v2()).unwrap();
    assert_eq!(user_version(&db), 2);
    assert_eq!(count(&db, "notes"), 1);
    assert_eq!(count(&db, "tags"), 0);
}

#[test]
fn a_newer_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    drop(Db::open(&path, v2()).unwrap());
    let err = Db::open(&path, v1()).unwrap_err();
    assert!(matches!(err, DbError::TooNew { found: 2, .. }), "{err}");
}

#[test]
fn a_failing_migration_rolls_back_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    drop(Db::open(&path, v1()).unwrap());
    assert!(matches!(
        Db::open(&path, broken()).unwrap_err(),
        DbError::Migration(_)
    ));
    let db = Db::open(&path, v1()).unwrap();
    assert_eq!(user_version(&db), 1);
    let half: i64 = db
        .call_blocking(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'half'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(half, 0, "the failed migration left no table");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn async_calls_from_many_tasks_are_serialized() {
    let db = Db::open_in_memory(v1()).unwrap();
    let mut tasks = Vec::new();
    for i in 0..50 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            db.call(move |c| {
                let tx = c.transaction()?;
                let n: i64 = tx.query_row("SELECT count(*) FROM notes", [], |r| r.get(0))?;
                tx.execute("INSERT INTO notes (body) VALUES (?1)", [format!("{i}:{n}")])?;
                tx.commit()?;
                Ok(n)
            })
            .await
            .unwrap()
        }));
    }
    let mut seen = Vec::new();
    for t in tasks {
        seen.push(t.await.unwrap());
    }
    seen.sort_unstable();
    // Every job saw a distinct count: no two ran at the same time.
    assert_eq!(seen, (0..50).collect::<Vec<i64>>());
}

#[test]
fn errors_and_panics_in_a_job_do_not_stop_the_thread() {
    let db = Db::open_in_memory(v1()).unwrap();
    assert!(matches!(
        db.call_blocking(|c| {
            c.execute("INSERT INTO nope VALUES (1)", [])?;
            Ok(())
        }),
        Err(DbError::Sqlite(_))
    ));
    let err = db
        .call_blocking::<()>(|c| {
            let tx = c.transaction()?;
            tx.execute("INSERT INTO notes (body) VALUES ('lost')", [])?;
            panic!("boom");
        })
        .unwrap_err();
    assert!(matches!(&err, DbError::Panicked(m) if m == "boom"), "{err}");
    // The open transaction was rolled back during the unwind.
    assert_eq!(count(&db, "notes"), 0);
}

#[test]
fn dropping_the_last_handle_finishes_queued_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    {
        let db = Db::open(&path, v1()).unwrap();
        let other = db.clone();
        for _ in 0..20 {
            other
                .submit(
                    |c| {
                        c.execute("INSERT INTO notes (body) VALUES ('q')", [])?;
                        Ok(())
                    },
                    |_| {},
                )
                .unwrap();
        }
        drop(other);
    }
    let db = Db::open(&path, v1()).unwrap();
    assert_eq!(count(&db, "notes"), 20);
    assert_eq!(db.path(), Some(path.as_path()));
}
