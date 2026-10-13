//! Hibernation (R6.6, DL-169) against `fake-claude sim`: an idle session
//! that nothing touched for the configured time is stopped gracefully with
//! `session.hibernated`, and the resume brings it back.

use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::capability::Capabilities;
use fleetdeck_proto::session::OpenRequest;

use super::*;
use crate::manager::hibernate;

const AFTER: u32 = 30;
const MIN_MS: i64 = 60_000;

fn idle_summary() -> SessionSummary {
    let mut s = SessionSummary::new(SessionId::from("s1".to_owned()));
    s.state = SessionState::Running;
    let mut caps = Capabilities::minimal();
    caps.resume = true;
    s.capabilities = Some(caps);
    s.updated_at = 1_000;
    s
}

#[test]
fn only_an_idle_resumable_session_that_nothing_touched_is_a_candidate() {
    let s = idle_summary();
    let late = 1_000 + i64::from(AFTER) * MIN_MS;
    assert!(hibernate::is_candidate(&s, late, AFTER));
    assert!(!hibernate::is_candidate(&s, late - 1, AFTER), "too early");
    assert!(!hibernate::is_candidate(&s, late, 0), "off");

    let mut busy = s.clone();
    busy.current_run = Some("r1".into());
    assert!(!hibernate::is_candidate(&busy, late, AFTER));
    busy.activity = fleetdeck_proto::session::Activity::Working;
    assert!(!hibernate::is_candidate(&busy, late, AFTER));

    let mut s2 = s.clone();
    s2.background_tasks = vec!["bg".to_owned()];
    assert!(
        !hibernate::is_candidate(&s2, late, AFTER),
        "background work"
    );

    let mut s3 = s.clone();
    s3.state = SessionState::Exited;
    assert!(!hibernate::is_candidate(&s3, late, AFTER), "ended");

    let mut s4 = s.clone();
    s4.hibernated = true;
    assert!(
        !hibernate::is_candidate(&s4, late, AFTER),
        "already hibernated"
    );

    let mut s5 = s.clone();
    s5.capabilities = Some(Capabilities::minimal());
    assert!(!hibernate::is_candidate(&s5, late, AFTER), "cannot resume");

    let mut s6 = s;
    s6.open_requests = Vec::<OpenRequest>::new();
    s6.terminal = Some(fleetdeck_proto::session::TerminalHold {
        shell_id: "sh".to_owned(),
        pid: None,
        since: 0,
        reason: None,
    });
    assert!(
        !hibernate::is_candidate(&s6, late, AFTER),
        "a terminal holds it"
    );
}

#[tokio::test]
async fn an_idle_session_hibernates_once_and_resumes() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("turn done", |e| {
        matches!(
            e,
            Event::TurnCompleted {
                status: TurnStatus::Done,
                ..
            }
        )
        .then_some(())
    })
    .await;
    st.wait_for("run completed", |e| {
        matches!(e, Event::RunCompleted { .. }).then_some(())
    })
    .await;
    let store = st.manager.store().clone();
    let policy = st.manager.policy().clone();

    // Too early: nothing stops.
    assert_eq!(hibernate::sweep(&store, &policy, AFTER).await.unwrap(), 0);
    assert_eq!(st.state(&s).await, SessionState::Running);

    st.det.advance(Duration::from_millis(
        u64::try_from(i64::from(AFTER) * MIN_MS).unwrap() + 1,
    ));
    assert_eq!(hibernate::sweep(&store, &policy, AFTER).await.unwrap(), 1);
    st.wait_for("hibernated", |e| {
        matches!(e, Event::SessionHibernated { after_min } if *after_min == AFTER).then_some(())
    })
    .await;
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    let summary = store.summary(s.clone()).await.unwrap().unwrap();
    assert!(summary.hibernated);
    assert_eq!(summary.state, SessionState::Exited);

    // The ended session is no candidate: a second sweep stores nothing.
    assert_eq!(hibernate::sweep(&store, &policy, AFTER).await.unwrap(), 0);

    // The resume works on a hibernated session.
    st.settled(&s).await;
    st.accepted(SessionCommand::Resume {
        session_id: s.clone(),
    })
    .await;
    assert_eq!(st.state(&s).await, SessionState::Starting);
}

#[tokio::test]
async fn a_session_with_hibernation_off_is_never_stopped() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("turn done", |e| {
        matches!(e, Event::TurnCompleted { .. }).then_some(())
    })
    .await;
    st.det.advance(Duration::from_secs(24 * 60 * 60));
    let store = st.manager.store().clone();
    let policy = st.manager.policy().clone();
    assert_eq!(hibernate::sweep(&store, &policy, 0).await.unwrap(), 0);
    assert_eq!(st.state(&s).await, SessionState::Running);
}
