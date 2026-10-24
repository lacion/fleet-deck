//! The helpers of the P7b deck tests (R13 P7b task K1): the click call, the
//! `window_list` query, a fake station that answers the three P7b policy
//! purposes and the `window_list` read, and the worlds of both scopes.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::*;

/// An async step of the fake station.
pub type StepFn = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// A hook of an async closure.
pub fn step_fn<F, Fut>(f: F) -> StepFn
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Arc::new(move || Box::pin(f()))
}

/// The `pointer_click` call.
pub fn click_call(request_id: &str, x: u32, y: u32, station: Option<&str>) -> Value {
    let mut args = json!({"request_id": request_id, "window_id": WINDOW, "x": x, "y": y});
    if let Some(s) = station {
        args["station"] = json!(s);
    }
    json!({"tool": "pointer_click", "arguments": args})
}

/// The `window_list` query.
pub fn window_list_query(station: Option<&str>) -> Value {
    match station {
        Some(s) => json!({"tool": "window_list", "arguments": {"station": s}}),
        None => json!({"tool": "window_list", "arguments": {}}),
    }
}

/// The answer of `desktop.policy_now` for a P7b purpose: the five flags of
/// the input answer and the gates that `purpose` needs; `off` names the flag
/// that is false.
pub fn gates_policy(purpose: &str, off: Option<&str>) -> Value {
    let mut p = input_policy(None);
    if purpose == "input_click" || purpose == "input_any_click" {
        p["allow_desktop_click"] = json!(true);
    }
    if purpose == "input_any" || purpose == "input_any_click" {
        p["allow_desktop_input_any"] = json!(true);
    }
    if let Some(flag) = off {
        p[flag] = json!(false);
    }
    p
}

/// A `window_list` answer of `station` with `windows` windows on one monitor.
pub fn window_list_value(station: &str, windows: usize) -> Value {
    let list: Vec<Value> = (0..windows)
        .map(|i| {
            let n = i32::try_from(i).unwrap();
            json!({
                "window_id": format!("{i:032x}"),
                "rect": {"x": 100 + n, "y": 50, "width": 640, "height": 480},
                "monitors": ["DP-1"],
                "focused": i == 0,
                "expires_in_ms": 60_000,
            })
        })
        .collect();
    json!({
        "station": station,
        "monitors": [{"name": "DP-1", "x": 0, "y": 0, "width": 1920, "height": 1080}],
        "windows": list,
    })
}

/// The answer of a station that allows everything, the P7b purposes
/// included. The purpose of `desktop.policy_now` picks the answer.
pub fn p7b_answer(req: &Req) -> Outcome {
    if req.method == "desktop.policy_now" {
        let purpose = req.params["purpose"].as_str().unwrap_or_default();
        if matches!(purpose, "input_click" | "input_any" | "input_any_click") {
            return ok(gates_policy(purpose, None));
        }
    }
    input_answer(req)
}

/// The station that allows everything, except that `method` is answered by `f`.
pub fn p7b_except(
    method: &'static str,
    f: impl Fn(&Req) -> Option<Outcome> + Send + Sync + 'static,
) -> Handler {
    handler(move |req| {
        let out = if req.method == method {
            f(&req)
        } else {
            Some(p7b_answer(&req))
        };
        async move { out }
    })
}

/// The station that allows everything, the P7b purposes included.
pub fn p7b_station() -> Handler {
    handler(|req| async move { Some(p7b_answer(&req)) })
}

/// What the fake station does around its answer to a `window_list` read.
#[derive(Clone, Default)]
pub struct Around {
    /// Runs before the authority check of the read.
    pub before_check: Option<StepFn>,
    /// Runs after the check and before the answer.
    pub after_check: Option<StepFn>,
}

/// A station that answers `desktop.coordinator_read` as the target of a
/// `window_list` read does: it asks the authority check (phase `read`) first,
/// logs the verdict in `verdicts`, and answers `listing` when the check is
/// valid (else the reason as a wire code). Every other request is answered as
/// by [`p7b_station`].
pub fn list_station(listing: Value, verdicts: Arc<Mutex<Vec<Value>>>, around: Around) -> Handler {
    handler(move |req| {
        let (listing, verdicts, around) = (listing.clone(), Arc::clone(&verdicts), around.clone());
        async move {
            if req.method != "desktop.coordinator_read" {
                return Some(p7b_answer(&req));
            }
            if let Some(step) = &around.before_check {
                step().await;
            }
            let check = json!({
                "operation_id": req.params["read_id"],
                "action_hash": req.params["read_hash"],
                "phase": "read",
                "read": req.params["read"],
            });
            let Outcome::Ok(verdict) = req
                .control
                .request("desktop.authority_check", None, check)
                .await
                .unwrap()
            else {
                panic!("a wire error");
            };
            verdicts.lock().unwrap().push(verdict.clone());
            if let Some(step) = &around.after_check {
                step().await;
            }
            if verdict["valid"] == true {
                Some(ok(listing))
            } else {
                Some(fail(verdict["reason"].as_str().unwrap(), "no"))
            }
        }
    })
}

/// A world whose grant has the three classes, the input allowance, `scope`
/// and the click allowance `click`, and whose station answers the P7b
/// purposes.
pub async fn p7b_world(scope: InputScope, click: bool) -> World {
    let w = world_with(&INPUT3, |g| {
        g.allow_input = true;
        g.input_scope = Some(scope);
        g.allow_click = click;
        g.input_cap = 100;
        g.input_character_cap = 1_000;
    })
    .await;
    w.fake.set(p7b_station());
    w
}

/// The features of a full link, without `missing`.
pub fn without_feature(missing: &str) -> Vec<String> {
    feature::ALL
        .iter()
        .filter(|f| **f != missing)
        .map(|f| (*f).to_owned())
        .collect()
}

/// Runs `UPDATE tokens SET <set> WHERE grant_id = ?` for the grant.
pub async fn exec_tokens(store: &fleetdeck_deck::DeckStore, grant_id: &str, set: &'static str) {
    let gid = grant_id.to_owned();
    store
        .db()
        .call(move |conn| {
            conn.execute(
                &format!("UPDATE tokens SET {set} WHERE grant_id = ?1"),
                [gid],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The verdicts of the authority checks that a fake station made.
pub fn new_verdicts() -> Arc<Mutex<Vec<Value>>> {
    Arc::default()
}

/// `(valid, reason)` of each verdict.
pub fn seen_verdicts(v: &Arc<Mutex<Vec<Value>>>) -> Vec<(bool, String)> {
    v.lock()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["valid"].as_bool().unwrap(),
                v["reason"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}
