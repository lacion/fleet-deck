//! Capture tests of the session history: the import snapshot, the copy
//! checks and the lazy capture.

use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryState, HistoryUnavailable};

use super::tests_support::*;
use super::{History, Limits, at_import};
use crate::store::transcripts::Capture;

fn lines(n: u32) -> Vec<String> {
    (1..=n)
        .map(|i| {
            if i % 2 == 1 {
                claude_user(i, &format!("question {i}"))
            } else {
                claude_assistant(i, &format!("answer {i}"))
            }
        })
        .collect()
}

pub(super) async fn imported(
    rig: &Rig,
    id: &str,
    content: &[String],
) -> fleetdeck_proto::SessionId {
    let sid = rig.session(id, Some(NATIVE)).await;
    rig.first_effect(id, "resume", true).await;
    rig.write(NATIVE, content);
    at_import(&rig.store, Some(&rig.policy), &rig.private, &sid, NATIVE).await;
    sid
}

#[tokio::test]
async fn an_import_stores_a_private_snapshot_of_the_complete_lines() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let content = lines(4);
    let sid = rig.session("s1", Some(NATIVE)).await;
    let path = rig.write(NATIVE, &content);
    // The provider is in the middle of a line.
    let mut raw = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    raw.write_all(b"{\"type\":\"user\",\"half").unwrap();
    at_import(&rig.store, Some(&rig.policy), &rig.private, &sid, NATIVE).await;
    let row = rig.store.transcript(sid.clone()).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Import);
    let complete = join(&content);
    assert_eq!(row.prefix_size, Some(complete.len() as u64));
    assert_eq!(row.home.as_deref(), rig.home().to_str());
    let snapshot = std::path::PathBuf::from(row.snapshot.unwrap());
    assert_eq!(snapshot, rig.root.join("data/history/s1.jsonl"));
    assert_eq!(std::fs::read(&snapshot).unwrap(), complete);
    let mode = std::fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(snapshot.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
    let sha = super::index::hex(ring::digest::digest(&ring::digest::SHA256, &complete).as_ref());
    assert_eq!(row.prefix_sha256, Some(sha));
}

#[tokio::test]
async fn pages_do_not_change_when_the_provider_file_is_appended_or_rewritten() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let sid = imported(&rig, "s1", &lines(6)).await;
    let before = rig.ask("s1", None, Some(100), false).await.unwrap();
    assert_eq!(before.items.len(), 6);
    // Appended, then rewritten in place.
    let path = rig.transcript_path(NATIVE);
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"XXXXXXXXXXXXXXXX").unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(join(&[claude_user(40, "later")]).as_slice())
        .unwrap();
    drop(file);
    assert_eq!(rig.ask("s1", None, Some(100), false).await.unwrap(), before);
    // Also with no index in memory (a restart).
    let sha = rig
        .store
        .transcript(sid.clone())
        .await
        .unwrap()
        .unwrap()
        .prefix_sha256
        .unwrap();
    rig.history
        .shared
        .cache
        .lock()
        .unwrap()
        .remove(&(sid.to_string(), sha));
    assert_eq!(rig.history.cached(), 0);
    assert_eq!(rig.ask("s1", None, Some(100), false).await.unwrap(), before);
}

#[tokio::test]
async fn a_write_during_the_copy_retries_once_and_then_fails() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let path = rig.transcript_path(NATIVE);
    let sid = rig.session("s1", Some(NATIVE)).await;
    rig.write(NATIVE, &lines(4));
    // The provider rewrites a byte after every copy pass.
    let flip = {
        let path = path.clone();
        Arc::new(move |attempt: u32| {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(3)).unwrap();
            f.write_all(&[b'Z' - attempt as u8]).unwrap();
        })
    };
    *rig.history.seams().after_copy.lock().unwrap() = Some(flip);
    at_import(&rig.store, Some(&rig.policy), &rig.private, &sid, NATIVE).await;
    let row = rig.store.transcript(sid.clone()).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Failed);
    assert_eq!(row.reason.as_deref(), Some("not_captured"));
    assert!(!rig.root.join("data/history/s1.jsonl").exists());
    // The failed row is final: the owner read says so.
    let answer = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(
        answer.state,
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NotCaptured
        }
    );
}

#[tokio::test]
async fn a_write_in_the_first_pass_only_is_caught_and_the_retry_succeeds() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let path = rig.transcript_path(NATIVE);
    let sid = rig.session("s1", Some(NATIVE)).await;
    rig.write(NATIVE, &lines(4));
    let flip = Arc::new(move |attempt: u32| {
        if attempt == 0 {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(3)).unwrap();
            f.write_all(b"Z").unwrap();
        }
    });
    *rig.history.seams().after_copy.lock().unwrap() = Some(flip);
    at_import(&rig.store, Some(&rig.policy), &rig.private, &sid, NATIVE).await;
    let row = rig.store.transcript(sid).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Import);
    let snapshot = std::fs::read(row.snapshot.unwrap()).unwrap();
    assert_eq!(&snapshot[3..4], b"Z");
}

