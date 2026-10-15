//! Page, cursor, state and budget tests of the session history.

use std::time::Duration;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryKind, HistoryState, HistoryUnavailable};

use super::tests_support::*;
use super::{Limits, at_import};

const N: u32 = 7;

fn convo(native: &str, n: u32) -> Vec<String> {
    (1..=n)
        .map(|i| {
            let line = if i % 2 == 1 {
                claude_user(i, &format!("text {i}"))
            } else {
                claude_assistant(i, &format!("text {i}"))
            };
            line.replace(NATIVE, native)
        })
        .collect()
}

async fn imported(rig: &Rig, id: &str, native: &str, content: &[String]) {
    rig.session(id, Some(native)).await;
    rig.first_effect(id, "resume", true).await;
    rig.write(native, content);
    at_import(
        &rig.store,
        Some(&rig.policy),
        &rig.private,
        &id.into(),
        native,
    )
    .await;
}

fn texts(h: &fleetdeck_proto::history::SessionHistory) -> Vec<String> {
    h.items.iter().map(|i| i.text.clone()).collect()
}

#[tokio::test]
async fn three_pages_cover_all_items_once_newest_page_first() {
    let rig = Rig::new(ProviderKind::Claude).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, N)).await;
    let p1 = rig.ask("s1", None, Some(3), false).await.unwrap();
    assert_eq!(texts(&p1), ["text 5", "text 6", "text 7"]);
    assert_eq!(p1.state, HistoryState::Complete);
    let p2 = rig
        .ask("s1", p1.before.as_deref(), Some(3), false)
        .await
        .unwrap();
    assert_eq!(texts(&p2), ["text 2", "text 3", "text 4"]);
    let p3 = rig
        .ask("s1", p2.before.as_deref(), Some(3), false)
        .await
        .unwrap();
    assert_eq!(texts(&p3), ["text 1"]);
    assert_eq!(p3.before, None);
    let kinds: Vec<_> = p1.items.iter().map(|i| i.kind).collect();
    assert_eq!(
        kinds,
        [HistoryKind::User, HistoryKind::Assistant, HistoryKind::User]
    );
    // The default page holds everything here.
    assert_eq!(
        rig.ask("s1", None, None, false).await.unwrap().items.len(),
        N as usize
    );
}

#[tokio::test]
async fn the_items_of_the_snapshot_get_their_count_stored() {
    let rig = Rig::new(ProviderKind::Claude).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, N)).await;
    rig.ask("s1", None, None, false).await.unwrap();
    let row = rig.store.transcript("s1".into()).await.unwrap().unwrap();
    assert_eq!(row.item_count, Some(u64::from(N)));
}

#[tokio::test]
async fn a_page_of_escaped_text_stays_under_512_kib_serialized() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let nasty = "\"\\".repeat(6 * 1024);
    let content: Vec<String> = (1..=120).map(|i| claude_user(i, &nasty)).collect();
    imported(&rig, "s1", NATIVE, &content).await;
    let page = rig.ask("s1", None, Some(100), false).await.unwrap();
    let wire = serde_json::to_vec(&page).unwrap();
    assert!(wire.len() <= 512 * 1024, "{}", wire.len());
    assert!(
        page.items.len() >= 20 && page.items.len() < 100,
        "{}",
        page.items.len()
    );
    assert!(page.items.iter().all(|i| i.text.len() <= 8 * 1024));
    assert!(page.items[0].text.ends_with("… [cut]"));
    // The cursor goes on where the page stopped.
    let next = rig
        .ask("s1", page.before.as_deref(), Some(100), false)
        .await
        .unwrap();
    assert!(!next.items.is_empty());
    assert_ne!(next.items.last().unwrap().id, page.items[0].id);
}

#[tokio::test]
async fn a_cursor_of_another_snapshot_is_stale_and_a_bad_one_is_invalid() {
    let rig = Rig::new(ProviderKind::Claude).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, N)).await;
    let page = rig.ask("s1", None, Some(2), false).await.unwrap();
    let cursor = page.before.clone().unwrap();
    let (_, number) = cursor.split_once(':').unwrap();
    let stale = rig
        .ask(
            "s1",
            Some(&format!("0123456789abcdef:{number}")),
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, "stale_cursor");
    for bad in [
        "nonsense",
        "0123456789abcdef:",
        "xyz:1",
        "0123456789ABCDEF:1",
        "0123456789abcdef:-1",
    ] {
        let e = rig.ask("s1", Some(bad), None, false).await.unwrap_err();
        assert_eq!(e.code, "invalid_params", "{bad}");
    }
    let past = format!("{}:99", cursor.split_once(':').unwrap().0);
    assert_eq!(
        rig.ask("s1", Some(&past), None, false)
            .await
            .unwrap_err()
            .code,
        "invalid_params"
    );
    for limit in [0, 101] {
        let e = rig.ask("s1", None, Some(limit), false).await.unwrap_err();
        assert_eq!(e.code, "invalid_params", "{limit}");
    }
    let e = rig.ask("missing", None, None, false).await.unwrap_err();
    assert_eq!(e.code, "unknown_session");
}

