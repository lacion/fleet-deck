//! The timeline of a wall display (R13 §5D items 7 and 10): a dedicated event
//! type that the deck builds from scratch. It is never an owner event with
//! fields removed. It has no author, input id, option id, provider
//! reference, file list, exit code, checkpoint or `raw_ref`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::{RequestKind, ToolStatus, TurnStatus};
use crate::ids::{NodeId, RequestId, RunId, SessionId, TurnId};
use crate::resources::{CpuLoad, MemoryUse};

/// The longest tool name that a display gets, in characters.
pub const MAX_DISPLAY_TOOL_NAME: usize = 40;
/// The longest first line of a prompt that a display gets, in characters.
pub const MAX_DISPLAY_PROMPT_LINE: usize = 120;
/// The text of a run start whose origin is not an ordinary prompt.
pub const RUN_STARTED_TEXT: &str = "Run started";

/// What happened to a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum DisplayRequestState {
    Opened,
    Resolved,
    Expired,
}

/// One item of a display timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "item", rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum DisplayItem {
    /// The first line of the prompt (at most
    /// [`MAX_DISPLAY_PROMPT_LINE`] characters) for an ordinary origin, else
    /// [`RUN_STARTED_TEXT`].
    RunStarted {
        run_id: RunId,
        summary: String,
    },
    RunEnded {
        run_id: RunId,
        status: TurnStatus,
    },
    TurnStarted {
        run_id: RunId,
        turn_id: TurnId,
    },
    TurnEnded {
        turn_id: TurnId,
        status: TurnStatus,
    },
    /// The assistant message text.
    Message {
        turn_id: TurnId,
        text: String,
    },
    /// A tool: its safe name (at most [`MAX_DISPLAY_TOOL_NAME`]
    /// characters) and its status once it ended.
    Tool {
        node_id: NodeId,
        name: String,
        status: Option<ToolStatus>,
    },
    /// A request: its kind, the correlated safe tool name and its state.
    Request {
        request_id: RequestId,
        kind: RequestKind,
        tool_name: Option<String>,
        state: DisplayRequestState,
    },
}

/// One event of a display timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayEvent {
    pub station_seq: u64,
    pub session_seq: u64,
    /// Unix ms.
    pub at: i64,
    pub item: DisplayItem,
}

/// The reduced resources of a station for a display (R13 §5D item 10): the
/// numeric host metrics and the load of the sessions that the display may
/// see. It has no disk path, no process detail and no other station data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayResources {
    /// When the station measured it, in Unix ms.
    pub at: i64,
    /// Total and free bytes of each disk, without the path or the label.
    pub disks: Vec<DisplayDisk>,
    pub memory: Option<MemoryUse>,
    pub cpu: Option<CpuLoad>,
    pub sessions: Vec<DisplaySessionLoad>,
}

/// The space of one disk, without a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayDisk {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// The CPU and memory of one session that the display may see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplaySessionLoad {
    pub session_id: SessionId,
    pub cpu_pct: Option<u32>,
    pub rss_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_has_a_snake_case_tag() {
        let item = DisplayItem::Tool {
            node_id: NodeId::from("n".to_owned()),
            name: "Shell".to_owned(),
            status: None,
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["item"], "tool");
        assert_eq!(v["name"], "Shell");
        assert!(v.get("input_summary").is_none());
    }
}
