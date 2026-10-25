//! Tests of the ambient screensaver (R13 P4 task G, `omarchy-p4.md` §3.8;
//! the tick, `on`, `off` and `rebaseline` of `omarchy-admin.md` §3.4 to §3.7).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;

fn summary_json(needs_you: u32, items: Value, usage: Value) -> Value {
    json!({
        "v": 1, "at": "2026-10-18T12:04:00Z", "board": null, "deck": "up",
        "counts": {"working": 7, "idle": 3, "needs_you": needs_you, "blocked": 1, "lost": 2},
        "needs_you": items, "usage": usage, "stations": []
    })
}

fn item(session: &str, callsign: Value, kind: &str, age_s: u32) -> Value {
    json!({"station": "srv", "station_id": "st1", "session": session, "request": "r1",
           "callsign": callsign, "kind": kind, "age_s": age_s})
}

pub(super) fn summary(needs_you: u32, items: Value, usage: Value) -> FleetSummary {
    serde_json::from_value(summary_json(needs_you, items, usage)).unwrap()
}

fn usage(label: &str, pct: u32, resets: Value) -> Value {
    json!({"label": label, "pct": pct, "resets_in_s": resets})
}

pub(super) fn fleet() -> Vec<String> {
    vec!["FLEET DECK".to_owned()]
}

fn lines(text: &str) -> Vec<&str> {
    assert!(text.ends_with('\n'), "every line ends with a newline");
    text.lines().collect()
}

pub(super) fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("ss").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

/// Makes the lock file (the install does it), and saves `record`.
pub(super) fn install(home: &Home, record: &Record) {
    drop(lock::take(home, Duration::ZERO, true).unwrap());
    record::save(home, record).unwrap();
}

/// A version 2 record with the switch on.
pub(super) fn enable(home: &Home) -> Record {
    let mut record = Record::new();
    record.screensaver.enabled = true;
    install(home, &record);
    record
}

pub(super) fn screensaver_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("omarchy").join(FILE)
}

pub(super) fn backup_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("omarchy").join(BACKUP)
}

