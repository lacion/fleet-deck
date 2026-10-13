//! The session's own MCP tools (R12, R6; DL-165): `publish_artifact` and
//! `set_focus`. Every session with a fleet MCP endpoint gets them, and
//! `ask_coordinator`, which the station forwards to the deck (R2.3). The
//! station answers the own tools itself; the caller is the session that
//! the binding names, and no argument can change it.

use async_trait::async_trait;
use fleetdeck_proto::Event;
use fleetdeck_proto::artifacts::{AUTHOR_AGENT, ArtifactPublish, MAX_FOCUS_CHARS};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ArtifactService;
use super::service::ContentPublish;
use crate::coordinator::{Caller, FleetTools};
use crate::store::{NewEvent, Store};

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
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artifact_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusArgs {
    text: String,
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
                let session_id = Some(caller.session_id);
                let author = Some(AUTHOR_AGENT.to_owned());
                let published = match (args.path, args.content) {
                    (Some(path), None) => {
                        if args.name.is_some() {
                            return Err(invalid("`name` goes with `content`, not with `path`"));
                        }
                        service
                            .publish(ArtifactPublish {
                                session_id,
                                path,
                                title: args.title,
                                artifact_id: args.artifact_id,
                                author,
                            })
                            .await
                    }
                    (None, Some(content)) => {
                        let name = args
                            .name
                            .ok_or_else(|| invalid("`content` needs a `name`"))?;
                        service
                            .publish_content(ContentPublish {
                                session_id,
                                name,
                                content,
                                title: args.title,
                                artifact_id: args.artifact_id,
                                author,
                            })
                            .await
                    }
                    _ => return Err(invalid("give exactly one of `path` or `content`")),
                }
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
            other => Err(format!("forbidden: no such tool `{other}`")),
        }
    }
}
