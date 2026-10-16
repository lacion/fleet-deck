//! Tests of the import checks before a launch (B7, design §2.4). A test
//! that needs a supported filesystem skips, with a printed reason, when the
//! temp directory is on another one.

use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::native::TranscriptPin;

use super::Limits;
use super::at_import;
use super::import_check::{BUSY, CHANGED, Refusal, SUBAGENT, before_import};
use super::tests_support::*;
use crate::store::transcripts::Capture;
use crate::store::{NewSession, OfflineApprovalPolicy};

const CODEX_CHILD: &str = "01a13f28-e384-7ed1-aee0-423548cce999";

async fn claude_session(b: &B, id: &str, native: &str) -> SessionId {
    b.rig.session(id, Some(native)).await
}

async fn codex_session(b: &B, id: &str, native: &str) -> SessionId {
    b.rig
        .store
        .create_session(NewSession {
            session_id: id.into(),
            provider: ProviderKind::Codex,
            provider_instance: "codex".into(),
            cwd: b.cwd.clone(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    b.rig
        .store
        .exec_for_tests(format!(
            "UPDATE sessions SET native_session_id = '{native}' WHERE session_id = '{id}'"
        ))
        .await;
    id.into()
}

async fn check(
    b: &B,
    session: &SessionId,
    native: &str,
    pin: Option<&TranscriptPin>,
) -> Result<(), Refusal> {
    before_import(
        &b.rig.store,
        Some(&b.policy),
        &b.rig.private,
        session,
        native,
        &b.cwd,
        pin,
    )
    .await
}

fn codex_lines(id: &str, cwd: &str, parent: Option<&str>) -> Vec<String> {
    vec![
        codex_meta_rec(id, cwd, parent),
        turn_rec(cwd),
        codex_message(2, "user", "hello"),
    ]
}

#[tokio::test]
async fn a_pinned_import_of_the_right_file_passes_and_is_captured_once() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    let sid = claude_session(&b, "s1", ID_A).await;
    assert_eq!(check(&b, &sid, ID_A, Some(&pin)).await, Ok(()));
    let row = b.rig.store.transcript(sid.clone()).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Import);
    let held = std::fs::read(row.snapshot.clone().unwrap()).unwrap();
    assert_eq!(held, std::fs::read(&path).unwrap());
    // A second run (a redelivery) verifies again and keeps the first capture.
    assert_eq!(check(&b, &sid, ID_A, Some(&pin)).await, Ok(()));
    let again = b.rig.store.transcript(sid.clone()).await.unwrap().unwrap();
    assert_eq!(again.prefix_sha256, row.prefix_sha256);
    // The later `at_import` does nothing: the row is there.
    at_import(&b.rig.store, Some(&b.policy), &b.rig.private, &sid, ID_A).await;
    let last = b.rig.store.transcript(sid).await.unwrap().unwrap();
    assert_eq!(last.capture, Capture::Import);
}

#[tokio::test]
async fn a_swapped_file_fails_a_pinned_import_and_captures_nothing() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    // Another file (another inode) takes the place of the listed one.
    let copy = path.with_extension("copy");
    std::fs::copy(&path, &copy).unwrap();
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(&copy, &path).unwrap();
    assert_ne!(pin_of(&path, &b.home()).ino, pin.ino);
    let sid = claude_session(&b, "s1", ID_A).await;
    let refused = check(&b, &sid, ID_A, Some(&pin)).await;
    assert_eq!(refused, Err(Refusal::Unverified));
    assert_eq!(refused.unwrap_err().message(), CHANGED);
    assert!(b.rig.store.transcript(sid).await.unwrap().is_none());
}

#[tokio::test]
async fn a_changed_home_fails_a_pinned_import() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = TranscriptPin {
        home: b.rig.root.join(".other").to_string_lossy().into_owned(),
        ..pin_of(&path, &b.home())
    };
    let sid = claude_session(&b, "s1", ID_A).await;
    assert_eq!(
        check(&b, &sid, ID_A, Some(&pin)).await,
        Err(Refusal::Unverified)
    );
}

#[tokio::test]
async fn a_cwd_with_a_group_writable_parent_fails_a_pinned_import() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    let sid = claude_session(&b, "s1", ID_A).await;
    assert_eq!(check(&b, &sid, ID_A, Some(&pin)).await, Ok(()));
    let work = b.rig.root.join("work");
    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o775)).unwrap();
    // Another session: the first one holds a row already.
    let second = claude_session(&b, "s2", ID_A).await;
    assert_eq!(
        check(&b, &second, ID_A, Some(&pin)).await,
        Err(Refusal::Unverified)
    );
    assert!(b.rig.store.transcript(second).await.unwrap().is_none());
}

#[tokio::test]
async fn a_contradiction_in_the_head_window_fails_a_pinned_import() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.put_claude(
        ID_A,
        &[
            claude_rec(ID_A, &b.cwd, 1, "one"),
            claude_rec(ID_B, &b.cwd, 2, "two"),
        ],
    );
    let pin = pin_of(&path, &b.home());
    let sid = claude_session(&b, "s1", ID_A).await;
    assert_eq!(
        check(&b, &sid, ID_A, Some(&pin)).await,
        Err(Refusal::Unverified)
    );
    assert!(b.rig.store.transcript(sid).await.unwrap().is_none());
}

