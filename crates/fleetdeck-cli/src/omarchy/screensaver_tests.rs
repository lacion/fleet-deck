//! Tests of the ambient screensaver (R13 P4 task G, `omarchy-p4.md` §3.8).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::cell::Cell;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;

const SHORT: Duration = Duration::from_millis(5);

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

fn summary(needs_you: u32, items: Value, usage: Value) -> FleetSummary {
    serde_json::from_value(summary_json(needs_you, items, usage)).unwrap()
}

fn usage(label: &str, pct: u32, resets: Value) -> Value {
    json!({"label": label, "pct": pct, "resets_in_s": resets})
}

fn fleet() -> Vec<String> {
    vec!["FLEET DECK".to_owned()]
}

fn lines(text: &str) -> Vec<&str> {
    assert!(text.ends_with('\n'), "every line ends with a newline");
    text.lines().collect()
}

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("ss").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

fn enable(home: &Home) -> Record {
    let mut record = Record::new();
    record.screensaver.enabled = true;
    record::save(home, &record).unwrap();
    record
}

/// Turns the switch of the record off; the other fields stay.
fn disable(home: &Home) {
    let mut record = record::load(home).unwrap().unwrap();
    record.screensaver.enabled = false;
    record::save(home, &record).unwrap();
}

fn screensaver_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("omarchy").join(FILE)
}

fn backup_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("omarchy").join(BACKUP)
}

fn put_original(dir: &tempfile::TempDir, text: &str) {
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

// ---- the job ----

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn no_tools() -> (tempfile::TempDir, ChildEnv) {
    let bin = tempfile::Builder::new().prefix("bin").tempdir().unwrap();
    let env = tool_env(bin.path());
    (bin, env)
}

#[test]
fn screensaver_job_exits_at_once_without_a_record_or_when_disabled() {
    let (_dir, home) = home();
    let (_bin, env) = no_tools();
    let calls = Cell::new(0);
    let fetch = || {
        calls.set(calls.get() + 1);
        std::future::ready(None)
    };
    runtime().block_on(job(&home, &env, SHORT, fetch)).unwrap();
    let mut record = Record::new();
    record::save(&home, &record).unwrap();
    runtime().block_on(job(&home, &env, SHORT, fetch)).unwrap();
    record.screensaver.enabled = false;
    record::save(&home, &record).unwrap();
    runtime().block_on(job(&home, &env, SHORT, fetch)).unwrap();
    assert_eq!(calls.get(), 0);
}

#[test]
fn screensaver_job_first_write_makes_the_backup_and_records_the_hashes() {
    let (dir, home) = home();
    let (_bin, env) = no_tools();
    put_original(&dir, "my own screensaver\n");
    enable(&home);
    let s = summary(1, json!([]), Value::Null);
    let calls = Cell::new(0);
    let fetch = || {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            disable(&home);
        }
        std::future::ready(Some(s.clone()))
    };
    runtime().block_on(job(&home, &env, SHORT, fetch)).unwrap();
    assert_eq!(
        std::fs::read_to_string(backup_file(&dir)).unwrap(),
        "my own screensaver\n"
    );
    let written = std::fs::read_to_string(screensaver_file(&dir)).unwrap();
    assert!(written.starts_with("FLEET DECK\n"), "{written}");
    let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(backup_file(&dir)), 0o644);
    assert_eq!(mode(screensaver_file(&dir)), 0o644);
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.had_original);
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
fn screensaver_job_without_an_original_records_no_backup() {
    let (dir, home) = home();
    let (_bin, env) = no_tools();
    enable(&home);
    let calls = Cell::new(0);
    let fetch = || {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            return std::future::ready(None);
        }
        disable(&home);
        std::future::ready(None)
    };
    runtime().block_on(job(&home, &env, SHORT, fetch)).unwrap();
    assert_eq!(
        std::fs::read_to_string(screensaver_file(&dir)).unwrap(),
        "Fleet Deck: no data\n"
    );
    assert!(!backup_file(&dir).exists());
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.had_original);
    assert!(record.screensaver.backup_sha256.is_none());
    assert!(record.screensaver.last_written_sha256.is_some());
}

#[test]
fn screensaver_job_an_existing_backup_is_never_replaced() {
    let (dir, home) = home();
    let (_bin, env) = no_tools();
    put_original(&dir, "newer original\n");
    std::fs::write(backup_file(&dir), "older backup\n").unwrap();
    enable(&home);
    let calls = Cell::new(0);
    let fetch = || {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            disable(&home);
        }
        std::future::ready(None)
    };
    runtime().block_on(job(&home, &env, SHORT, fetch)).unwrap();
    assert_eq!(
        std::fs::read_to_string(backup_file(&dir)).unwrap(),
        "older backup\n"
    );
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.had_original);
    assert_eq!(
        record.screensaver.backup_sha256.as_deref(),
        Some(sha256_hex(b"older backup\n").as_str())
    );
}

