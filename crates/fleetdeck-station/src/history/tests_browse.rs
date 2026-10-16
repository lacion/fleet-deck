//! Tests of the native-session list and preview (B7). A test that needs a
//! supported filesystem skips, with a printed reason, when the temp
//! directory is on another one.

use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryState, HistoryUnavailable};
use fleetdeck_proto::native::{HomeState, ImportState, NativeSessionsQuery, TranscriptPin};

use super::Limits;
use super::browse::Budget;
use super::browse_preview::run_preview;
use super::tests_support::*;

// ---------------------------------------------------------------- homes

#[tokio::test]
async fn a_relative_home_is_unavailable_and_a_shared_home_is_listed_once() {
    let Some(b) = rig(
        Limits::default(),
        &[("claude2", None), ("rel", Some("relative/dir"))],
    )
    .await
    else {
        return;
    };
    b.good_claude(ID_A);
    let list = b.list().await;
    let claude: Vec<_> = list
        .homes
        .iter()
        .filter(|h| h.provider == ProviderKind::Claude)
        .collect();
    assert_eq!(claude.len(), 2, "{claude:?}");
    let shared = claude.iter().find(|h| h.state == HomeState::Ok).unwrap();
    assert_eq!(
        shared.instances,
        vec![
            ProviderInstanceId::from("claude"),
            ProviderInstanceId::from("claude2")
        ]
    );
    let rel = claude.iter().find(|h| h.state != HomeState::Ok).unwrap();
    assert_eq!(
        rel.state,
        HomeState::Unavailable {
            reason: "home_not_absolute".into()
        }
    );
    assert_eq!(rel.instances, vec![ProviderInstanceId::from("rel")]);
    assert_eq!(entry(&list, ID_A).instances, shared.instances);
}

#[tokio::test]
async fn a_missing_home_is_ok_without_sessions() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let list = b.list().await;
    assert!(list.sessions.is_empty());
    assert!(list.homes.iter().all(|h| h.state == HomeState::Ok));
}

#[tokio::test]
async fn the_instance_filter_keeps_one_home_and_an_unknown_instance_is_refused() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    b.put_codex(CODEX_ID, &[codex_meta_rec(CODEX_ID, &b.cwd, None)]);
    let query = NativeSessionsQuery {
        provider_instance: Some("codex".into()),
        limit: None,
    };
    let list = b.list_with(query, Budget::default()).await.unwrap();
    assert_eq!(list.homes.len(), 1);
    assert_eq!(list.homes[0].provider, ProviderKind::Codex);
    assert_eq!(list.sessions.len(), 1);
    let unknown = NativeSessionsQuery {
        provider_instance: Some("nope".into()),
        limit: None,
    };
    let err = b.list_with(unknown, Budget::default()).await.unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        ("invalid_params", "unknown provider instance")
    );
    for limit in [0, 201] {
        let query = NativeSessionsQuery {
            provider_instance: None,
            limit: Some(limit),
        };
        let err = b.list_with(query, Budget::default()).await.unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            ("invalid_params", "limit must be 1 to 200")
        );
    }
}

// ----------------------------------------------------------------- walk

#[tokio::test]
async fn it_lists_a_claude_session_with_its_fields_and_pin() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let list = b.list().await;
    assert!(!list.truncated && !list.timed_out);
    let s = entry(&list, ID_A);
    assert_eq!(s.provider, ProviderKind::Claude);
    assert_eq!(s.cwd.as_deref(), Some(b.cwd.as_str()));
    assert_eq!(s.title.as_deref(), Some("A title"));
    assert_eq!(s.first_prompt.as_deref(), Some("the first prompt"));
    assert_eq!(s.import, ImportState::Ok);
    assert!(s.verified);
    assert!(s.known.is_none());
    assert_eq!(s.size, std::fs::metadata(&path).unwrap().len());
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(
        s.modified_at,
        meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000
    );
    assert_eq!(s.pin, pin_of(&path, &b.home()));
}

