use rusqlite_migration::M;

use super::*;
use crate::db::Db;

fn v1() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    )])
}

fn v2() -> Migrations<'static> {
    Migrations::new(vec![
        M::up("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);"),
        M::up("CREATE TABLE tags (note_id INTEGER NOT NULL, tag TEXT NOT NULL);"),
    ])
}

fn v3() -> Migrations<'static> {
    Migrations::new(vec![
        M::up("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);"),
        M::up("CREATE TABLE tags (note_id INTEGER NOT NULL, tag TEXT NOT NULL);"),
        M::up("CREATE TABLE extra (x INTEGER);"),
    ])
}

/// A v1 database with one row at `path`, closed.
fn make_v1(path: &Path) {
    let db = Db::open(path, v1()).unwrap();
    db.call_blocking(|c| {
        c.execute("INSERT INTO notes (body) VALUES ('kept')", [])?;
        Ok(())
    })
    .unwrap();
}

fn user_version_of(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn opts(ms: i64) -> BackupOptions {
    BackupOptions {
        skip: false,
        now_ms: ms,
    }
}

#[test]
fn an_upgrade_writes_a_private_backup_with_the_old_version_and_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    make_v1(&path);
    let db = Db::open(&path, v2()).unwrap();
    assert_eq!(user_version_of(&path), 2);
    drop(db);
    let backups = dir.path().join("backups");
    let list = names(&backups);
    assert_eq!(list.len(), 1, "{list:?}");
    assert!(
        list[0].starts_with("station-v1-to-v2-") && list[0].ends_with(".db"),
        "{list:?}"
    );
    assert_eq!(mode(&backups), 0o700);
    let file = backups.join(&list[0]);
    assert_eq!(mode(&file), 0o600);
    assert_eq!(user_version_of(&file), 1);
    let rows: i64 = Connection::open(&file)
        .unwrap()
        .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn the_deck_database_gets_a_deck_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck/deck.db");
    make_v1(&path);
    drop(Db::open(&path, v2()).unwrap());
    let list = names(&dir.path().join("backups"));
    assert!(list[0].starts_with("deck-v1-to-v2-"), "{list:?}");
}

#[test]
fn a_new_database_gets_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    drop(Db::open(&path, v2()).unwrap());
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn a_current_database_gets_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    drop(Db::open(&path, v2()).unwrap());
    drop(Db::open(&path, v2()).unwrap());
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn a_database_outside_the_layout_gets_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    // The trial copy, an epoch database and a loose test database.
    for rel in [
        "work/station.db",
        "link/epochs.db",
        "station.db",
        "deck/station.db",
    ] {
        let path = dir.path().join(rel);
        make_v1(&path);
        drop(Db::open(&path, v2()).unwrap());
        assert_eq!(user_version_of(&path), 2, "{rel}");
    }
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn only_the_newest_three_backups_stay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    let backups = dir.path().join("backups");
    fs::create_dir_all(&backups).unwrap();
    // Old backups; one number has more digits than the next (a string sort fails).
    for ms in [900, 1000, 20000] {
        fs::write(backups.join(format!("station-v1-to-v2-{ms}.db")), b"x").unwrap();
    }
    // Files that are not a backup of this database stay.
    fs::write(backups.join("deck-v1-to-v2-1.db"), b"x").unwrap();
    fs::write(backups.join("notes.txt"), b"x").unwrap();
    fs::write(backups.join("station-v1-to-v2-abc.db"), b"x").unwrap();
    make_v1(&path);
    let db = Db::open(&path, v2()).unwrap();
    drop(db);
    let list = names(&backups);
    assert!(
        !list.contains(&"station-v1-to-v2-900.db".to_owned()),
        "{list:?}"
    );
    assert!(
        list.contains(&"station-v1-to-v2-1000.db".to_owned()),
        "{list:?}"
    );
    assert!(
        list.contains(&"station-v1-to-v2-20000.db".to_owned()),
        "{list:?}"
    );
    for keep in ["deck-v1-to-v2-1.db", "notes.txt", "station-v1-to-v2-abc.db"] {
        assert!(list.contains(&keep.to_owned()), "{keep} in {list:?}");
    }
    let station: Vec<_> = list
        .iter()
        .filter(|n| backup_ms(n, "station").is_some())
        .collect();
    assert_eq!(station.len(), 3, "{list:?}");
}

