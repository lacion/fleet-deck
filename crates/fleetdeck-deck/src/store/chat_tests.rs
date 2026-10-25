#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::share::{CHAT_KEEP, CHAT_PAGE, MAX_CHAT_CHARS};
use fleetdeck_testkit::Det;

use crate::store::{DeckError, DeckStore};

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

async fn post(st: &DeckStore, session: &str, text: &str) -> Result<u64, DeckError> {
    st.post_chat(
        "st".to_owned(),
        session.to_owned(),
        "member:Ann".to_owned(),
        text,
    )
    .await
    .map(|m| m.seq)
}

#[tokio::test]
async fn messages_get_a_sequence_per_session_and_are_listed_after_a_cursor() {
    let det = Det::new();
    let st = store(&det).await;
    assert_eq!(post(&st, "s1", " one ").await.unwrap(), 1);
    assert_eq!(post(&st, "s1", "two").await.unwrap(), 2);
    assert_eq!(post(&st, "s2", "other").await.unwrap(), 1);
    let all = st.chat("st".to_owned(), "s1".to_owned(), 0).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|m| (m.seq, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(1, "one"), (2, "two")]
    );
    assert_eq!(all[0].author, "member:Ann");
    let after = st.chat("st".to_owned(), "s1".to_owned(), 1).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].seq, 2);
}

#[tokio::test]
async fn a_message_is_checked_and_the_oldest_are_dropped() {
    let det = Det::new();
    let st = store(&det).await;
    assert!(matches!(
        post(&st, "s1", "  ").await,
        Err(DeckError::Invalid(_))
    ));
    let long = "x".repeat(MAX_CHAT_CHARS + 1);
    assert!(matches!(
        post(&st, "s1", &long).await,
        Err(DeckError::Invalid(_))
    ));
    assert!(post(&st, "s1", &"x".repeat(MAX_CHAT_CHARS)).await.is_ok());
    for i in 0..CHAT_KEEP + 5 {
        post(&st, "s1", &format!("m{i}")).await.unwrap();
    }
    let rows = st.chat("st".to_owned(), "s1".to_owned(), 0).await.unwrap();
    // A page holds the newest messages, oldest first.
    assert_eq!(rows.len(), usize::try_from(CHAT_PAGE).unwrap());
    assert_eq!(rows.last().unwrap().text, format!("m{}", CHAT_KEEP + 4));
    let count: i64 = st
        .db()
        .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM session_chat", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(count, i64::from(CHAT_KEEP));
}