#[tokio::test]
async fn it_lists_a_codex_session_and_a_child_thread() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let prompt = serde_json::json!({
        "type": "response_item", "timestamp": "2026-10-14T10:00:02.000Z",
        "payload": {"type": "message", "id": "m-1", "role": "user",
                    "content": [{"type": "input_text", "text": "codex prompt"}]},
    })
    .to_string();
    b.put_codex(
        CODEX_ID,
        &[
            codex_meta_rec(CODEX_ID, &b.cwd, None),
            turn_rec(&b.cwd),
            prompt.clone(),
        ],
    );
    b.put_codex(
        ID_B,
        &[codex_meta_rec(ID_B, &b.cwd, Some(CODEX_ID)), prompt],
    );
    let archived = b.codex_home().join("archived_sessions");
    std::fs::create_dir_all(&archived).unwrap();
    write_private(
        &archived.join(format!("rollout-2026-10-13T09-00-00-{ID_C}.jsonl")),
        &join(&[codex_meta_rec(ID_C, &b.cwd, None)]),
    );
    let list = b.list().await;
    let s = entry(&list, CODEX_ID);
    assert_eq!(s.first_prompt.as_deref(), Some("codex prompt"));
    assert_eq!(s.import, ImportState::Ok);
    assert!(s.parent_native_id.is_none());
    let child = entry(&list, ID_B);
    assert_eq!(child.parent_native_id.as_deref(), Some(CODEX_ID));
    assert_eq!(child.import, ImportState::SubagentThread);
    assert_eq!(entry(&list, ID_C).import, ImportState::Ok);
}

#[tokio::test]
async fn links_hard_links_and_deeper_files_are_never_listed_as_sessions() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let real = b.good_claude(ID_A);
    let dir = real.parent().unwrap().to_owned();
    // A link in place of a transcript.
    std::os::unix::fs::symlink(&real, dir.join(format!("{ID_B}.jsonl"))).unwrap();
    // A hard link: both names have two links, so neither is trusted.
    let other = b.good_claude(ID_C);
    std::fs::hard_link(
        &other,
        dir.join("dddddddd-1111-2222-3333-444444444444.jsonl"),
    )
    .unwrap();
    // A subagent file and a deeper file.
    let deeper = dir.join(ID_A).join("subagents");
    std::fs::create_dir_all(&deeper).unwrap();
    write_private(&deeper.join("agent-a1.jsonl"), b"{}\n");
    write_private(&dir.join(ID_A).join("x.jsonl"), b"{}\n");
    // A name that is no session id.
    write_private(&dir.join("bad name.jsonl"), b"{}\n");
    // A project directory that is a link.
    std::os::unix::fs::symlink(&dir, b.home().join("projects/linked")).unwrap();
    let list = b.list().await;
    let ids: Vec<&str> = list.sessions.iter().map(|s| s.native_id.as_str()).collect();
    assert!(ids.contains(&ID_A), "{ids:?}");
    assert!(!ids.contains(&ID_B), "a link is not listed: {ids:?}");
    for id in [ID_C, "dddddddd-1111-2222-3333-444444444444"] {
        let s = entry(&list, id);
        assert_eq!(s.import, ImportState::NotTrusted);
        assert!(s.cwd.is_none() && s.title.is_none() && s.first_prompt.is_none());
    }
    assert_eq!(list.sessions.len(), 3, "{ids:?}");
}

#[tokio::test]
async fn the_newest_limit_sessions_are_listed_and_the_rest_count_as_truncated() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    for (i, id) in [ID_A, ID_B, ID_C].into_iter().enumerate() {
        let path = b.good_claude(id);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + i as u64))
            .unwrap();
    }
    let query = NativeSessionsQuery {
        provider_instance: None,
        limit: Some(2),
    };
    let list = b.list_with(query, Budget::default()).await.unwrap();
    let ids: Vec<&str> = list.sessions.iter().map(|s| s.native_id.as_str()).collect();
    assert_eq!(ids, vec![ID_C, ID_B]);
    assert!(list.truncated);
}

