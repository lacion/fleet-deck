//! The analytics store (B29, design `analytics.md` sections 3.3, 3.5 step 4,
//! 3.6a and 3.7 steps 4, 5 and 7): the file rows, the Codex thread ancestry,
//! the conversation root, the attribution of a record to a proven run, the
//! per-file transaction, the dedupe keys and the hour buckets, and the reads
//! of the query.

use std::collections::{HashMap, HashSet};

use fleetdeck_proto::SessionId;
use fleetdeck_proto::analytics::{Audience, HOUR_MS};
use fleetdeck_proto::event::ProviderKind;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Result, Store, call, to_u64};
use crate::analytics::UsageRecord;

/// The longest parent chain that the root resolution follows.
const MAX_ANCESTRY: usize = 16;

/// One row of `analytics_files`. `dev`, `ino` and `mtime_ns` are the bits of
/// the file facts as signed integers (SQLite has no unsigned type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub home: String,
    pub rel_path: String,
    pub provider: ProviderKind,
    pub dev: i64,
    pub ino: i64,
    pub mtime_ns: i64,
    pub offset: u64,
    pub guard: String,
    pub state: String,
    pub seen_size: u64,
    pub malformed: u64,
    pub scanned_at: i64,
}

/// What one file read gives the store: the records of the whole lines that
/// were read and the new file row. They commit together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCommit {
    pub file: FileRow,
    pub records: Vec<UsageRecord>,
}

/// What [`Store::analytics_commit_file`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommitReport {
    /// Records that added a request to a bucket.
    pub counted: u64,
    /// Records whose key was seen before.
    pub duplicates: u64,
    /// A Codex conversation whose ancestry was a cycle or too deep: the
    /// file's own thread is its conversation.
    pub ancestry_broken: bool,
}

/// One row of `analytics_usage` as stored: the conversation is the id, not
/// the hash of the wire form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRaw {
    pub hour: i64,
    pub home: String,
    pub provider: ProviderKind,
    pub model: String,
    pub conversation: String,
    /// `""` for a record outside a proven run.
    pub session_id: String,
    pub requests: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
}

/// One counter of an hour and a session: an event counter or a kept table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRaw {
    pub hour: i64,
    pub session_id: String,
    pub metric: String,
    pub value: u64,
}

/// The facts of one home that the owner form of a source holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeFacts {
    pub home: String,
    pub files: u64,
    pub malformed: u64,
    /// The oldest hour of the home in `analytics_usage`.
    pub first_at: Option<i64>,
}

/// The coverage dates (design section 3.7 step 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageRaw {
    pub events_since: Option<i64>,
    /// The oldest `created_at` of the listed sessions (all of them for
    /// `None`).
    pub kept_since: Option<i64>,
}

fn kind_text(kind: ProviderKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn kind_of(text: &str) -> ProviderKind {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
        .unwrap_or(ProviderKind::Unknown)
}

/// A count as the signed integer that SQLite stores (it saturates).
fn sql(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn count(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// The dedupe key of a record: the first 8 bytes of the SHA-256 of the key
/// text, as a signed integer.
#[must_use]
pub fn seen_key(text: &str) -> i64 {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let mut head = [0_u8; 8];
    head.copy_from_slice(&digest.as_ref()[..8]);
    i64::from_be_bytes(head)
}

/// The start of the UTC hour that holds `at`.
#[must_use]
pub fn hour_of(at: i64) -> i64 {
    at - at.rem_euclid(HOUR_MS)
}

/// `analytics_meta.next_seq`: every event below it is counted. The prune
/// keeps the events from it on (design section 3.3).
pub(super) fn next_seq(conn: &Connection) -> Result<u64> {
    let next: i64 = conn.query_row(
        "SELECT next_seq FROM analytics_meta WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    to_u64(next)
}

fn file_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        home: r.get(0)?,
        rel_path: r.get(1)?,
        provider: kind_of(&r.get::<_, String>(2)?),
        dev: r.get(3)?,
        ino: r.get(4)?,
        mtime_ns: r.get(5)?,
        offset: count(r.get(6)?),
        guard: r.get(7)?,
        state: r.get(8)?,
        seen_size: count(r.get(9)?),
        malformed: count(r.get(10)?),
        scanned_at: r.get(11)?,
    })
}

const FILE_COLUMNS: &str = "home, rel_path, provider, dev, ino, mtime_ns, offset, guard, state,
                            seen_size, malformed, scanned_at";

fn put_file(conn: &Connection, f: &FileRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO analytics_files
           (home, rel_path, provider, dev, ino, mtime_ns, offset, guard, state, seen_size,
            malformed, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            f.home,
            f.rel_path,
            kind_text(f.provider),
            f.dev,
            f.ino,
            f.mtime_ns,
            sql(f.offset),
            f.guard,
            f.state,
            sql(f.seen_size),
            sql(f.malformed),
            f.scanned_at,
        ],
    )?;
    Ok(())
}