#[test]
fn screensaver_job_writes_only_when_the_text_changed() {
    let (dir, home) = home();
    let (_bin, env) = no_tools();
    enable(&home);
    let a = summary(1, json!([]), Value::Null);
    let b = summary(2, json!([]), Value::Null);
    let calls = Cell::new(0);
    let fetch = || {
        let n = calls.get() + 1;
        calls.set(n);
        let path = screensaver_file(&dir);
        match n {
            // The same summary again: the marker must stay.
            2 => std::fs::write(&path, "MARK\n").unwrap(),
            // A changed summary: the file is written again.
            4 => {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "MARK\n");
            }
            5 => disable(&home),
            _ => {}
        }
        std::future::ready(Some(match n {
            1..=3 => a.clone(),
            _ => b.clone(),
        }))
    };
    runtime().block_on(job(&home, &env, SHORT, fetch)).unwrap();
    assert_eq!(calls.get(), 5);
    let text = std::fs::read_to_string(screensaver_file(&dir)).unwrap();
    assert_ne!(text, "MARK\n");
    assert_eq!(
        lines(&text)[2..7],
        ["#####", "    #", "#####", "#", "#####"]
    );
}

// ---- the restore ----

fn written_record(
    home: &Home,
    dir: &tempfile::TempDir,
    original: Option<&str>,
    text: &str,
) -> Record {
    let mut record = enable(home);
    let file = screensaver_file(dir);
    if let Some(original) = original {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(backup_file(dir), original).unwrap();
        record.screensaver.had_original = true;
        record.screensaver.backup_sha256 = Some(sha256_hex(original.as_bytes()));
    }
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, text).unwrap();
    record.screensaver.last_written_sha256 = Some(sha256_hex(text.as_bytes()));
    record
}

#[test]
fn screensaver_restore_with_an_original_puts_the_backup_back() {
    let (dir, home) = home();
    let mut record = written_record(&home, &dir, Some("mine\n"), "ours\n");
    restore(&home, &mut record);
    assert_eq!(
        std::fs::read_to_string(screensaver_file(&dir)).unwrap(),
        "mine\n"
    );
    assert!(!backup_file(&dir).exists());
    assert_eq!(record.screensaver, Screensaver::default());
}

#[test]
fn screensaver_restore_without_an_original_removes_the_written_file() {
    let (dir, home) = home();
    let mut record = written_record(&home, &dir, None, "ours\n");
    restore(&home, &mut record);
    assert!(!screensaver_file(&dir).exists());
    assert_eq!(record.screensaver, Screensaver::default());
}

#[test]
fn screensaver_restore_keeps_a_file_that_the_user_changed() {
    let (dir, home) = home();
    let mut record = written_record(&home, &dir, Some("mine\n"), "ours\n");
    std::fs::write(screensaver_file(&dir), "edited by the user\n").unwrap();
    restore(&home, &mut record);
    assert_eq!(
        std::fs::read_to_string(screensaver_file(&dir)).unwrap(),
        "edited by the user\n"
    );
    assert_eq!(
        std::fs::read_to_string(backup_file(&dir)).unwrap(),
        "mine\n"
    );
    assert_eq!(
        record.screensaver,
        Screensaver::default(),
        "reset after a kept file"
    );
}

#[test]
fn screensaver_restore_does_nothing_when_the_screensaver_was_not_on() {
    let (dir, home) = home();
    let mut record = written_record(&home, &dir, Some("mine\n"), "ours\n");
    record.screensaver.enabled = false;
    let before = record.clone();
    restore(&home, &mut record);
    assert_eq!(
        std::fs::read_to_string(screensaver_file(&dir)).unwrap(),
        "ours\n"
    );
    assert_eq!(record, before);
}

#[test]
fn screensaver_restore_refuses_to_follow_a_link() {
    let (dir, home) = home();
    let mut record = written_record(&home, &dir, Some("mine\n"), "ours\n");
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    std::fs::write(outside.path().join("victim"), "keep").unwrap();
    std::fs::remove_file(screensaver_file(&dir)).unwrap();
    std::os::unix::fs::symlink(outside.path().join("victim"), screensaver_file(&dir)).unwrap();
    restore(&home, &mut record);
    assert_eq!(
        std::fs::read_to_string(outside.path().join("victim")).unwrap(),
        "keep"
    );
}
