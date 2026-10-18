//! The local socket op `wall_directive` (R13 P4 task E, design
//! `omarchy-p4.md` §3.6): the answer for `fleetdeck desktop wall next` and
//! `desktop wall mode next`.
//!
//! The op takes the `display_id` of every wall window that the desktop
//! adapter reports (`DesktopInfo.walls`) and sends one `display.directive`
//! request for each over the current deck link. The station sends an intent,
//! never a body: the deck builds the directive and checks the binding.
//!
//! - No wall window (or no desktop): `results` is empty.
//! - No live deck link that negotiated `desktop_shell.v1`: each result is
//!   `deck_down`.
//! - A request is limited to [`SEND_LIMIT`]; a time-out is `failed`.
//! - A refusal of the deck keeps its `code` and `message`.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::link::Outcome;
use serde::{Deserialize, Serialize};

use crate::desktop;
use crate::link::StationNode;
use crate::local::LocalResponse;
use crate::manager::SessionManager;

/// The time that one `display.directive` request may take.
pub const SEND_LIMIT: Duration = Duration::from_secs(10);

const DECK_DOWN: &str = "The deck is not connected.";
const FAILED: &str = "The wall request failed.";

/// The result for one wall display: `code` and `text` are both `None` when
/// the deck accepted the directive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallResult {
    pub display_id: String,
    pub code: Option<String>,
    pub text: Option<String>,
}

impl WallResult {
    fn accepted(display_id: &str) -> Self {
        Self {
            display_id: display_id.to_owned(),
            code: None,
            text: None,
        }
    }

    fn refused(display_id: &str, code: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            display_id: display_id.to_owned(),
            code: Some(code.into()),
            text: Some(text.into()),
        }
    }
}

/// The path of the deck link: one `display.directive` request.
trait Deck {
    /// The deck's outcome, or `None` when the link is gone.
    async fn send(&self, params: serde_json::Value) -> Option<Outcome>;
}

impl Deck for Control {
    async fn send(&self, params: serde_json::Value) -> Option<Outcome> {
        match self.request("display.directive", None, params).await {
            Ok(outcome) => Some(outcome),
            Err(e) => {
                tracing::debug!(error = %e, "display.directive: the link is gone");
                None
            }
        }
    }
}

/// One request for each wall, in order.
async fn run<D: Deck>(
    walls: &[String],
    deck: Option<&D>,
    intent: &str,
    limit: Duration,
) -> Vec<WallResult> {
    let mut results = Vec::with_capacity(walls.len());
    for id in walls {
        let result = match deck {
            None => WallResult::refused(id, "deck_down", DECK_DOWN),
            Some(deck) => {
                let params = serde_json::json!({ "display_id": id, "intent": intent });
                match tokio::time::timeout(limit, deck.send(params)).await {
                    Ok(Some(Outcome::Ok(_))) => WallResult::accepted(id),
                    Ok(Some(Outcome::Err(e))) => WallResult::refused(id, e.code, e.message),
                    Ok(None) => WallResult::refused(id, "deck_down", DECK_DOWN),
                    Err(_) => WallResult::refused(id, "failed", FAILED),
                }
            }
        };
        results.push(result);
    }
    results
}

/// Sends `intent` for each of `walls` over `deck` (the control of the
/// current link, if any).
pub async fn send_all(walls: &[String], deck: Option<&Control>, intent: &str) -> Vec<WallResult> {
    run(walls, deck, intent, SEND_LIMIT).await
}

