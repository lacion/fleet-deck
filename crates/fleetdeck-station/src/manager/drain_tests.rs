//! Station drain (M3 7-3, DL-128) against `fake-claude sim`.

use super::*;
use crate::manager::drain;

#[tokio::test]
async fn a_draining_station_refuses_new_work_and_reports_the_busy_sessions() {
    let mut st = Station::new(4).await;
    let cwd = st.workspace();
    let s = st.start(&cwd, Some("hello")).await;
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
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("wait for the host"),
    })
    .await;
    st.wait_for("streaming", |e| {
        matches!(e, Event::MessageDelta { .. }).then_some(())
    })
    .await;

    let report = st.manager.drain(Duration::from_millis(200)).await.unwrap();
    assert!(st.manager.draining());
    assert!(!report.idle, "{report:?}");
    assert_eq!(report.busy, vec![s.to_string()]);

    // New work is refused with a stored rejection; nothing starts.
    assert_eq!(
        st.rejected(SessionCommand::Start {
            spec: spec(&cwd, "claude"),
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        })
        .await,
        RejectCode::InvalidState
    );
    assert_eq!(
        st.rejected(SessionCommand::FollowUp {
            session_id: s.clone(),
            input: text("more"),
        })
        .await,
        RejectCode::InvalidState
    );
    // Commands that settle work are still admitted.
    st.accepted(SessionCommand::Interrupt {
        session_id: s.clone(),
    })
    .await;
    let report = st.manager.drain(WAIT).await.unwrap();
    assert!(report.idle, "{report:?}");
    assert!(report.busy.is_empty());
}

#[test]
fn only_the_commands_that_create_work_are_refused_while_draining() {
    use fleetdeck_proto::command::StopMode;
    let session_id = SessionId::from("s".to_owned());
    let stop = SessionCommand::Stop {
        session_id: session_id.clone(),
        how: StopMode::Graceful,
    };
    let interrupt = SessionCommand::Interrupt {
        session_id: session_id.clone(),
    };
    let resume = SessionCommand::Resume { session_id };
    assert!(drain::guard(true, &stop).is_ok());
    assert!(drain::guard(true, &interrupt).is_ok());
    assert!(drain::guard(true, &resume).is_err());
    assert!(drain::guard(false, &resume).is_ok());
}

/// An import resumes a native session in a new process: it is new work, so a
/// drain refuses it (6-12 merge review r50, DL-130).
#[test]
fn a_draining_station_refuses_an_import() {
    let import = SessionCommand::Import {
        spec: spec("/w", "claude"),
        native_session_id: "cli-session-7".to_owned(),
        prompt: None,
        transcript: None,
    };
    assert!(drain::guard(true, &import).is_err());
    assert!(drain::guard(false, &import).is_ok());
}

/// NV-25: a submit that read the flag before the drain began holds the drain
/// until its decision is done.
#[tokio::test]
async fn a_drain_waits_for_a_submit_that_is_being_decided() {
    let st = Station::new(4).await;
    let deciding = st.manager.enter_drain_submit();
    let report = st.manager.drain(Duration::from_millis(150)).await.unwrap();
    assert!(!report.idle, "{report:?}");
    assert_eq!(
        report.busy,
        vec!["(a command was being decided)".to_owned()]
    );

    drop(deciding);
    let report = st.manager.drain(WAIT).await.unwrap();
    assert!(report.idle, "{report:?}");
    assert!(report.busy.is_empty());
}

#[tokio::test]
async fn a_submit_that_creates_no_work_does_not_hold_the_drain() {
    let mut st = Station::new(4).await;
    let cwd = st.workspace();
    let s = st.start(&cwd, Some("hello")).await;
    st.wait_for("turn done", |e| {
        matches!(e, Event::TurnCompleted { .. }).then_some(())
    })
    .await;
    st.manager.drain(WAIT).await.unwrap();
    // The count of a settling command ends with its submit.
    st.accepted(SessionCommand::Rename {
        session_id: s,
        title: Some("renamed".to_owned()),
    })
    .await;
    let report = st.manager.drain(WAIT).await.unwrap();
    assert!(report.idle, "{report:?}");
}