/// The conversation root of a Codex `thread`: follows `analytics_threads`
/// parents, at most `MAX_ANCESTRY` steps. A thread without its own row, or
/// with no parent, is the root. A cycle or a longer chain gives the own
/// thread and `true` (the ancestry is broken).
pub(super) fn conversation_root(
    conn: &Connection,
    home: &str,
    thread: &str,
) -> Result<(String, bool)> {
    let mut seen: Vec<String> = vec![thread.to_owned()];
    let mut current = thread.to_owned();
    for step in 0..=MAX_ANCESTRY {
        let row: Option<Option<String>> = conn
            .query_row(
                "SELECT parent FROM analytics_threads WHERE home = ?1 AND thread = ?2",
                params![home, current],
                |r| r.get(0),
            )
            .optional()?;
        let Some(Some(parent)) = row else {
            return Ok((current, false));
        };
        if step == MAX_ANCESTRY || seen.contains(&parent) {
            return Ok((thread.to_owned(), true));
        }
        seen.push(parent.clone());
        current = parent;
    }
    Ok((thread.to_owned(), true))
}

/// One execution interval of a holder of a conversation.
struct Interval {
    session: String,
    start: i64,
    end: Option<i64>,
}

/// The intervals of every session that holds `(provider, conversation)`.
fn holder_intervals(
    conn: &Connection,
    provider: ProviderKind,
    conversation: &str,
) -> Result<Vec<Interval>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.session_id, e.started_at, e.ended_at
         FROM sessions s JOIN analytics_exec e ON e.session_id = s.session_id
         WHERE s.provider = ?1 AND s.native_session_id = ?2",
    )?;
    let rows = stmt.query_map(params![kind_text(provider), conversation], |r| {
        Ok(Interval {
            session: r.get(0)?,
            start: r.get(1)?,
            end: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The session of the proven run that holds a record made at `at`: exactly
/// one session with an interval that contains `at`; else `""` (design
/// section 3.6a).
fn attribute(intervals: &[Interval], at: i64) -> String {
    let mut found: Option<&str> = None;
    for i in intervals {
        if i.start <= at && i.end.is_none_or(|end| at <= end) {
            match found {
                Some(other) if other != i.session => return String::new(),
                _ => found = Some(&i.session),
            }
        }
    }
    found.unwrap_or_default().to_owned()
}

fn upsert_bucket(
    tx: &Transaction<'_>,
    f: &FileRow,
    r: &UsageRecord,
    conversation: &str,
    session: &str,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO analytics_usage
           (hour, home, provider, model, conversation, session_id, requests, input, cache_read,
            cache_write, output, reasoning)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (hour, home, provider, model, conversation, session_id) DO UPDATE SET
           requests    = requests + 1,
           input       = input + excluded.input,
           cache_read  = cache_read + excluded.cache_read,
           cache_write = cache_write + excluded.cache_write,
           output      = output + excluded.output,
           reasoning   = reasoning + excluded.reasoning",
    )?
    .execute(params![
        hour_of(r.at),
        f.home,
        kind_text(f.provider),
        r.model,
        conversation,
        session,
        sql(r.input),
        sql(r.cache_read),
        sql(r.cache_write),
        sql(r.output),
        sql(r.reasoning),
    ])?;
    Ok(())
}

/// The per-file transaction (design section 3.5 step 4): each record's key
/// is inserted once, a new key (or a record without a key) is attributed and
/// added to its hour bucket, then the file row is stored. The offset and the
/// counts commit together.
fn commit_file(conn: &mut Connection, commit: &FileCommit) -> Result<CommitReport> {
    let tx = conn.transaction()?;
    let f = &commit.file;
    let mut report = CommitReport::default();
    let mut roots: HashMap<String, String> = HashMap::new();
    let mut holders: HashMap<String, Vec<Interval>> = HashMap::new();
    for r in &commit.records {
        if let Some(key) = &r.key {
            let inserted = tx
                .prepare_cached("INSERT OR IGNORE INTO analytics_seen (key) VALUES (?1)")?
                .execute([seen_key(key)])?;
            if inserted == 0 {
                report.duplicates += 1;
                continue;
            }
        }
        let conversation = if f.provider == ProviderKind::Codex {
            if let Some(root) = roots.get(&r.conversation) {
                root.clone()
            } else {
                let (root, broken) = conversation_root(&tx, &f.home, &r.conversation)?;
                report.ancestry_broken |= broken;
                roots.insert(r.conversation.clone(), root.clone());
                root
            }
        } else {
            r.conversation.clone()
        };
        if !holders.contains_key(&conversation) {
            let list = holder_intervals(&tx, f.provider, &conversation)?;
            holders.insert(conversation.clone(), list);
        }
        let session = attribute(&holders[&conversation], r.at);
        upsert_bucket(&tx, f, r, &conversation, &session)?;
        report.counted += 1;
    }
    put_file(&tx, f)?;
    tx.commit()?;
    Ok(report)
}

fn usage_rows(
    conn: &Connection,
    since: i64,
    top: i64,
    audience: Audience,
    sessions: Option<&HashSet<String>>,
) -> Result<Vec<UsageRaw>> {
    let mut stmt = conn.prepare(
        "SELECT hour, home, provider, model, conversation, session_id, requests, input,
                cache_read, cache_write, output, reasoning
         FROM analytics_usage WHERE hour >= ?1 AND hour < ?2
         ORDER BY hour DESC, home, provider, model, conversation, session_id",
    )?;
    let rows = stmt.query_map(params![since, top], |r| {
        Ok(UsageRaw {
            hour: r.get(0)?,
            home: r.get(1)?,
            provider: kind_of(&r.get::<_, String>(2)?),
            model: r.get(3)?,
            conversation: r.get(4)?,
            session_id: r.get(5)?,
            requests: count(r.get(6)?),
            input: count(r.get(7)?),
            cache_read: count(r.get(8)?),
            cache_write: count(r.get(9)?),
            output: count(r.get(10)?),
            reasoning: count(r.get(11)?),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if audience == Audience::Member && row.session_id.is_empty() {
            continue;
        }
        if let Some(list) = sessions
            && !list.contains(&row.session_id)
        {
            continue;
        }
        out.push(row);
    }
    Ok(out)
}

fn activity_rows(
    conn: &Connection,
    since: i64,
    top: i64,
    sessions: Option<&HashSet<String>>,
) -> Result<Vec<ActivityRaw>> {
    let hour = HOUR_MS;
    let mut stmt = conn.prepare(&format!(
        "SELECT hour, session_id, metric, value FROM analytics_activity
           WHERE hour >= ?1 AND hour < ?2
         UNION ALL
         SELECT h, session_id, 'sessions.created', count(*) FROM
           (SELECT session_id, created_at / {hour} * {hour} AS h FROM sessions)
           WHERE h >= ?1 AND h < ?2 GROUP BY h, session_id
         UNION ALL
         SELECT h, session_id, 'turns.recorded', count(*) FROM
           (SELECT session_id, created_at / {hour} * {hour} AS h FROM turn_checkpoints
             WHERE state IN ('finalized', 'abandoned'))
           WHERE h >= ?1 AND h < ?2 GROUP BY h, session_id
         UNION ALL
         SELECT h, session_id, 'shell.commands', count(*) FROM
           (SELECT session_id, created_at / {hour} * {hour} AS h FROM shell_runs)
           WHERE h >= ?1 AND h < ?2 GROUP BY h, session_id
         UNION ALL
         SELECT h, session_id, 'shell.failed', count(*) FROM
           (SELECT session_id, created_at / {hour} * {hour} AS h FROM shell_runs
             WHERE exit_code IS NOT NULL AND exit_code <> 0)
           WHERE h >= ?1 AND h < ?2 GROUP BY h, session_id"
    ))?;
    let rows = stmt.query_map(params![since, top], |r| {
        Ok(ActivityRaw {
            hour: r.get(0)?,
            session_id: r.get(1)?,
            metric: r.get(2)?,
            value: count(r.get(3)?),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if let Some(list) = sessions
            && !list.contains(&row.session_id)
        {
            continue;
        }
        out.push(row);
    }
    out.sort_by(|a, b| {
        b.hour
            .cmp(&a.hour)
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.metric.cmp(&b.metric))
    });
    Ok(out)
}

fn home_facts(conn: &Connection) -> Result<Vec<HomeFacts>> {
    let mut stmt = conn.prepare(
        "SELECT home, files, malformed, first_at FROM (
           SELECT f.home AS home, count(*) AS files, sum(f.malformed) AS malformed,
                  (SELECT min(u.hour) FROM analytics_usage u WHERE u.home = f.home) AS first_at
           FROM analytics_files f GROUP BY f.home
           UNION ALL
           SELECT u.home, 0, 0, min(u.hour) FROM analytics_usage u
             WHERE u.home NOT IN (SELECT home FROM analytics_files) GROUP BY u.home)
         ORDER BY home",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(HomeFacts {
            home: r.get(0)?,
            files: count(r.get(1)?),
            malformed: count(r.get(2)?),
            first_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn coverage(conn: &Connection, sessions: Option<&HashSet<String>>) -> Result<CoverageRaw> {
    let events_since: Option<i64> = conn.query_row(
        "SELECT events_since FROM analytics_meta WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    let kept_since = match sessions {
        None => conn.query_row("SELECT min(created_at) FROM sessions", [], |r| r.get(0))?,
        Some(list) => {
            let mut stmt = conn.prepare("SELECT session_id, created_at FROM sessions")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            let mut min: Option<i64> = None;
            for row in rows {
                let (id, at) = row?;
                if list.contains(&id) {
                    min = Some(min.map_or(at, |m| m.min(at)));
                }
            }
            min
        }
    };
    Ok(CoverageRaw {
        events_since,
        kept_since,
    })
}

fn id_set(sessions: Option<Vec<SessionId>>) -> Option<HashSet<String>> {
    sessions.map(|list| list.into_iter().map(|s| s.as_str().to_owned()).collect())
}

impl Store {
    /// The file rows of one home.
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_files(&self, home: String) -> Result<Vec<FileRow>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {FILE_COLUMNS} FROM analytics_files WHERE home = ?1 ORDER BY rel_path"
            ))?;
            let rows = stmt.query_map([home], file_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// Stores a file row alone (a file that was walked but has no record
    /// to commit).
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_put_file(&self, file: FileRow) -> Result<()> {
        call(&self.inner.db, move |conn| put_file(conn, &file)).await
    }

    /// The Codex threads of one home that have a row.
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_known_threads(&self, home: String) -> Result<HashSet<String>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare("SELECT thread FROM analytics_threads WHERE home = ?1")?;
            let rows = stmt.query_map([home], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
        })
        .await
    }

    /// Records the ancestry of one Codex thread (the first record wins).
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_put_thread(
        &self,
        home: String,
        thread: String,
        parent: Option<String>,
    ) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO analytics_threads (home, thread, parent)
                 VALUES (?1, ?2, ?3)",
                params![home, thread, parent],
            )?;
            Ok(())
        })
        .await
    }

    /// The conversation root of a Codex thread, and whether the ancestry
    /// was broken (a cycle or more than 16 steps).
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_root(&self, home: String, thread: String) -> Result<(String, bool)> {
        call(&self.inner.db, move |conn| {
            conversation_root(conn, &home, &thread)
        })
        .await
    }

    /// Counts the records of one file read and stores the file row, in one
    /// transaction.
    ///
    /// # Errors
    /// A database error; nothing is stored.
    pub async fn analytics_commit_file(&self, commit: FileCommit) -> Result<CommitReport> {
        call(&self.inner.db, move |conn| commit_file(conn, &commit)).await
    }

    /// The usage rows of the hours `since <= hour < top`, newest hour
    /// first. A member gets only the rows of a proven run; `sessions`
    /// limits the rows to those sessions.
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_usage_rows(
        &self,
        since: i64,
        top: i64,
        audience: Audience,
        sessions: Option<Vec<SessionId>>,
    ) -> Result<Vec<UsageRaw>> {
        let sessions = id_set(sessions);
        call(&self.inner.db, move |conn| {
            usage_rows(conn, since, top, audience, sessions.as_ref())
        })
        .await
    }

    /// The counters of the hours `since <= hour < top`, newest hour first:
    /// the event counters and the kept tables.
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_activity_rows(
        &self,
        since: i64,
        top: i64,
        sessions: Option<Vec<SessionId>>,
    ) -> Result<Vec<ActivityRaw>> {
        let sessions = id_set(sessions);
        call(&self.inner.db, move |conn| {
            activity_rows(conn, since, top, sessions.as_ref())
        })
        .await
    }

    /// The owner facts of every home (files, malformed lines, first hour).
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_home_facts(&self) -> Result<Vec<HomeFacts>> {
        call(&self.inner.db, |conn| home_facts(conn)).await
    }

    /// The coverage dates; `sessions` limits `kept_since` to those sessions.
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_coverage(
        &self,
        sessions: Option<Vec<SessionId>>,
    ) -> Result<CoverageRaw> {
        let sessions = id_set(sessions);
        call(&self.inner.db, move |conn| {
            coverage(conn, sessions.as_ref())
        })
        .await
    }
}
