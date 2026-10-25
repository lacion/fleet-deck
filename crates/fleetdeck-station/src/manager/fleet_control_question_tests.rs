//! A coordinator answers a Claude question with its answers, never with an
//! option (ADR-008 §5.3, PRE-PR fix 3): the station validates the answers
//! before the claim and rewrites the decision to a one-time `allow`.

use fleetdeck_proto::event::{ApprovalOption, RequestKind, ResponseChannel};
use serde_json::{Value, json};

use super::rules::row;
use super::*;

fn input() -> Value {
    json!({"questions": [
        {"question": "Which database?", "header": "db", "multiSelect": false,
         "options": [{"label": "sqlite"}, {"label": "postgres"}]},
        {"question": "Why?", "header": "why", "multiSelect": false, "options": []}]})
}

fn option(id: &str) -> ApprovalOption {
    ApprovalOption {
        option_id: id.to_owned(),
        label: id.to_owned(),
        kind: None,
    }
}

/// A Claude session with three open questions that offer `allow_always`
/// (the CLI sent permission suggestions).
async fn claude_questions(st: &Station) -> SessionId {
    let store = st.manager.store().clone();
    let id = "claude-q";
    store
        .create_session(row(
            id,
            ProviderKind::Claude,
            "claude",
            None,
            &st.workspace(),
        ))
        .await
        .unwrap();
    store
        .process_started(id.into(), "proc-claude-q".to_owned().into(), None)
        .await
        .unwrap();
    let ask = |request: &str| Event::ApprovalRequested {
        request_id: request.into(),
        node_id: None,
        request_kind: RequestKind::Question,
        title: "AskUserQuestion".to_owned(),
        details: "two questions".to_owned(),
        input: Some(input()),
        options: vec![option("allow"), option("allow_always"), option("deny")],
        response: ResponseChannel::RpcReply,
        provider_refs: serde_json::Map::new(),
    };
    store
        .append(
            id.into(),
            vec![ask("q-1").into(), ask("q-2").into(), ask("q-3").into()],
        )
        .await
        .unwrap();
    id.into()
}

fn reply(
    session: &SessionId,
    request: &str,
    option_id: &str,
    answers: Option<Value>,
) -> SessionCommand {
    SessionCommand::Answer {
        session_id: session.clone(),
        request_id: request.into(),
        decision: ApprovalDecision {
            option_id: option_id.to_owned(),
            updated_input: answers,
            text: None,
            answered_by: Some("coordinator:coord-1".to_owned()),
        },
    }
}

#[tokio::test]
async fn a_coordinator_answers_a_claude_question_with_its_answers_never_an_option() {
    let mut st = Station::new(4).await;
    st.switch(true);
    let s = claude_questions(&st).await;
    let good = json!({"Which database?": "sqlite", "Why?": "small"});

    // Any option other than `answer` is forbidden, also `allow_always`.
    for option_id in ["allow_always", "allow", "deny", "zz"] {
        let d = st
            .coord(reply(&s, "q-1", option_id, Some(good.clone())))
            .await;
        let (code, message) = st.refused(&d).await;
        assert_eq!(code, RejectCode::Forbidden, "{option_id}");
        assert_eq!(
            message, "a coordinator answers a question with its answers or text, never an option",
            "{option_id}"
        );
    }
    // Text only: a Claude question needs `question_answers`.
    let d = st.coord(reply(&s, "q-1", "answer", None)).await;
    let (code, message) = st.refused(&d).await;
    assert_eq!(code, RejectCode::InvalidCommand);
    assert_eq!(message, "a Claude question needs question_answers");
    // Keys that are not the questions, values that are not short strings.
    let long = "x".repeat(4097);
    for bad in [
        json!({"Which cache?": "redis"}),
        json!({"Which database?": "sqlite", "extra": "x"}),
        json!({"Which database?": 1}),
        json!({"Which database?": ["sqlite"]}),
        json!({"Which database?": long}),
        json!(["sqlite"]),
        json!("sqlite"),
        // Every question needs its answer: an empty or partial map is refused.
        json!({}),
        json!({"Why?": "small"}),
    ] {
        let d = st
            .coord(reply(&s, "q-1", "answer", Some(bad.clone())))
            .await;
        let (code, message) = st.refused(&d).await;
        assert_eq!(code, RejectCode::InvalidCommand, "{bad}");
        assert_eq!(message, "the answers do not match the questions", "{bad}");
    }
    // The refusals did not claim the request: valid answers are accepted,
    // as the one-time `allow` with the answers in the tool input.
    let d = st
        .coord(reply(&s, "q-1", "answer", Some(good.clone())))
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let EffectPayload::Answer { decision, by, .. } = &d.effects[0].payload else {
        panic!("{:?}", d.effects[0].payload)
    };
    assert_eq!(*by, ResolvedBy::Coordinator);
    assert_eq!(decision.option_id, "allow");
    let mut expected = input();
    expected["answers"] = good.clone();
    assert_eq!(decision.updated_input, Some(expected));
    // A string of 4096 characters is the longest answer.
    let edge = json!({"Which database?": "sqlite", "Why?": "y".repeat(4096)});
    let d = st.coord(reply(&s, "q-2", "answer", Some(edge))).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_board_answer_to_a_claude_question_keeps_its_option() {
    let mut st = Station::new(4).await;
    st.switch(true);
    let s = claude_questions(&st).await;
    let d = st
        .send_as(
            CommandOrigin::Board,
            None,
            reply(&s, "q-3", "allow_always", Some(json!({"anything": 1}))),
        )
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let EffectPayload::Answer { decision, by, .. } = &d.effects[0].payload else {
        panic!("{:?}", d.effects[0].payload)
    };
    assert_eq!(*by, ResolvedBy::Board);
    assert_eq!(decision.option_id, "allow_always");
    assert_eq!(decision.updated_input, Some(json!({"anything": 1})));
    st.manager.shutdown(StopMode::Kill).await;
}
