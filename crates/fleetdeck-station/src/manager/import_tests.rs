//! `session.import` (M3 6-12, DL-308) against `fake-claude sim`: a new
//! driven session continues a native CLI session that this station never
//! ran, and every admission rule stores a rejection that spawns nothing.

use super::*;

/// The `session.started` events of `session`: `(resumed, native id)`.
async fn starts(st: &Station, session: &SessionId) -> Vec<(Option<bool>, Option<String>)> {
    st.events()
        .await
        .into_iter()
        .filter(|e| e.session_id == *session)
        .filter_map(|e| match e.event {
            EventBody::Known(Event::SessionStarted {
                resumed,
                provider_refs,
                ..
            }) => Some((
                resumed,
                provider_refs
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            )),
            _ => None,
        })
        .collect()
}

fn import(cwd: &str, instance: &str, native: &str, prompt: Option<&str>) -> SessionCommand {
    SessionCommand::Import {
        spec: spec(cwd, instance),
        native_session_id: native.to_owned(),
        prompt: prompt.map(text),
    }
}

#[tokio::test]
async fn an_imported_claude_session_continues_the_given_native_session() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let d = st
        .accepted(import(&ws, "claude", "cli-session-7", Some("go on")))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    let found = loop {
        let found = starts(&st, &s).await;
        if !found.is_empty() {
            break found;
        }
        assert!(tokio::time::Instant::now() < deadline, "no session.started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        found[0],
        (Some(true), Some("cli-session-7".to_owned())),
        "the board shows a resumed session with the imported native id"
    );
    let effects = st.settled(&s).await;
    assert!(
        effects.contains(&("resume", EffectState::Confirmed)),
        "{effects:?}"
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_import_is_refused_for_a_bad_id_a_foreign_workspace_or_a_native_session_in_use() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let elsewhere = st
        .root
        .path()
        .join("elsewhere")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        st.rejected(import(
            &ws,
            "claude",
            "--dangerously-skip-permissions",
            None
        ))
        .await,
        RejectCode::InvalidCommand,
        "an id that reads as a flag"
    );
    assert_eq!(
        st.rejected(import(&elsewhere, "claude", "cli-1", None))
            .await,
        RejectCode::Forbidden,
        "a workspace outside the allowed roots"
    );
    assert_eq!(
        st.rejected(import(&ws, "nope", "cli-1", None)).await,
        RejectCode::UnknownProvider
    );
    st.accepted(import(&ws, "claude", "cli-1", Some("hi")))
        .await;
    assert_eq!(
        st.rejected(import(&ws, "claude", "cli-1", None)).await,
        RejectCode::InvalidState,
        "one native session is driven by one session at a time"
    );
    st.manager.shutdown(StopMode::Kill).await;
}
