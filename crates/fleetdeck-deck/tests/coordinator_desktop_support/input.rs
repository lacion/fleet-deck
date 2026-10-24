//! The input helpers of the P7 deck tests (R13 P7 task K1): the three input
//! calls, a fake station that answers the input policy purpose, and a world
//! whose grant allows input.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_deck::store::GrantClass;
use fleetdeck_deck::store::coordinator_desktop::{NewOp, OpClass, OpTool, request_key};
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use super::*;

/// The three classes of an input grant.
pub const INPUT3: [GrantClass; 3] = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];

/// The `key_press` call.
pub fn key_call(request_id: &str, chord: &str, station: Option<&str>) -> Value {
    let mut args = json!({"request_id": request_id, "window_id": WINDOW, "chord": chord});
    if let Some(s) = station {
        args["station"] = json!(s);
    }
    json!({"tool": "key_press", "arguments": args})
}

/// The `type_text` call.
pub fn text_call(request_id: &str, text: &str, station: Option<&str>) -> Value {
    let mut args = json!({"request_id": request_id, "window_id": WINDOW, "text": text});
    if let Some(s) = station {
        args["station"] = json!(s);
    }
    json!({"tool": "type_text", "arguments": args})
}

/// The `pointer_move` call.
pub fn move_call(request_id: &str, x: u32, y: u32, station: Option<&str>) -> Value {
    let mut args = json!({"request_id": request_id, "window_id": WINDOW, "x": x, "y": y});
    if let Some(s) = station {
        args["station"] = json!(s);
    }
    json!({"tool": "pointer_move", "arguments": args})
}

/// The answer of `desktop.policy_now` for the input purpose; `off` names the
/// flag that is false.
pub fn input_policy(off: Option<&str>) -> Value {
    let mut p = json!({
        "allow_fleet_control": true, "allow_desktop": true,
        "allow_desktop_capture": true, "allow_desktop_input": true,
        "monitor_allowed": true
    });
    if let Some(flag) = off {
        p[flag] = json!(false);
    }
    p
}

/// The answer of a station that allows everything, input included.
pub fn input_answer(req: &Req) -> Outcome {
    if req.method == "desktop.policy_now" && req.params["purpose"] == "input" {
        return ok(input_policy(None));
    }
    standard_answer(req)
}

/// The station that allows everything, except that `method` is answered by `f`.
pub fn input_except(
    method: &'static str,
    f: impl Fn(&Req) -> Option<Outcome> + Send + Sync + 'static,
) -> Handler {
    handler(move |req| {
        let out = if req.method == method {
            f(&req)
        } else {
            Some(input_answer(&req))
        };
        async move { out }
    })
}

/// The station that allows everything, input included.
pub fn input_station() -> Handler {
    handler(|req| async move { Some(input_answer(&req)) })
}

/// A world whose grant has the three classes, the allowance and the caps.
pub async fn input_world(cap: u32, characters: u32) -> World {
    let w = world_with(&INPUT3, |g| {
        g.allow_input = true;
        g.input_cap = cap;
        g.input_character_cap = characters;
    })
    .await;
    w.fake.set(input_station());
    w
}

/// The grant counters `(desktop, input, input characters, attempts)`.
pub async fn input_counters(w: &World) -> (u32, u32, u32, u32) {
    let g =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    (
        g.desktop_used,
        g.input_used,
        g.input_character_used,
        g.desktop_attempts,
    )
}

/// A new input operation of the grant for the check tests.
pub fn input_new_op(
    w: &World,
    id: &str,
    tool: OpTool,
    hash: &str,
    length: u32,
    target: &StationId,
) -> NewOp {
    NewOp {
        operation_id: id.to_owned(),
        grant_id: w.b.grant_id.clone(),
        request_key: request_key(id),
        payload_hash: format!("payload-{id}"),
        tool,
        class: OpClass::Input,
        ctx: w.context(),
        target_station: target.clone(),
        action_hash: Some(hash.to_owned()),
        resource: json!({"window_id": WINDOW}),
        seconds: None,
        input_length: Some(length),
    }
}

/// The stored operation of a request ID.
pub async fn op_of(
    w: &World,
    request_id: &str,
) -> fleetdeck_deck::store::coordinator_desktop::DesktopOp {
    w.f.store
        .desktop_op_by_key(&w.b.grant_id, &request_key(request_id))
        .await
        .unwrap()
        .expect("an operation row")
}

/// `(attempted_at, finished_at, exposure_addrs)` of an operation.
pub async fn op_times(w: &World, id: &str) -> (Option<i64>, Option<i64>, Option<String>) {
    let id = id.to_owned();
    w.f.store
        .db()
        .call(move |conn| {
            Ok(conn.query_row(
                "SELECT attempted_at, finished_at, exposure_addrs
                 FROM coordinator_desktop_ops WHERE operation_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap()
}

/// The stored texts of the operation and refusal tables that could hold a
/// payload: the tool, the class, the resource, the result, the code, the
/// charges and the snapshot (never an identifier or a hash).
pub async fn stored_text(w: &World) -> String {
    w.f.store
        .db()
        .call(|conn| {
            let mut out = String::new();
            let mut stmt = conn.prepare(
                "SELECT tool || class || resource || COALESCE(result, '')
                     || COALESCE(code, '') || charged || COALESCE(exposure_addrs, '')
                 FROM coordinator_desktop_ops",
            )?;
            for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                out.push_str(&row?);
            }
            let mut stmt = conn.prepare("SELECT code FROM coordinator_desktop_rejects")?;
            for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                out.push_str(&row?);
            }
            Ok(out)
        })
        .await
        .unwrap()
}

/// The audit rows of one action as parsed details, oldest first.
pub async fn audit_details(w: &World, action: &str) -> Vec<Value> {
    w.audit_rows(Some(action))
        .await
        .into_iter()
        .map(|r| serde_json::from_str(&r.2).unwrap())
        .collect()
}