/// The op as `local::handle` calls it.
pub(crate) async fn wall_directive(
    manager: &Arc<SessionManager>,
    node: Option<&StationNode>,
    intent: String,
) -> LocalResponse {
    let allowed = {
        let manager = Arc::clone(manager);
        move || manager.policy().allow_desktop()
    };
    // An `info` error (no desktop, no Hyprland, a time-out) is the same as
    // no wall window.
    let walls: Vec<String> = match desktop::service().info(&allowed).await {
        Ok(info) => info.walls.into_iter().map(|w| w.display_id).collect(),
        Err(e) => {
            tracing::debug!(code = e.code, "wall_directive: no desktop information");
            Vec::new()
        }
    };
    if walls.is_empty() {
        return LocalResponse::WallDirective {
            results: Vec::new(),
        };
    }
    let deck = node.and_then(|node| node.link().summary.deck());
    LocalResponse::WallDirective {
        results: send_all(&walls, deck.as_ref(), &intent).await,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use fleetdeck_proto::link::WireError;

    use super::*;
    use crate::local::{LocalRequest, LocalResponse};

    /// Answers the scripted outcomes in order and keeps the params.
    struct Script {
        outcomes: Mutex<Vec<Option<Outcome>>>,
        seen: Mutex<Vec<serde_json::Value>>,
    }

    impl Script {
        fn new(outcomes: Vec<Option<Outcome>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes),
                seen: Mutex::default(),
            }
        }
    }

    impl Deck for Script {
        async fn send(&self, params: serde_json::Value) -> Option<Outcome> {
            self.seen.lock().unwrap().push(params);
            self.outcomes.lock().unwrap().remove(0)
        }
    }

    /// Never answers.
    struct Silent;

    impl Deck for Silent {
        async fn send(&self, _params: serde_json::Value) -> Option<Outcome> {
            std::future::pending().await
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_wall_wire_is_wall_directive_with_an_intent() {
        let request = serde_json::to_string(&LocalRequest::WallDirective {
            intent: "next_mode".to_owned(),
        })
        .unwrap();
        assert_eq!(request, r#"{"op":"wall_directive","intent":"next_mode"}"#);
        let response = LocalResponse::WallDirective {
            results: vec![
                WallResult::accepted("d1"),
                WallResult::refused("d2", "not_bound", "text"),
            ],
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["status"], "wall_directive");
        assert_eq!(value["results"][0]["code"], serde_json::Value::Null);
        assert_eq!(value["results"][1]["code"], "not_bound");
        let back: LocalResponse = serde_json::from_value(value).unwrap();
        assert_eq!(back, response);
    }

    #[tokio::test]
    async fn the_wall_op_sends_one_intent_for_each_display() {
        let deck = Script::new(vec![
            Some(Outcome::Ok(serde_json::json!({}))),
            Some(Outcome::Err(WireError::new("not_bound", "No."))),
        ]);
        let results = run(
            &ids(&["d1", "d2"]),
            Some(&deck),
            "next_needs_you",
            SEND_LIMIT,
        )
        .await;
        assert_eq!(
            results,
            vec![
                WallResult::accepted("d1"),
                WallResult::refused("d2", "not_bound", "No."),
            ]
        );
        let seen = deck.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                serde_json::json!({"display_id": "d1", "intent": "next_needs_you"}),
                serde_json::json!({"display_id": "d2", "intent": "next_needs_you"}),
            ]
        );
    }

    #[tokio::test]
    async fn with_no_wall_the_wall_op_sends_nothing() {
        let deck = Script::new(Vec::new());
        let results = run(&[], Some(&deck), "next_mode", SEND_LIMIT).await;
        assert!(results.is_empty());
        assert!(deck.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn without_a_live_deck_link_each_wall_result_is_deck_down() {
        let none: Option<&Script> = None;
        let results = run(&ids(&["d1", "d2"]), none, "next_mode", SEND_LIMIT).await;
        let down = |id: &str| WallResult::refused(id, "deck_down", "The deck is not connected.");
        assert_eq!(results, vec![down("d1"), down("d2")]);
        // A link that went away during the request is the same.
        let gone = Script::new(vec![None]);
        let results = run(&ids(&["d1"]), Some(&gone), "next_mode", SEND_LIMIT).await;
        assert_eq!(results, vec![down("d1")]);
    }

    #[tokio::test]
    async fn a_wall_request_that_takes_too_long_is_failed() {
        let results = run(
            &ids(&["d1"]),
            Some(&Silent),
            "next_mode",
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(
            results,
            vec![WallResult::refused(
                "d1",
                "failed",
                "The wall request failed."
            )]
        );
    }

    #[tokio::test]
    async fn on_a_station_with_no_wall_the_local_wall_op_answers_no_results() {
        use std::collections::BTreeMap;

        use fleetdeck_core::ChildEnv;
        use fleetdeck_testkit::Det;

        use crate::desktop::Desktop;
        use crate::local::{LocalClient, bind, serve, socket_path};
        use crate::manager::Policy;
        use crate::store::{OfflineApprovalPolicy, Store, StoreConfig};

        // A process that has no desktop: the adapter gets an empty
        // environment, so `info` fails and there is no wall.
        desktop::install(Arc::new(Desktop::with_env(ChildEnv::empty())));
        let det = Det::new();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let policy = Policy::new(
            vec![dir.path().to_owned()],
            2,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        );
        let manager = Arc::new(
            SessionManager::start(
                store,
                policy,
                crate::manager::Adapters::new(),
                det.ids(),
                Arc::new(crate::workspace::PrivatePaths::default()),
            )
            .await
            .unwrap(),
        );
        let path = socket_path(dir.path());
        let listener = bind(&path).await.unwrap();
        let (shutdown, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(serve(listener, Arc::clone(&manager), None, rx));
        let mut client = LocalClient::connect(&path).await.unwrap();
        for intent in ["next_needs_you", "next_mode"] {
            let got = client
                .call(&LocalRequest::WallDirective {
                    intent: intent.to_owned(),
                })
                .await
                .unwrap();
            assert_eq!(
                got,
                LocalResponse::WallDirective {
                    results: Vec::new()
                }
            );
        }
        shutdown.send(true).unwrap();
        task.await.unwrap();
    }
}
