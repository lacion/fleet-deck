//! The background task type of the event stream (split from `event.rs` for
//! the module size rule, B6).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One unit of provider background work (L7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct BackgroundTask {
    pub task_id: String,
    /// The provider's kind name, for example `local_bash` or `local_agent`.
    pub task_kind: String,
}
