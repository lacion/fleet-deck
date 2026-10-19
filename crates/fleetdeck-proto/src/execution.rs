//! `session.execution_check` (feature `shim.resume.v1`, scope `read`; DL-182,
//! design `docs/v2/design/shim-resume.md` §4.2): what the station sees of
//! the previous execution of a session under a shim, so that the owner can
//! stop what still runs and then mark the run as ended
//! (`session.reconcile_execution`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::execution_stop::ExecutionStopOutcome;
use crate::ids::SessionId;

/// `session.execution_check` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "execution/")]
pub struct ExecutionCheckQuery {
    pub session_id: SessionId,
}

/// One process in an [`ExecutionCheck`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "execution/")]
pub struct ProcessInfo {
    pub pid: u32,
    /// The first 120 characters of the process name (`/proc/<pid>/comm`),
    /// never its arguments; `?` when the name cannot be read.
    pub command: String,
}

/// The `session.execution_check` answer: the newest shim execution of the
/// session whose end is not proven. The check never kills anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "execution/")]
pub struct ExecutionCheck {
    /// The checked shim; `None` when no execution of the session is
    /// unresolved.
    pub shim_id: Option<String>,
    /// Why the execution is unresolved (the row's state and reason), or
    /// `nothing unresolved`.
    pub reason: String,
    /// The current boot id equals the one of the execution. `false` proves
    /// that the execution ended with the old boot.
    pub same_boot: bool,
    /// A live process has the stored identity (pid, start time, boot id).
    pub identity_alive: bool,
    /// Live members of the provider's process group.
    pub group_members: Vec<ProcessInfo>,
    /// Live members of the provider's session.
    pub session_members: Vec<ProcessInfo>,
    /// Live processes outside the group and the session that descend from
    /// the provider or its shim. For information only: they do not block.
    pub escaped: Vec<ProcessInfo>,
    /// The check read every process. `false`: it proves nothing (see
    /// `error`).
    pub complete: bool,
    pub error: Option<String>,
    /// The outcome of the newest stop of this run (`execution_stop.v1`).
    /// The station fills it only for a deck link that negotiated the
    /// feature: an older deck never sees the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_stop: Option<ExecutionStopOutcome>,
}

impl ExecutionCheck {
    /// The answer for a session without an unresolved execution.
    #[must_use]
    pub fn nothing_unresolved() -> Self {
        Self {
            shim_id: None,
            reason: "nothing unresolved".to_owned(),
            same_boot: false,
            identity_alive: false,
            group_members: Vec::new(),
            session_members: Vec::new(),
            escaped: Vec::new(),
            complete: true,
            error: None,
            last_stop: None,
        }
    }

    /// True when a live process of the execution remains: the stored
    /// identity or a member of its group or session.
    #[must_use]
    pub fn still_runs(&self) -> bool {
        self.identity_alive || !self.group_members.is_empty() || !self.session_members.is_empty()
    }
}