pub(super) fn put_original(dir: &tempfile::TempDir, text: &str) {
    let path = screensaver_file(dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn tool_env(path: &Path) -> ChildEnv {
    let mut env = ChildEnv::empty();
    env.set("PATH", path).unwrap();
    env
}

fn fake_omarchy(dir: &Path, body: &str) {
    let path = dir.join("omarchy");
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// No wait: the lock is free or the command fails at once.
pub(super) const W: Duration = Duration::ZERO;

/// The exit code as text (`ExitCode` has no equality).
pub(super) fn code(code: ExitCode) -> String {
    format!("{code:?}")
}

pub(super) fn ok() -> String {
    code(ExitCode::SUCCESS)
}

pub(super) fn fail() -> String {
    code(ExitCode::from(1))
}

// ---- the render ----

#[test]
fn screensaver_render_without_data_is_one_line() {
    assert_eq!(render(None, &fleet()), "Fleet Deck: no data\n");
}

#[test]
fn screensaver_render_the_six_parts() {
    let s = summary(
        0,
        json!([item("s1", json!("lynx-12"), "tool", 95)]),
        usage("claude \u{b7} 5 h", 63, json!(4320)),
    );
    let text = render(Some(&s), &fleet());
    let want = [
        "FLEET DECK",
        "",
        "#####",
        "#   #",
        "#   #",
        "#   #",
        "#####",
        "need you",
        "working 7   idle 3   blocked 1   lost 2",
        "lynx-12 - Approval - 1 min",
        "claude - 5 h - 63% - resets in 1 h 12 min",
    ];
    assert_eq!(lines(&text), want);
}

#[test]
fn screensaver_render_seven_needs_you_and_a_question() {
    let s = summary(
        7,
        json!([item("s1", json!(null), "question", 4)]),
        Value::Null,
    );
    let text = render(Some(&s), &fleet());
    let got = lines(&text);
    assert_eq!(got[2..7], ["#####", "    #", "   #", "  #", "  #"]);
    assert_eq!(got[7], "need you");
    assert_eq!(got[9], "s1 - Question - 4 s");
    assert_eq!(got.len(), 10, "no usage line");
}

#[test]
fn screensaver_render_above_99_shows_99() {
    let s = summary(120, json!([]), Value::Null);
    let text = render(Some(&s), &fleet());
    let got = lines(&text);
    assert_eq!(got[2], "##### #####");
    assert_eq!(got[3], "#   # #   #");
    assert_eq!(got[7], "need you");
    assert_eq!(
        got[8], "working 7   idle 3   blocked 1   lost 2",
        "the counts line is always there"
    );
    assert_eq!(got.len(), 9, "no item and no usage line");
}

#[test]
fn screensaver_render_a_long_callsign_and_the_oldest_item() {
    let long = "a".repeat(32);
    let s = summary(
        2,
        json!([
            item("s1", json!("young"), "command", 10),
            item("s2", json!(long), "file_change", 7300),
            item("s3", json!("tie"), "tool", 7300),
        ]),
        Value::Null,
    );
    let text = render(Some(&s), &fleet());
    let want = format!("{long} - Approval - 2 h");
    assert_eq!(lines(&text)[9], want);
    assert!(want.len() <= 100);
}

#[test]
fn screensaver_render_a_session_name_follows_the_notification_rule() {
    let s = summary(
        1,
        json!([item("-sessionid-long", json!("Bad Call"), "tool", 1)]),
        Value::Null,
    );
    assert_eq!(
        lines(&render(Some(&s), &fleet()))[9],
        "session - Approval - 1 s"
    );
    let s = summary(
        1,
        json!([item("abcdefghijkl", json!(null), "tool", 61)]),
        Value::Null,
    );
    assert_eq!(
        lines(&render(Some(&s), &fleet()))[9],
        "abcdefgh - Approval - 1 min"
    );
}

#[test]
fn screensaver_render_non_ascii_becomes_a_question_mark() {
    let s = summary(
        1,
        json!([item("s\u{e9}ssion1", json!(null), "tool", 1)]),
        usage("clau\u{e9}\u{4e2d} \u{b7} 5 h", 9, Value::Null),
    );
    let text = render(Some(&s), &fleet());
    assert!(text.is_ascii());
    let got = lines(&text);
    // The session prefix `s?ssion1` breaks the name rule: `session`.
    assert_eq!(got[9], "session - Approval - 1 s");
    assert_eq!(got[10], "clau?? - 5 h - 9%");
}

#[test]
fn screensaver_render_usage_without_a_reset_omits_that_part() {
    let s = summary(1, json!([]), usage("claude \u{b7} 7 d", 100, Value::Null));
    let text = render(Some(&s), &fleet());
    assert_eq!(
        lines(&text)[8..],
        [
            "working 7   idle 3   blocked 1   lost 2",
            "claude - 7 d - 100%"
        ]
    );
    let s = summary(1, json!([]), usage("x", 1, json!(59)));
    assert_eq!(
        *lines(&render(Some(&s), &fleet())).last().unwrap(),
        "x - 1% - resets in 0 h 0 min"
    );
}

#[test]
fn screensaver_render_limits_are_40_lines_of_100_columns() {
    let wide = vec!["W".repeat(150); 30];
    let s = summary(1, json!([]), usage(&"L".repeat(300), 1, Value::Null));
    let text = render(Some(&s), &wide);
    let got = lines(&text);
    assert!(got.len() <= 40);
    assert!(got.iter().all(|l| l.len() <= 100));
    // The banner is cut at 18 lines.
    assert_eq!(got.iter().filter(|l| l.starts_with('W')).count(), 18);
    assert_eq!(got[0].len(), 100);
    assert_eq!(got.last().unwrap().len(), 100);
}

#[test]
fn screensaver_banner_uses_omarchy_ascii_when_it_answers() {
    let bin = tempfile::Builder::new().prefix("bin").tempdir().unwrap();
    let body = format!(
        "[ \"$1 $2\" = \"ascii FLEET\" ] || exit 2\necho 'AAA'\necho 'B\u{e9}B'\n\
         i=0\nwhile [ $i -lt 30 ]; do echo {}; i=$((i+1)); done\n",
        "z".repeat(150)
    );
    fake_omarchy(bin.path(), &body);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let got = rt.block_on(banner(&tool_env(bin.path())));
    assert_eq!(got.len(), 18);
    assert_eq!(got[0], "AAA");
    assert_eq!(got[1], "B?B");
    assert!(got[2].len() == 100 && got[2].starts_with('z'));
}

#[test]
fn screensaver_banner_falls_back_to_fleet_deck() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // No `omarchy` on the PATH.
    let empty = tempfile::Builder::new().prefix("bin").tempdir().unwrap();
    assert_eq!(rt.block_on(banner(&tool_env(empty.path()))), ["FLEET DECK"]);
    // A failing `omarchy ascii`, and one without output.
    fake_omarchy(empty.path(), "echo oops\nexit 1\n");
    assert_eq!(rt.block_on(banner(&tool_env(empty.path()))), ["FLEET DECK"]);
    fake_omarchy(empty.path(), "exit 0\n");
    assert_eq!(rt.block_on(banner(&tool_env(empty.path()))), ["FLEET DECK"]);
}

// ---- the tick ----

/// One tick with `summary` (and no banner but the fallback).
pub(super) fn tick(home: &Home, summary: Option<&FleetSummary>) -> String {
    code(tick_with(home, summary, &fleet()).unwrap())
}

pub(super) fn read(path: std::path::PathBuf) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn screensaver_tick_does_nothing_without_a_lock_file_a_record_or_when_off() {
    let (dir, home) = home();
    // No lock file at all.
    assert_eq!(tick(&home, None), ok());
    assert!(!screensaver_file(&dir).exists());
    // A lock file, no record.
    drop(lock::take(&home, Duration::ZERO, true).unwrap());
    assert_eq!(tick(&home, None), ok());
    assert!(!screensaver_file(&dir).exists());
    // A record with the switch off.
    let record = Record::new();
    install(&home, &record);
    assert_eq!(tick(&home, None), ok());
    assert!(!screensaver_file(&dir).exists());
    assert_eq!(record::load(&home).unwrap().unwrap(), record);
}

#[test]
fn screensaver_tick_writes_only_with_version_2_and_the_switch_on() {
    let (dir, home) = home();
    let mut record = Record::new();
    record.v = 1;
    record.screensaver.enabled = true;
    install(&home, &record);
    assert_eq!(tick(&home, None), ok());
    assert!(!screensaver_file(&dir).exists(), "version 1 writes nothing");
    assert_eq!(record::load(&home).unwrap().unwrap(), record);
    record.v = 2;
    record::save(&home, &record).unwrap();
    assert_eq!(tick(&home, None), ok());
    assert_eq!(read(screensaver_file(&dir)), "Fleet Deck: no data\n");
}

#[test]
fn screensaver_tick_with_an_invalid_record_writes_nothing_and_exits_0() {
    let (dir, home) = home();
    enable(&home);
    std::fs::write(dir.path().join(record::RECORD_PATH), "{ not json").unwrap();
    assert_eq!(tick(&home, None), ok());
    assert!(!screensaver_file(&dir).exists());
}

#[test]
fn screensaver_tick_with_the_lock_held_writes_nothing() {
    let (dir, home) = home();
    let before = enable(&home);
    let guard = lock::take(&home, Duration::ZERO, false).unwrap();
    assert!(matches!(guard, Take::Held(_)));
    assert_eq!(tick(&home, None), ok());
    assert!(!screensaver_file(&dir).exists());
    assert_eq!(record::load(&home).unwrap().unwrap(), before);
    drop(guard);
    assert_eq!(tick(&home, None), ok());
    assert!(screensaver_file(&dir).exists());
}

#[test]
fn screensaver_tick_first_write_makes_the_backup_and_records_the_hashes() {
    let (dir, home) = home();
    put_original(&dir, "my own screensaver\n");
    enable(&home);
    let s = summary(1, json!([]), Value::Null);
    assert_eq!(tick(&home, Some(&s)), ok());
    assert_eq!(read(backup_file(&dir)), "my own screensaver\n");
    let written = read(screensaver_file(&dir));
    assert!(written.starts_with("FLEET DECK\n"), "{written}");
    let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(backup_file(&dir)), 0o644);
    assert_eq!(mode(screensaver_file(&dir)), 0o644);
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.had_original);
    assert!(!record.screensaver.first_write_refused);
    assert_eq!(
        record.screensaver.backup_sha256.as_deref(),
        Some(sha256_hex(b"my own screensaver\n").as_str())
    );
    assert_eq!(
        record.screensaver.last_written_sha256.as_deref(),
        Some(sha256_hex(written.as_bytes()).as_str())
    );
}