#[tokio::test]
async fn a_failed_capture_is_recorded_and_the_import_goes_on() {
    // No transcript.
    let rig = Rig::new(ProviderKind::Claude).await;
    let sid = rig.session("s1", Some(NATIVE)).await;
    at_import(&rig.store, Some(&rig.policy), &rig.private, &sid, NATIVE).await;
    let row = rig.store.transcript(sid).await.unwrap().unwrap();
    assert_eq!(
        (row.capture, row.reason.as_deref()),
        (Capture::Failed, Some("not_found"))
    );
    // No snapshot directory.
    let det = fleetdeck_testkit::Det::new();
    let store = crate::store::Store::open_in_memory(det.clock(), det.ids(), Default::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(crate::store::NewSession {
            session_id: "s9".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: crate::store::OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    at_import(&store, None, &rig.private, &"s9".into(), NATIVE).await;
    let row = store.transcript("s9".into()).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Failed);
}

#[tokio::test]
async fn an_existing_row_is_not_captured_again() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let sid = imported(&rig, "s1", &lines(2)).await;
    let first = rig.store.transcript(sid.clone()).await.unwrap().unwrap();
    rig.write(NATIVE, &lines(8));
    at_import(&rig.store, Some(&rig.policy), &rig.private, &sid, NATIVE).await;
    assert_eq!(rig.store.transcript(sid).await.unwrap().unwrap(), first);
}

#[tokio::test]
async fn an_owner_read_of_an_older_import_takes_a_lazy_snapshot_before_the_first_process() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    rig.first_effect("s1", "resume", true).await;
    // Records 1 and 2 are before the first process (10:00:10); a record
    // without a time follows 2; record 30 is later; so is the record
    // without a time that follows it.
    let no_time = r#"{"type":"user","uuid":"nt","sessionId":"x","message":{"role":"user","content":"no time"}}"#;
    let content = vec![
        claude_user(1, "q1"),
        claude_assistant(2, "a2"),
        no_time.replace("\"x\"", &format!("\"{NATIVE}\"")),
        claude_user(30, "later"),
        no_time
            .replace("\"nt\"", "\"nt2\"")
            .replace("\"x\"", &format!("\"{NATIVE}\"")),
    ];
    rig.write(NATIVE, &content);
    let cutoff = super::parse::parse_time_ms("2026-10-14T10:00:10.000Z").unwrap();
    rig.process("s1", cutoff).await;
    // A member read does not capture.
    let member = rig.ask("s1", None, None, true).await.unwrap();
    assert_eq!(
        member.state,
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NotCaptured
        }
    );
    assert!(rig.store.transcript("s1".into()).await.unwrap().is_none());
    // The owner read does.
    let owner = rig.ask("s1", None, None, false).await.unwrap();
    let texts: Vec<&str> = owner.items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["q1", "a2", "no time"]);
    let row = rig.store.transcript("s1".into()).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Lazy);
    // A member never reads a lazy snapshot.
    let member = rig.ask("s1", None, None, true).await.unwrap();
    assert_eq!(
        member.state,
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NotCaptured
        }
    );
}

#[tokio::test]
async fn two_first_owner_reads_take_one_lazy_snapshot_that_later_writes_do_not_change() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    rig.first_effect("s1", "resume", true).await;
    rig.write(NATIVE, &[claude_user(1, "q1"), claude_assistant(2, "a2")]);
    let cutoff = super::parse::parse_time_ms("2026-10-14T10:00:10.000Z").unwrap();
    rig.process("s1", cutoff).await;
    let (a, b) = tokio::join!(
        rig.ask("s1", None, None, false),
        rig.ask("s1", None, None, false)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.items, b.items);
    assert_eq!(a.state, HistoryState::Complete);
    let row = rig.store.transcript("s1".into()).await.unwrap().unwrap();
    // A rewrite of the provider file after the capture changes nothing.
    rig.write(
        NATIVE,
        &[claude_user(1, "other"), claude_assistant(2, "text")],
    );
    let later = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(later.items, a.items);
    assert_eq!(
        rig.store.transcript("s1".into()).await.unwrap().unwrap(),
        row
    );
}

#[tokio::test]
async fn a_lazy_capture_needs_a_process_row() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    rig.first_effect("s1", "resume", true).await;
    rig.write(NATIVE, &lines(2));
    let answer = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(
        answer.state,
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NotCaptured
        }
    );
}

#[test]
fn the_history_default_has_no_directory_until_one_is_set() {
    let history = History::with_limits(Limits::default());
    assert!(history.dir().is_none());
    history.set_dir("/a".into());
    history.set_dir("/b".into());
    assert_eq!(history.dir().unwrap().to_str(), Some("/a"));
}
