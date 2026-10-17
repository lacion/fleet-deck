//! `session.start` with a batch reference (R1.4, DL-170): the station records
//! `session.batch` with the name of the session; a bad reference is refused
//! and creates nothing; a start without one has no batch.

use fleetdeck_proto::session::BatchRef;

use super::*;

fn batch(id: &str, index: u32, size: u32) -> BatchRef {
    BatchRef {
        batch_id: id.to_owned(),
        index,
        size,
    }
}

fn start_in(cwd: &str, batch: Option<BatchRef>) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(cwd, "claude"),
        prompt: Some(text("hello")),
        workspace: None,
        setup_cmd: None,
        batch: batch.map(Box::new),
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

async fn batch_of(st: &Station, session: &SessionId) -> Option<BatchRef> {
    st.manager
        .store()
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap()
        .batch
}

#[tokio::test]
async fn a_start_with_a_batch_records_it_in_the_summary() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let d = st
        .accepted(start_in(
            &ws,
            Some(batch("01JABCDEFGHJKMNPQRSTVWXYZ0", 2, 3)),
        ))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    assert_eq!(
        batch_of(&st, &s).await,
        Some(batch("01JABCDEFGHJKMNPQRSTVWXYZ0", 2, 3))
    );
    let events = st.events().await;
    let batches = events
        .iter()
        .filter(|e| matches!(e.event, EventBody::Known(Event::SessionBatch { .. })))
        .count();
    assert_eq!(batches, 1);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_start_without_a_batch_has_none() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    assert_eq!(batch_of(&st, &s).await, None);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_bad_batch_reference_is_refused_and_creates_nothing() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    for bad in [
        batch("", 1, 2),
        batch("has space", 1, 2),
        batch(&"a".repeat(65), 1, 2),
        batch("b1", 0, 2),
        batch("b1", 3, 2),
        batch("b1", 1, 0),
        batch("b1", 1, 101),
    ] {
        let code = st.rejected(start_in(&ws, Some(bad.clone()))).await;
        assert_eq!(code, RejectCode::InvalidCommand, "{bad:?}");
    }
    assert!(st.events().await.is_empty());
    st.manager.shutdown(StopMode::Kill).await;
}