#[test]
fn screensaver_tick_without_an_original_records_no_backup() {
    let (dir, home) = home();
    enable(&home);
    assert_eq!(tick(&home, None), ok());
    assert_eq!(read(screensaver_file(&dir)), "Fleet Deck: no data\n");
    assert!(!backup_file(&dir).exists());
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.had_original);
    assert!(record.screensaver.backup_sha256.is_none());
    assert!(record.screensaver.last_written_sha256.is_some());
}

#[test]
fn screensaver_tick_an_old_backup_that_differs_is_refused_and_recorded() {
    let (dir, home) = home();
    put_original(&dir, "newer original\n");
    std::fs::write(backup_file(&dir), "older backup\n").unwrap();
    enable(&home);
    assert_eq!(tick(&home, None), ok(), "a refusal still exits 0");
    assert_eq!(read(screensaver_file(&dir)), "newer original\n");
    assert_eq!(read(backup_file(&dir)), "older backup\n");
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.first_write_refused);
    assert!(record.screensaver.enabled, "the switch stays on");
    assert!(record.screensaver.last_written_sha256.is_none());
    assert!(!record.screensaver.had_original);
}

#[test]
fn screensaver_tick_an_old_backup_without_a_current_file_writes_nothing() {
    let (dir, home) = home();
    std::fs::create_dir_all(backup_file(&dir).parent().unwrap()).unwrap();
    std::fs::write(backup_file(&dir), "older backup\n").unwrap();
    enable(&home);
    assert_eq!(tick(&home, None), ok());
    assert!(!screensaver_file(&dir).exists());
    assert_eq!(read(backup_file(&dir)), "older backup\n");
    assert!(
        record::load(&home)
            .unwrap()
            .unwrap()
            .screensaver
            .first_write_refused
    );
}

