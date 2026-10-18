//! Step 1 of the deck decision (R13 P6 §6.2, decision O12-9 items 3, 9, 21).

use fleetdeck_proto::coordinator_desktop::{
    CoordUrgency, DesktopCall, DesktopInfoArguments, DesktopQuery, NotifyArguments, StatusArguments,
};
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{CommandId, IncarnationId};
use serde_json::json;

use super::*;

const NOW: i64 = 1_000_000;

fn incarnation() -> IncarnationId {
    IncarnationId::from("deck-db")
}

fn binding() -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b1".into(),
        grant_id: "g1".into(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "n1".into(),
    }
}

fn notify() -> DesktopCall {
    DesktopCall::Notify(NotifyArguments {
        request_id: "r1".into(),
        station: None,
        title: "Hello".into(),
        body: String::new(),
        urgency: CoordUrgency::Normal,
    })
}

fn call(retry_until: i64) -> CoordinatorDesktopCallEnvelope {
    CoordinatorDesktopCallEnvelope {
        command_id: CommandId::from("c1"),
        target_incarnation: incarnation(),
        retry_until,
        caller: "coord-1".into(),
        binding: binding(),
        call: notify(),
    }
}

fn query(deadline: i64, query: DesktopQuery) -> CoordinatorDesktopQueryEnvelope {
    CoordinatorDesktopQueryEnvelope {
        query_id: "q1".into(),
        target_incarnation: incarnation(),
        deadline,
        caller: "coord-1".into(),
        binding: binding(),
        query,
    }
}

fn request(id: Option<&str>, params: serde_json::Value) -> InboundRequest {
    InboundRequest {
        id: 1,
        command_id: id.map(CommandId::from),
        method: "coordinator.desktop_call".into(),
        params,
        spec: fleetdeck_link::methods::method("coordinator.desktop_call").expect("a row"),
    }
}

#[test]
fn a_good_call_envelope_passes() {
    assert_eq!(check_call(&call(NOW + 10_000), NOW, &incarnation()), Ok(()));
}

#[test]
fn the_incarnation_the_deadline_and_the_arguments_are_checked_in_order() {
    let mut env = call(NOW + 10_000);
    env.target_incarnation = IncarnationId::from("another");
    // The incarnation comes first, also with a past deadline.
    env.retry_until = NOW - 1;
    assert_eq!(
        check_call(&env, NOW, &incarnation()),
        Err(Stop::Reply("incarnation_mismatch"))
    );
    // A past `retry_until` is `expired`; `now` itself is still fine.
    let env = call(NOW - 1);
    assert_eq!(
        check_call(&env, NOW, &incarnation()),
        Err(Stop::Reply("expired"))
    );
    assert_eq!(check_call(&call(NOW), NOW, &incarnation()), Ok(()));
    // At most 10 s plus the 5 s clock skew ahead.
    assert_eq!(check_call(&call(NOW + 15_000), NOW, &incarnation()), Ok(()));
    assert_eq!(
        check_call(&call(NOW + 15_001), NOW, &incarnation()),
        Err(Stop::Reply("invalid_params"))
    );
    // The argument rules of the proto crate.
    let mut env = call(NOW + 1_000);
    env.call = DesktopCall::Notify(NotifyArguments {
        request_id: "bad id!".into(),
        station: None,
        title: "t".into(),
        body: String::new(),
        urgency: CoordUrgency::Low,
    });
    assert_eq!(
        check_call(&env, NOW, &incarnation()),
        Err(Stop::Reply("invalid_params"))
    );
}

#[test]
fn the_query_deadline_is_bounded_by_the_deck_deadline_of_the_tool() {
    let info = || DesktopQuery::DesktopInfo(DesktopInfoArguments { station: None });
    let status = || {
        DesktopQuery::DesktopActionStatus(StatusArguments {
            request_id: "r1".into(),
        })
    };
    // `desktop_info`: 40 s plus 5 s; the status tool: 10 s plus 5 s.
    assert_eq!(
        check_query(&query(NOW + 45_000, info()), NOW, &incarnation()),
        Ok(())
    );
    assert_eq!(
        check_query(&query(NOW + 45_001, info()), NOW, &incarnation()),
        Err(Stop::Reply("invalid_params"))
    );
    assert_eq!(
        check_query(&query(NOW + 15_000, status()), NOW, &incarnation()),
        Ok(())
    );
    assert_eq!(
        check_query(&query(NOW + 15_001, status()), NOW, &incarnation()),
        Err(Stop::Reply("invalid_params"))
    );
    // Past is `expired`; `now` is fine.
    assert_eq!(
        check_query(&query(NOW - 1, info()), NOW, &incarnation()),
        Err(Stop::Reply("expired"))
    );
    assert_eq!(
        check_query(&query(NOW, info()), NOW, &incarnation()),
        Ok(())
    );
    let mut env = query(NOW + 1_000, info());
    env.target_incarnation = IncarnationId::from("another");
    assert_eq!(
        check_query(&env, NOW, &incarnation()),
        Err(Stop::Reply("incarnation_mismatch"))
    );
    let bad = DesktopQuery::DesktopInfo(DesktopInfoArguments {
        station: Some("not valid!".into()),
    });
    assert_eq!(
        check_query(&query(NOW + 1_000, bad), NOW, &incarnation()),
        Err(Stop::Reply("invalid_params"))
    );
}

#[test]
fn a_frame_command_id_that_differs_and_a_parse_failure_are_wire_errors() {
    let params = serde_json::to_value(call(NOW)).expect("json");
    assert!(parse_call(&request(Some("c1"), params.clone())).is_ok());
    assert_eq!(
        parse_call(&request(Some("other"), params.clone())).err(),
        Some(Stop::Wire)
    );
    assert_eq!(
        parse_call(&request(None, params.clone())).err(),
        Some(Stop::Wire)
    );
    // An unknown field, and an unknown tool.
    let mut extra = params.clone();
    extra["surprise"] = json!(1);
    assert_eq!(
        parse_call(&request(Some("c1"), extra)).err(),
        Some(Stop::Wire)
    );
    let mut tool = params;
    tool["call"]["tool"] = json!("rm_rf");
    assert_eq!(
        parse_call(&request(Some("c1"), tool)).err(),
        Some(Stop::Wire)
    );
    assert_eq!(
        parse_query(&request(None, json!({}))).err(),
        Some(Stop::Wire)
    );
}
