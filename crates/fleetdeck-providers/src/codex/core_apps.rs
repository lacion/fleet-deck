//! The MCP app requests of the Codex core (B15, design
//! `docs/v2/design/mcp-apps.md` §4.2): the `initialize` extension, the three
//! app requests with their `fd-app-<n>` ids, and their answers.

use std::collections::{HashMap, HashSet};

use fleetdeck_proto::apps::{MCP_APP_EXTENSION, MCP_APP_MIME};
use serde_json::{Value, json};

use super::{CodexCore, Output, rpc};
use crate::adapter::Result;
use crate::codex::map::apps::AppOp;

/// The id prefix of an app request. The number is the shared host id counter,
/// which a checkpoint keeps, so a restored core never reuses an id of a
/// request that a surviving provider process may still answer.
const APP_ID_PREFIX: &str = "fd-app-";

/// The app requests in flight and their answers. Not in the checkpoint.
#[derive(Debug, Default)]
pub(super) struct AppRequests {
    pending: HashSet<String>,
    answers: HashMap<String, std::result::Result<Value, String>>,
}

impl CodexCore {
    /// Offer the MCP Apps extension (`io.modelcontextprotocol/ui`) to the MCP
    /// servers in `initialize`. Set it before `initialize` and only for a
    /// Codex version with a recorded app path.
    pub fn set_offer_mcp_apps(&mut self, on: bool) {
        self.offer_mcp_apps = on;
    }

    /// The `capabilities` of `initialize`.
    pub(super) fn capabilities_json(&self) -> Value {
        let mut caps = json!({"experimentalApi": true});
        if self.offer_mcp_apps {
            caps["extensions"] = json!({MCP_APP_EXTENSION: {"mimeTypes": [MCP_APP_MIME]}});
        }
        caps
    }

    /// Writes one app request and returns its host id. The answer comes with
    /// [`Self::take_app_answer`].
    ///
    /// # Errors
    /// [`crate::adapter::ProviderError::SessionEnded`] after the process ended;
    /// `NotReady` before the thread is open.
    pub fn app_request(&mut self, op: &AppOp) -> Result<(String, Vec<Output>)> {
        let thread = self.thread()?;
        self.next_host_id += 1;
        let id = format!("{APP_ID_PREFIX}{}", self.next_host_id);
        self.app_requests.pending.insert(id.clone());
        let (method, params) = op.wire(&thread);
        let write = Output::Write(rpc::request(&id, method, params));
        Ok((id, vec![write]))
    }

    /// Takes the answer of an app request: the result, or the error message
    /// of the app-server. `None` while it waits.
    pub fn take_app_answer(&mut self, id: &str) -> Option<std::result::Result<Value, String>> {
        self.app_requests.answers.remove(id)
    }

    /// Forgets an app request that nobody waits for any more (a timeout or a
    /// dropped wait). Its late answer is then dropped.
    pub fn forget_app_request(&mut self, id: &str) {
        self.app_requests.pending.remove(id);
        self.app_requests.answers.remove(id);
    }

    /// Takes the answer of an app request. An answer of an id that is not
    /// pending (a late answer after a timeout or a restart) is dropped
    /// without an unknown-frame count. Returns `false` when `id` is not an
    /// app request id.
    pub(super) fn app_answer(
        &mut self,
        id: &Value,
        answer: std::result::Result<&Value, &str>,
    ) -> bool {
        let Some(id) = id.as_str().filter(|i| i.starts_with(APP_ID_PREFIX)) else {
            return false;
        };
        if self.app_requests.pending.remove(id) {
            let answer = answer.cloned().map_err(str::to_owned);
            self.app_requests.answers.insert(id.to_owned(), answer);
        }
        true
    }
}
