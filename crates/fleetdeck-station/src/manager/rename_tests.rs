//! `session.rename` (DL-095): the board title of a session is a station
//! event with no effect; it works on an ended session, an unchanged title
//! adds no event, and a refused title changes nothing.

use super::*;

async fn title(st: &Station, session: &SessionId) -> Option<String> {
    st.manager
        .store()
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap()
        .title
}

async fn renames(st: &Station, session: &SessionId) -> Vec<Option<String>> {
    st.events()
        .await
        .into_iter()
        .filter(|e| &e.session_id == session)
        .filter_map(|e| match e.event {
            EventBody::Known(Event::SessionRenamed { title }) => Some(title),
            _ => None,
        })
        .collect()
}

fn rename(session: &SessionId, title: Option<&str>) -> SessionCommand {
    SessionCommand::Rename {
        session_id: session.clone(),
        title: title.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_rename_sets_the_title_once_and_a_clear_removes_it() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    let before = st.effect_states(&s).await;
    st.accepted(rename(&s, Some("  docs pass  "))).await;
    assert_eq!(title(&st, &s).await.as_deref(), Some("docs pass"));
    // The same title again: accepted, no second event.
    st.accepted(rename(&s, Some("docs pass"))).await;
    // An empty title clears it.
    st.accepted(rename(&s, Some("   "))).await;
    assert_eq!(title(&st, &s).await, None);
    assert_eq!(
        renames(&st, &s).await,
        vec![Some("docs pass".to_owned()), None]
    );
    // A rename is no provider operation: no effect was added.
    let after = st.effect_states(&s).await;
    assert_eq!(before.len(), after.len(), "{after:?}");
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_refused_title_changes_nothing_and_a_repeat_gets_the_same_receipt() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.accepted(rename(&s, Some("first"))).await;
    let long = "x".repeat(fleetdeck_proto::command::MAX_TITLE_CHARS + 1);
    for bad in [long.as_str(), "line\nbreak", "tab\there"] {
        assert_eq!(
            st.rejected(rename(&s, Some(bad))).await,
            RejectCode::InvalidCommand,
            "{bad:?}"
        );
    }
    // Exactly 80 characters, with non-ASCII letters, is a title.
    let max = "é".repeat(fleetdeck_proto::command::MAX_TITLE_CHARS);
    st.accepted(rename(&s, Some(&max))).await;
    assert_eq!(title(&st, &s).await.as_deref(), Some(max.as_str()));
    assert_eq!(
        st.rejected(rename(&SessionId::from("nope"), Some("x")))
            .await,
        RejectCode::UnknownSession
    );
    assert_eq!(renames(&st, &s).await.len(), 2);
    // The same command id again returns the stored rejection.
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("rename-bad"),
        target_incarnation: None,
        retry_until: i64::try_from(st.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: rename(&s, Some("bad\u{7}")),
    };
    let first = st
        .manager
        .submit(envelope.clone(), Caller::local())
        .await
        .unwrap();
    let again = st.manager.submit(envelope, Caller::local()).await.unwrap();
    assert_eq!(first.receipt.state, ReceiptState::Rejected);
    assert_eq!(first.receipt, again.receipt);
    assert_eq!(title(&st, &s).await.as_deref(), Some(max.as_str()));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_ended_session_can_be_renamed() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("the first turn", |e| {
        matches!(e, Event::TurnCompleted { .. }).then_some(())
    })
    .await;
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Kill,
    })
    .await;
    st.wait_for("the end", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    st.accepted(rename(&s, Some("done"))).await;
    assert_eq!(title(&st, &s).await.as_deref(), Some("done"));
    assert_eq!(st.state(&s).await, SessionState::Exited);
    st.manager.shutdown(StopMode::Kill).await;
}