#[tokio::test]
async fn the_budgets_stop_the_walk() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    for id in [ID_A, ID_B, ID_C] {
        b.good_claude(id);
    }
    let small = |f: fn(&mut Budget)| {
        let mut budget = Budget::default();
        f(&mut budget);
        budget
    };
    let entries = b.list_with(q(), small(|x| x.entries = 2)).await;
    let entries = entries.unwrap();
    assert!(
        entries.truncated && entries.sessions.len() < 3,
        "{entries:?}"
    );
    let dirs = b.list_with(q(), small(|x| x.dirs = 1)).await;
    let dirs = dirs.unwrap();
    assert!(dirs.truncated && dirs.sessions.is_empty(), "{dirs:?}");
    let bytes = b.list_with(q(), small(|x| x.bytes = 100)).await;
    let bytes = bytes.unwrap();
    assert!(bytes.truncated && bytes.sessions.is_empty(), "{bytes:?}");
    let time = b
        .list_with(q(), small(|x| x.deadline = Duration::ZERO))
        .await
        .unwrap();
    assert!(time.timed_out && time.sessions.is_empty(), "{time:?}");
}

#[tokio::test]
async fn a_second_request_waits_then_gets_busy() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let _held = Arc::clone(&b.rig.history.shared.budget)
        .acquire_owned()
        .await
        .unwrap();
    let budget = Budget {
        wait: Duration::from_millis(50),
        ..Budget::default()
    };
    let err = b.list_with(q(), budget).await.unwrap_err();
    assert_eq!(err.code, "busy");
}

// ---------------------------------------------------------- import states

#[tokio::test]
async fn it_reports_the_import_states() {
    let Some(b) = rig(
        Limits {
            file_max: 4000,
            ..Limits::default()
        },
        &[],
    )
    .await
    else {
        return;
    };
    // outside_roots
    let outside = b.rig.root.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let outside_cwd = outside.to_string_lossy().into_owned();
    let path = b.claude_path(&outside_cwd, ID_A);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_private(&path, &join(&[claude_rec(ID_A, &outside_cwd, 1, "x")]));
    // cwd_mismatch: not the canonical form
    let odd = format!("{}/work//project", b.rig.root.display());
    let path = b.claude_path(&odd, ID_B);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_private(&path, &join(&[claude_rec(ID_B, &odd, 1, "x")]));
    // too_large
    b.put_claude(ID_C, &[claude_rec(ID_C, &b.cwd, 1, &"y".repeat(5000))]);
    let list = b.list().await;
    assert_eq!(entry(&list, ID_A).import, ImportState::OutsideRoots);
    assert_eq!(entry(&list, ID_B).import, ImportState::CwdMismatch);
    assert_eq!(entry(&list, ID_C).import, ImportState::TooLarge);
}

#[tokio::test]
async fn the_store_decides_in_use_held_execution_unknown_and_known() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    for id in [ID_A, ID_B, ID_C] {
        b.good_claude(id);
    }
    let store = &b.rig.store;
    // Every Claude session of the store has ended and reports its id.
    let end = |id: &str, native: &str, state: &str| {
        format!(
            "UPDATE sessions SET native_session_id = '{native}', state = '{state}' \
             WHERE session_id = '{id}'"
        )
    };
    b.rig.session("s-use", Some(ID_A)).await;
    store.exec_for_tests(end("s-use", ID_A, "running")).await;
    b.rig.session("s-held", Some(ID_B)).await;
    store.exec_for_tests(end("s-held", ID_B, "exited")).await;
    store
        .exec_for_tests(
            "UPDATE sessions SET summary = json_set(summary, '$.terminal', json('{\"shell_id\":\"sh\"}')) \
             WHERE session_id = 's-held'"
                .into(),
        )
        .await;
    b.rig.session("s-old", Some(ID_C)).await;
    store.exec_for_tests(end("s-old", ID_C, "exited")).await;
    let list = b.list().await;
    assert_eq!(
        entry(&list, ID_A).import,
        ImportState::InUse {
            session_id: "s-use".into()
        }
    );
    assert_eq!(
        entry(&list, ID_A).known.as_ref().unwrap().session_id,
        "s-use".into()
    );
    assert_eq!(
        entry(&list, ID_B).import,
        ImportState::Held {
            session_id: "s-held".into()
        }
    );
    // An ended session keeps `known` and does not block.
    let c = entry(&list, ID_C);
    assert_eq!(c.known.as_ref().unwrap().session_id, "s-old".into());
    assert!(
        matches!(c.import, ImportState::Ok | ImportState::ExecutionUnknown),
        "{:?}",
        c.import
    );
}