#[test]
fn low_free_space_refuses_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    make_v1(&path);
    let conn = Connection::open(&path).unwrap();
    let size = file_len(&path) + file_len(&wal_path(&path));
    let err = backup_with_free(&conn, &path, &v2(), &opts(7), &|_| Ok(size * 2 - 1)).unwrap_err();
    let DbError::Backup { path: file, reason } = &err else {
        panic!("{err}");
    };
    assert!(file.ends_with("backups/station-v1-to-v2-7.db"), "{file:?}");
    assert_eq!(
        reason,
        &format!("needs {} bytes free, has {}", size * 2, size * 2 - 1)
    );
    let text = err.to_string();
    assert!(
        text.starts_with("Fleet Deck cannot write the pre-upgrade backup ")
            && text.ends_with(
                "Free disk space, or start once with FLEETDECK_SKIP_UPGRADE_BACKUP=1 to upgrade without a backup."
            ),
        "{text}"
    );
    assert!(!file.exists());
    // Exactly twice the size is enough.
    let ok = backup_with_free(&conn, &path, &v2(), &opts(8), &|_| Ok(size * 2)).unwrap();
    assert!(ok.is_some());
}

#[test]
fn an_unwritable_backup_folder_stops_before_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    make_v1(&path);
    // A file where the backup folder must be.
    fs::write(dir.path().join("backups"), b"in the way").unwrap();
    let err = Db::open(&path, v2()).unwrap_err();
    assert!(matches!(err, DbError::Backup { .. }), "{err}");
    assert_eq!(user_version_of(&path), 1, "the migration did not run");
}

#[test]
fn an_existing_backup_file_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    make_v1(&path);
    let conn = Connection::open(&path).unwrap();
    // The target name is taken: the backup fails and the file stays as it is.
    let backups = dir.path().join("backups");
    fs::create_dir_all(&backups).unwrap();
    let taken = backups.join("station-v1-to-v2-5.db");
    fs::write(&taken, b"other").unwrap();
    let err = backup_with_free(&conn, &path, &v2(), &opts(5), &|_| Ok(u64::MAX)).unwrap_err();
    assert!(matches!(err, DbError::Backup { .. }), "{err}");
    assert_eq!(fs::read(&taken).unwrap(), b"other");
}

#[test]
fn skip_migrates_without_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station/station.db");
    make_v1(&path);
    let conn = Connection::open(&path).unwrap();
    let skip = BackupOptions {
        skip: true,
        now_ms: 1,
    };
    assert_eq!(
        backup_with_free(&conn, &path, &v2(), &skip, &|_| Ok(0)).unwrap(),
        None
    );
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn the_backup_name_records_the_version_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck/deck.db");
    make_v1(&path);
    let conn = Connection::open(&path).unwrap();
    let file = backup_before_upgrade(&conn, &path, &v3(), &opts(42))
        .unwrap()
        .unwrap();
    assert!(file.ends_with("backups/deck-v1-to-v3-42.db"), "{file:?}");
}

#[test]
fn backup_names_parse_strictly() {
    assert_eq!(backup_ms("station-v1-to-v2-5.db", "station"), Some(5));
    assert_eq!(backup_ms("station-v12-to-v13-99.db", "station"), Some(99));
    assert_eq!(backup_ms("station-v1-to-v2-5.db", "deck"), None);
    assert_eq!(backup_ms("station-v1-to-v2-5.db.bak", "station"), None);
    assert_eq!(backup_ms("station-vx-to-v2-5.db", "station"), None);
    assert_eq!(backup_ms("station-v1-to-v2-.db", "station"), None);
}