#[tokio::test]
async fn a_transcript_that_names_another_cwd_fails_a_pinned_import() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.put_codex(CODEX_ID, &codex_lines(CODEX_ID, "/elsewhere/app", None));
    let pin = pin_of(&path, &b.codex_home());
    let sid = codex_session(&b, "s1", CODEX_ID).await;
    assert_eq!(
        check(&b, &sid, CODEX_ID, Some(&pin)).await,
        Err(Refusal::Unverified)
    );
}

#[tokio::test]
async fn a_failed_capture_fails_a_pinned_import_but_not_an_unpinned_one() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    // The provider rewrites a byte after every copy pass.
    let flip = {
        let path = path.clone();
        Arc::new(move |attempt: u32| {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(3)).unwrap();
            f.write_all(&[b'Z' - attempt as u8]).unwrap();
        })
    };
    *b.rig.history.seams().after_copy.lock().unwrap() = Some(flip);
    let pinned = claude_session(&b, "s1", ID_A).await;
    assert_eq!(
        check(&b, &pinned, ID_A, Some(&pin)).await,
        Err(Refusal::Unverified)
    );
    assert!(b.rig.store.transcript(pinned).await.unwrap().is_none());
    // Without a pin: the check passes, and the best-effort capture records the failure.
    let plain = claude_session(&b, "s2", ID_A).await;
    assert_eq!(check(&b, &plain, ID_A, None).await, Ok(()));
    at_import(&b.rig.store, Some(&b.policy), &b.rig.private, &plain, ID_A).await;
    let row = b.rig.store.transcript(plain).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Failed);
}

#[tokio::test]
async fn a_codex_child_thread_fails_both_branches() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.put_codex(
        CODEX_CHILD,
        &codex_lines(CODEX_CHILD, &b.cwd, Some(CODEX_ID)),
    );
    let pin = pin_of(&path, &b.codex_home());
    let pinned = codex_session(&b, "s1", CODEX_CHILD).await;
    let refused = check(&b, &pinned, CODEX_CHILD, Some(&pin)).await;
    assert_eq!(refused, Err(Refusal::Subagent));
    assert_eq!(refused.unwrap_err().message(), SUBAGENT);
    let plain = codex_session(&b, "s2", CODEX_CHILD).await;
    assert_eq!(
        check(&b, &plain, CODEX_CHILD, None).await,
        Err(Refusal::Subagent)
    );
    // The rule comes first: a wrong pin still gives the child-thread text.
    let wrong = TranscriptPin {
        ino: pin.ino + 1,
        ..pin
    };
    let third = codex_session(&b, "s3", CODEX_CHILD).await;
    assert_eq!(
        check(&b, &third, CODEX_CHILD, Some(&wrong)).await,
        Err(Refusal::Subagent)
    );
}

#[tokio::test]
async fn a_codex_parent_thread_and_a_missing_transcript_do_not_trigger_the_rule() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.put_codex(CODEX_ID, &codex_lines(CODEX_ID, &b.cwd, None));
    let pin = pin_of(&path, &b.codex_home());
    let pinned = codex_session(&b, "s1", CODEX_ID).await;
    assert_eq!(check(&b, &pinned, CODEX_ID, Some(&pin)).await, Ok(()));
    let row = b.rig.store.transcript(pinned).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Import);
    let plain = codex_session(&b, "s2", CODEX_ID).await;
    assert_eq!(check(&b, &plain, CODEX_ID, None).await, Ok(()));
    // No transcript at all: an unpinned import goes on, a pinned one fails.
    let gone = "01a13f28-e384-7ed1-aee0-4235480000aa";
    let missing = codex_session(&b, "s3", gone).await;
    assert_eq!(check(&b, &missing, gone, None).await, Ok(()));
    assert_eq!(
        check(&b, &missing, gone, Some(&pin)).await,
        Err(Refusal::Unverified)
    );
}

// ------------------------------------------- B7 PRE-PR r1: home links, size