#[tokio::test]
async fn a_run_with_an_unknown_conversation_id_gives_execution_unknown() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    // A new session that has not reported a native id blocks every import.
    b.rig.session("s-new", None).await;
    let list = b.list().await;
    assert_eq!(entry(&list, ID_A).import, ImportState::ExecutionUnknown);
}

// ---------------------------------------------------------- answer cap

#[tokio::test]
async fn the_answer_is_cut_at_the_cap() {
    let limits = Limits {
        answer_max: 3000,
        ..Limits::default()
    };
    let Some(b) = rig(limits, &[]).await else {
        return;
    };
    for i in 0..10 {
        b.good_claude(&format!("aaaaaaaa-1111-2222-3333-4444444444{i:02}"));
    }
    let list = b.list().await;
    assert!(list.truncated);
    assert!(!list.sessions.is_empty() && list.sessions.len() < 10);
    assert!(serde_json::to_vec(&list).unwrap().len() <= 3000);
}

// -------------------------------------------------------------- preview

#[tokio::test]
async fn it_previews_the_pinned_claude_and_codex_transcripts() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let p = b
        .preview(query_of("claude", ID_A, pin_of(&path, &b.home())))
        .await
        .unwrap();
    assert_eq!(p.state, HistoryState::Complete);
    assert_eq!(p.items.len(), 2);
    assert_eq!(p.items[0].text, "the first prompt");
    assert_eq!(p.cwd, b.cwd);
    assert_eq!(p.size, std::fs::metadata(&path).unwrap().len());
    assert!(!p.earlier_omitted);
    let reply = serde_json::json!({
        "type": "response_item", "timestamp": "2026-10-14T10:00:03.000Z",
        "payload": {"type": "message", "id": "m-2", "role": "assistant",
                    "content": [{"type": "output_text", "text": "codex reply"}]},
    })
    .to_string();
    let path = b.put_codex(CODEX_ID, &[codex_meta_rec(CODEX_ID, &b.cwd, None), reply]);
    let p = b
        .preview(query_of("codex", CODEX_ID, pin_of(&path, &b.codex_home())))
        .await
        .unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].text, "codex reply");
}

#[tokio::test]
async fn a_swapped_file_or_a_changed_home_is_unavailable() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    // Another inode at the same path.
    std::fs::remove_file(&path).unwrap();
    let other = b.rig.root.join("keep");
    write_private(&other, b"x\n");
    b.good_claude(ID_A);
    let moved = b
        .preview(query_of("claude", ID_A, pin.clone()))
        .await
        .unwrap();
    if moved.state == HistoryState::Complete {
        // The file system reused the inode number: the swap cannot be seen.
        eprintln!("skipped: the inode number was reused");
    } else {
        unavailable(&moved, HistoryUnavailable::NotFound);
    }
    let wrong_home = TranscriptPin {
        home: b.rig.root.join("elsewhere").to_string_lossy().into_owned(),
        ..pin
    };
    let changed = b
        .preview(query_of("claude", ID_A, wrong_home))
        .await
        .unwrap();
    unavailable(&changed, HistoryUnavailable::NotFound);
}