#[test]
fn screensaver_tick_an_old_backup_that_is_identical_to_the_file_goes_on() {
    let (dir, home) = home();
    put_original(&dir, "same\n");
    std::fs::write(backup_file(&dir), "same\n").unwrap();
    enable(&home);
    assert_eq!(tick(&home, None), ok());
    assert_eq!(read(backup_file(&dir)), "same\n");
    let written = read(screensaver_file(&dir));
    assert_eq!(written, "Fleet Deck: no data\n");
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.had_original);
    assert!(!record.screensaver.first_write_refused);
    assert_eq!(
        record.screensaver.backup_sha256.as_deref(),
        Some(sha256_hex(b"same\n").as_str())
    );
    assert_eq!(
        record.screensaver.last_written_sha256.as_deref(),
        Some(sha256_hex(written.as_bytes()).as_str())
    );
}

#[test]
fn screensaver_tick_a_later_first_write_clears_the_refusal() {
    let (dir, home) = home();
    put_original(&dir, "current\n");
    std::fs::write(backup_file(&dir), "older\n").unwrap();
    enable(&home);
    assert_eq!(tick(&home, None), ok());
    assert!(
        record::load(&home)
            .unwrap()
            .unwrap()
            .screensaver
            .first_write_refused
    );
    // The user makes the file identical to the kept backup.
    std::fs::write(screensaver_file(&dir), "older\n").unwrap();
    assert_eq!(tick(&home, None), ok());
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.first_write_refused);
    assert!(record.screensaver.last_written_sha256.is_some());
}

#[test]
fn screensaver_tick_writes_only_when_the_text_changed() {
    let (dir, home) = home();
    enable(&home);
    let a = summary(1, json!([]), Value::Null);
    let b = summary(2, json!([]), Value::Null);
    assert_eq!(tick(&home, Some(&a)), ok());
    std::fs::write(screensaver_file(&dir), "MARK\n").unwrap();
    // The same summary again: the marker must stay.
    assert_eq!(tick(&home, Some(&a)), ok());
    assert_eq!(read(screensaver_file(&dir)), "MARK\n");
    // A changed summary: the file is written again.
    assert_eq!(tick(&home, Some(&b)), ok());
    let text = read(screensaver_file(&dir));
    assert_ne!(text, "MARK\n");
    assert_eq!(
        lines(&text)[2..7],
        ["#####", "    #", "#####", "#", "#####"]
    );
}

#[test]
fn screensaver_tick_never_deletes_the_backup() {
    let (dir, home) = home();
    put_original(&dir, "mine\n");
    enable(&home);
    let a = summary(1, json!([]), Value::Null);
    let b = summary(2, json!([]), Value::Null);
    for s in [None, Some(&a), Some(&b)] {
        assert_eq!(tick(&home, s), ok());
        assert_eq!(read(backup_file(&dir)), "mine\n");
    }
}

/// An installed record with the screensaver written once over `original`.
pub(super) fn written(home: &Home, dir: &tempfile::TempDir, original: Option<&str>) -> Record {
    if let Some(original) = original {
        put_original(dir, original);
    }
    enable(home);
    assert_eq!(tick(home, None), ok());
    record::load(home).unwrap().unwrap()
}