#[tokio::test]
async fn a_member_answer_has_empty_tool_summaries() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let tool = serde_json::json!({"type":"assistant","uuid":"t","sessionId":NATIVE,
        "message":{"role":"assistant","content":[
            {"type":"tool_use","id":"x","name":"Bash","input":{"command":"secret --flag"}}]}})
    .to_string();
    imported(&rig, "s1", NATIVE, &[claude_user(1, "go"), tool]).await;
    let owner = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(
        owner.items[1].tool.as_ref().unwrap().summary,
        "secret --flag"
    );
    let member = rig.ask("s1", None, None, true).await.unwrap();
    let tool = member.items[1].tool.as_ref().unwrap();
    assert_eq!((tool.name.as_str(), tool.summary.as_str()), ("Bash", ""));
    // The cache does not leak: the owner still sees the summary.
    let again = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(
        again.items[1].tool.as_ref().unwrap().summary,
        "secret --flag"
    );
}

#[tokio::test]
async fn a_member_reads_an_import_snapshot_but_not_a_failed_capture() {
    let rig = Rig::new(ProviderKind::Claude).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, 2)).await;
    assert_eq!(
        rig.ask("s1", None, None, true).await.unwrap().items.len(),
        2
    );
    // A failed capture: unavailable for the owner and for a member.
    let sid = rig.session("s2", Some("no-file")).await;
    at_import(&rig.store, Some(&rig.policy), &rig.private, &sid, "no-file").await;
    for member in [false, true] {
        let a = rig.ask("s2", None, None, member).await.unwrap();
        let reason = if member {
            HistoryUnavailable::NotCaptured
        } else {
            HistoryUnavailable::NotFound
        };
        assert_eq!(a.state, HistoryState::Unavailable { reason });
    }
}

#[tokio::test]
async fn a_started_session_is_empty_without_reading_any_file() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    rig.first_effect("s1", "start", false).await;
    rig.process("s1", 1).await;
    let a = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(a.state, HistoryState::Empty);
    assert!(a.items.is_empty());
    assert!(rig.store.transcript("s1".into()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_session_without_a_native_id_has_no_history() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", None).await;
    rig.first_effect("s1", "resume", true).await;
    let a = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(
        a.state,
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NoNativeId
        }
    );
}

#[tokio::test]
async fn an_empty_snapshot_is_empty() {
    let rig = Rig::new(ProviderKind::Claude).await;
    imported(
        &rig,
        "s1",
        NATIVE,
        &[r#"{"type":"mode","sessionId":"x"}"#.replace('x', NATIVE)],
    )
    .await;
    // Only a record of another kind: nothing recognized.
    let a = rig.ask("s1", None, None, false).await.unwrap();
    assert!(matches!(a.state, HistoryState::Unavailable { .. }));
    // A snapshot with a conversation record that gives no item.
    let rig = Rig::new(ProviderKind::Claude).await;
    let thinking = serde_json::json!({"type":"assistant","uuid":"t","sessionId":NATIVE,
        "message":{"role":"assistant","content":[{"type":"thinking","thinking":"x"}]}})
    .to_string();
    imported(&rig, "s1", NATIVE, &[thinking]).await;
    let a = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!((a.state, a.items.len()), (HistoryState::Empty, 0));
}

#[tokio::test]
async fn the_snapshot_is_checked_when_the_index_is_built() {
    let rig = Rig::new(ProviderKind::Claude).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, 3)).await;
    let snapshot = rig.root.join("data/history/s1.jsonl");
    let good = std::fs::read(&snapshot).unwrap();
    let state = |a: fleetdeck_proto::history::SessionHistory| a.state;
    // Changed content of the same size.
    let mut bad = good.clone();
    bad[10] ^= 1;
    std::fs::write(&snapshot, &bad).unwrap();
    assert_eq!(
        state(rig.ask("s1", None, None, false).await.unwrap()),
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NotTrusted
        }
    );
    // Shorter.
    std::fs::write(&snapshot, &good[..good.len() - 5]).unwrap();
    assert_eq!(
        state(rig.ask("s1", None, None, false).await.unwrap()),
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NotTrusted
        }
    );
    // Missing.
    std::fs::remove_file(&snapshot).unwrap();
    assert_eq!(
        state(rig.ask("s1", None, None, false).await.unwrap()),
        HistoryState::Unavailable {
            reason: HistoryUnavailable::NotFound
        }
    );
    // Restored.
    std::fs::write(&snapshot, &good).unwrap();
    assert_eq!(
        rig.ask("s1", None, None, false).await.unwrap().items.len(),
        3
    );
}