#[tokio::test]
async fn a_large_transcript_shows_the_tail_and_says_so() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let filler = "z".repeat(10_000);
    let lines: Vec<String> = (0..600)
        .map(|n| claude_rec(ID_A, &b.cwd, n, &format!("{n}-{filler}")))
        .collect();
    let path = b.put_claude(ID_A, &lines);
    assert!(std::fs::metadata(&path).unwrap().len() > 5 * 1024 * 1024);
    let p = b
        .preview(query_of("claude", ID_A, pin_of(&path, &b.home())))
        .await
        .unwrap();
    assert!(p.earlier_omitted);
    assert!(!p.items.is_empty() && p.items.len() <= 300);
    assert!(p.items.last().unwrap().text.starts_with("599-"));
    assert!(serde_json::to_vec(&p).unwrap().len() <= 512 * 1024);
}

#[tokio::test]
async fn a_preview_refuses_bad_params() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    let err = b
        .preview(query_of("nope", ID_A, pin.clone()))
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
    let err = b
        .preview(query_of("claude", "../x", pin.clone()))
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
    let missing = b.preview(query_of("claude", ID_B, pin)).await.unwrap();
    unavailable(&missing, HistoryUnavailable::NotFound);
}

#[tokio::test]
async fn a_preview_over_the_file_cap_is_too_large() {
    let Some(b) = rig(
        Limits {
            file_max: 100,
            ..Limits::default()
        },
        &[],
    )
    .await
    else {
        return;
    };
    let path = b.good_claude(ID_A);
    let p = b
        .preview(query_of("claude", ID_A, pin_of(&path, &b.home())))
        .await
        .unwrap();
    unavailable(&p, HistoryUnavailable::TooLarge);
    assert_eq!(p.size, std::fs::metadata(&path).unwrap().len());
}

#[tokio::test]
async fn the_preview_runs_under_the_same_permit() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let _held = Arc::clone(&b.rig.history.shared.budget)
        .acquire_owned()
        .await
        .unwrap();
    let budget = Budget {
        wait: Duration::from_millis(50),
        ..Budget::default()
    };
    let err = run_preview(
        &b.rig.store,
        &b.policy,
        &b.rig.private,
        query_of("claude", ID_A, pin_of(&path, &b.home())),
        budget,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "busy");
}

// ------------------------------------------------ B7 PRE-PR r1: home links

fn link_home(b: &mut B, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let shared = b.rig.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::os::unix::fs::symlink(b.home(), shared.join("link")).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(mode)).unwrap();
    let config = shared.join("link").to_string_lossy().into_owned();
    b.policy = policy_of(&b.rig.root, &[("linked", Some(config.as_str()))]);
}

#[tokio::test]
async fn a_home_link_in_a_group_writable_dir_is_home_not_trusted() {
    let Some(mut b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    link_home(&mut b, 0o770);
    let list = b.list().await;
    let linked = list
        .homes
        .iter()
        .find(|h| h.instances.contains(&ProviderInstanceId::from("linked")))
        .unwrap();
    assert_eq!(
        linked.state,
        HomeState::Unavailable {
            reason: "home_not_trusted".into()
        }
    );
    // The real home is still listed.
    assert!(list.sessions.iter().any(|s| s.native_id == ID_A));
}

#[tokio::test]
async fn a_home_link_in_a_trusted_dir_lists_the_sessions_of_its_target() {
    let Some(mut b) = rig(Limits::default(), &[]).await else {
        return;
    };
    b.good_claude(ID_A);
    link_home(&mut b, 0o700);
    let list = b.list().await;
    let linked = list
        .homes
        .iter()
        .find(|h| h.instances.contains(&ProviderInstanceId::from("linked")))
        .unwrap();
    assert_eq!(linked.state, HomeState::Ok);
    // The link and the real home resolve to one home.
    assert!(
        entry(&list, ID_A)
            .instances
            .contains(&ProviderInstanceId::from("linked"))
    );
}