/// A session of the instance `linked`, whose Claude home is the link.
async fn linked_session(b: &B, id: &str, native: &str) -> SessionId {
    b.rig
        .store
        .create_session(NewSession {
            session_id: id.into(),
            provider: ProviderKind::Claude,
            provider_instance: "linked".into(),
            cwd: b.cwd.clone(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    b.rig
        .store
        .exec_for_tests(format!(
            "UPDATE sessions SET native_session_id = '{native}' WHERE session_id = '{id}'"
        ))
        .await;
    id.into()
}

/// Gives `b` an instance `linked` whose home is a link in `shared/` (with
/// the given mode) to the real Claude home.
fn link_home(b: &mut B, mode: u32) {
    let shared = b.rig.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::os::unix::fs::symlink(b.home(), shared.join("link")).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(mode)).unwrap();
    let config = shared.join("link").to_string_lossy().into_owned();
    b.policy = policy_of(&b.rig.root, &[("linked", Some(config.as_str()))]);
}

#[tokio::test]
async fn a_home_link_in_a_group_writable_dir_fails_a_pinned_import() {
    let Some(mut b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    link_home(&mut b, 0o770);
    let sid = linked_session(&b, "s1", ID_A).await;
    let refused = check(&b, &sid, ID_A, Some(&pin)).await;
    assert_eq!(refused, Err(Refusal::Unverified));
    assert_eq!(refused.unwrap_err().message(), CHANGED);
    assert!(b.rig.store.transcript(sid).await.unwrap().is_none());
}

#[tokio::test]
async fn a_pinned_import_through_a_trusted_home_link_passes() {
    let Some(mut b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let path = b.good_claude(ID_A);
    let pin = pin_of(&path, &b.home());
    link_home(&mut b, 0o700);
    let sid = linked_session(&b, "s1", ID_A).await;
    assert_eq!(check(&b, &sid, ID_A, Some(&pin)).await, Ok(()));
    let row = b.rig.store.transcript(sid).await.unwrap().unwrap();
    assert_eq!(row.capture, Capture::Import);
}

#[tokio::test]
async fn an_unpinned_codex_child_rollout_over_the_capture_cap_fails_with_the_subagent_text() {
    let limits = Limits {
        file_max: 64,
        ..Limits::default()
    };
    let Some(b) = rig(limits, &[]).await else {
        return;
    };
    let path = b.put_codex(
        CODEX_CHILD,
        &codex_lines(CODEX_CHILD, &b.cwd, Some(CODEX_ID)),
    );
    assert!(std::fs::metadata(&path).unwrap().len() > 64);
    let plain = codex_session(&b, "s1", CODEX_CHILD).await;
    let refused = check(&b, &plain, CODEX_CHILD, None).await;
    assert_eq!(refused, Err(Refusal::Subagent));
    assert_eq!(refused.unwrap_err().message(), SUBAGENT);
    // A pinned import of the same file gets the subagent text too.
    let pin = pin_of(&path, &b.codex_home());
    let pinned = codex_session(&b, "s2", CODEX_CHILD).await;
    assert_eq!(
        check(&b, &pinned, CODEX_CHILD, Some(&pin)).await,
        Err(Refusal::Subagent)
    );
}

#[tokio::test]
async fn a_codex_parent_rollout_over_the_capture_cap_is_not_a_child() {
    let limits = Limits {
        file_max: 64,
        ..Limits::default()
    };
    let Some(b) = rig(limits, &[]).await else {
        return;
    };
    let path = b.put_codex(CODEX_ID, &codex_lines(CODEX_ID, &b.cwd, None));
    let pin = pin_of(&path, &b.codex_home());
    // Unpinned: the best-effort rule goes on. Pinned: too large fails as before.
    let plain = codex_session(&b, "s1", CODEX_ID).await;
    assert_eq!(check(&b, &plain, CODEX_ID, None).await, Ok(()));
    let pinned = codex_session(&b, "s2", CODEX_ID).await;
    assert_eq!(
        check(&b, &pinned, CODEX_ID, Some(&pin)).await,
        Err(Refusal::Unverified)
    );
}

// ----------------------------------------------- B7: the check fails closed

#[tokio::test]
async fn an_unpinned_codex_import_is_refused_when_the_history_worker_is_busy() {
    let limits = Limits {
        deadline: Duration::from_millis(200),
        ..Limits::default()
    };
    let Some(b) = rig(limits, &[]).await else {
        return;
    };
    b.put_codex(CODEX_ID, &codex_lines(CODEX_ID, &b.cwd, None));
    let plain = codex_session(&b, "s1", CODEX_ID).await;
    // Another request holds the only permit past the deadline.
    let permit = Arc::clone(&b.rig.history.shared.budget)
        .acquire_owned()
        .await
        .unwrap();
    let refused = check(&b, &plain, CODEX_ID, None).await;
    assert_eq!(refused, Err(Refusal::Busy));
    let busy = refused.unwrap_err();
    assert_eq!(busy.message(), BUSY);
    assert_eq!(busy.code(), "import_check_busy");
    drop(permit);
    // With the permit free the same import goes on.
    assert_eq!(check(&b, &plain, CODEX_ID, None).await, Ok(()));
}

#[tokio::test]
async fn an_unpinned_codex_import_whose_check_hits_the_deadline_is_refused() {
    let limits = Limits {
        deadline: Duration::ZERO,
        ..Limits::default()
    };
    let Some(b) = rig(limits, &[]).await else {
        return;
    };
    b.put_codex(CODEX_ID, &codex_lines(CODEX_ID, &b.cwd, None));
    let plain = codex_session(&b, "s1", CODEX_ID).await;
    assert_eq!(check(&b, &plain, CODEX_ID, None).await, Err(Refusal::Busy));
}

#[tokio::test]
async fn a_missing_transcript_still_lets_an_unpinned_codex_import_go_on() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let gone = "01a13f28-e384-7ed1-aee0-4235480000bb";
    let missing = codex_session(&b, "s1", gone).await;
    assert_eq!(check(&b, &missing, gone, None).await, Ok(()));
}