#[tokio::test]
async fn an_index_over_the_item_cap_keeps_the_newest_with_absolute_cursors() {
    let limits = Limits {
        index_items: 3,
        ..Limits::default()
    };
    let rig = Rig::with_limits(ProviderKind::Claude, limits).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, 5)).await;
    let page = rig.ask("s1", None, Some(2), false).await.unwrap();
    assert_eq!(texts(&page), ["text 4", "text 5"]);
    assert_eq!(page.state, HistoryState::Partial { skipped: 2 });
    let cursor = page.before.clone().unwrap();
    assert!(cursor.ends_with(":3"), "{cursor}");
    let next = rig.ask("s1", Some(&cursor), Some(2), false).await.unwrap();
    assert_eq!(texts(&next), ["text 3"]);
    assert_eq!(next.before, None);
}

#[tokio::test]
async fn the_cache_holds_at_most_four_indexes() {
    let rig = Rig::new(ProviderKind::Claude).await;
    for n in 0..6 {
        let native = format!("native-{n}");
        let id = format!("s{n}");
        imported(&rig, &id, &native, &convo(&native, 2)).await;
        assert_eq!(
            rig.ask(&id, None, None, false).await.unwrap().items.len(),
            2
        );
    }
    assert_eq!(rig.history.cached(), 4);
    // An evicted index is rebuilt from the same snapshot.
    let again = rig.ask("s0", None, None, false).await.unwrap();
    assert_eq!(again.items.len(), 2);
}

fn slow(deadline_ms: u64) -> Limits {
    Limits {
        deadline: Duration::from_millis(deadline_ms),
        ..Limits::default()
    }
}

#[tokio::test]
async fn a_scan_past_the_deadline_is_busy_and_the_permit_stays_with_the_worker() {
    let rig = Rig::with_limits(ProviderKind::Claude, slow(200)).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, 3)).await;
    *rig.history.seams().scan_delay.lock().unwrap() = Some(Duration::from_millis(900));
    let started = std::time::Instant::now();
    let first = rig.ask("s1", None, None, false).await.unwrap_err();
    assert_eq!(first.code, "busy");
    assert!(started.elapsed() < Duration::from_millis(700));
    assert_eq!(rig.history.cached(), 0);
    assert_eq!(rig.history.shared.budget.available_permits(), 0);
    // The worker still holds the permit: a second request waits and times out
    // without a scan of its own.
    let second = rig.ask("s1", None, None, false).await.unwrap_err();
    assert_eq!(second.code, "busy");
    assert_eq!(
        rig.history
            .seams()
            .scans
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    // After the worker ends, a request works.
    *rig.history.seams().scan_delay.lock().unwrap() = None;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(rig.history.shared.budget.available_permits(), 1);
    let ok = rig.ask("s1", None, None, false).await.unwrap();
    assert_eq!(ok.items.len(), 3);
}

#[tokio::test]
async fn two_requests_for_one_snapshot_scan_it_once() {
    let rig = Rig::with_limits(ProviderKind::Claude, slow(5000)).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, 3)).await;
    *rig.history.seams().scan_delay.lock().unwrap() = Some(Duration::from_millis(300));
    let (a, b) = tokio::join!(
        rig.ask("s1", None, None, false),
        rig.ask("s1", None, None, false)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(
        rig.history
            .seams()
            .scans
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn a_page_reads_at_most_the_record_budget_but_at_least_one_item() {
    let limits = Limits {
        page_read_max: 600,
        ..Limits::default()
    };
    let rig = Rig::with_limits(ProviderKind::Claude, limits).await;
    imported(&rig, "s1", NATIVE, &convo(NATIVE, 6)).await;
    let page = rig.ask("s1", None, Some(6), false).await.unwrap();
    assert!(
        !page.items.is_empty() && page.items.len() < 6,
        "{}",
        page.items.len()
    );
    assert!(page.before.is_some());
}
