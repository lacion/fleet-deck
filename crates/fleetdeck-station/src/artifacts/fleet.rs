//! The session's own MCP tools (R12, R6, R2; DL-165): `publish_artifact`,
//! `set_focus` and `ask_coordinator`. Every session with a fleet MCP
//! endpoint gets them. The station answers them itself; the caller is the
//! session that the binding names, and no argument can change it.

use async_trait::async_trait;
use fleetdeck_proto::Event;
use fleetdeck_proto::artifacts::{AUTHOR_AGENT, ArtifactPublish, MAX_FOCUS_CHARS};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ArtifactService;
use crate::coordinator::{Caller, FleetTools};
use crate::store::{NewEvent, Store};

/// The longest question that `ask_coordinator` takes, in characters.
pub const MAX_QUESTION_CHARS: usize = 4000;

/// The station side of the session's own tools.
#[derive(Debug, Clone)]
pub struct StationFleet {
    store: Store,
    /// `None`: this station has no artifact directory.
    artifacts: Option<ArtifactService>,
}

impl StationFleet {
    #[must_use]
    pub fn new(store: Store, artifacts: Option<ArtifactService>) -> Self {
        Self { store, artifacts }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishArgs {
    path: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusArgs {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskArgs {
    question: String,
}

fn invalid(e: impl std::fmt::Display) -> String {
    format!("invalid_params: {e}")
}

#[async_trait]
impl FleetTools for StationFleet {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        match tool {
            "publish_artifact" => {
                let args: PublishArgs = serde_json::from_value(args).map_err(invalid)?;
                let service = self.artifacts.as_ref().ok_or_else(|| {
                    "unsupported: this station has no artifact directory".to_owned()
                })?;
                let published = service
                    .publish(ArtifactPublish {
                        session_id: Some(caller.session_id),
                        path: args.path,
                        title: args.title,
                        artifact_id: None,
                        author: Some(AUTHOR_AGENT.to_owned()),
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({
                    "artifact_id": published.artifact.artifact_id,
                    "version": published.version.version,
                    "title": published.artifact.title,
                    "created": published.created,
                }))
            }
            "set_focus" => {
                let args: FocusArgs = serde_json::from_value(args).map_err(invalid)?;
                let text = args.text.trim().to_owned();
                if text.chars().count() > MAX_FOCUS_CHARS || text.chars().any(char::is_control) {
                    return Err(invalid(format!(
                        "`text` has at most {MAX_FOCUS_CHARS} characters and no control character"
                    )));
                }
                self.store
                    .append(
                        caller.session_id,
                        vec![NewEvent::from(Event::SessionFocus { text: text.clone() })],
                    )
                    .await
                    .map_err(|e| format!("internal: {e}"))?;
                Ok(json!({ "focus": text }))
            }
            "ask_coordinator" => {
                let args: AskArgs = serde_json::from_value(args).map_err(invalid)?;
                if args.question.trim().is_empty()
                    || args.question.chars().count() > MAX_QUESTION_CHARS
                {
                    return Err(invalid(format!(
                        "`question` has 1 to {MAX_QUESTION_CHARS} characters"
                    )));
                }
                // The escalation path of R2 is not wired into this build.
                Err("no_coordinator: this session has no coordinator".to_owned())
            }
            other => Err(format!("forbidden: no such tool `{other}`")),
        }
    }
}
