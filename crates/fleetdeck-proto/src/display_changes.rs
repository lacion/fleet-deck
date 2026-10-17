//! Change summaries and patches for a wall display (R13 §5A item 5, §5D
//! items 13 and 14; feature `display_changes.v1`).
//!
//! The station applies the private-path protection and the member deny list
//! before it builds the answer. A denied entry is an anonymous `hidden`
//! marker with no path and no content. A binary file has unknown counts,
//! not zero.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::DiffSummary;
use crate::files::DiffScope;
use crate::ids::SessionId;

/// The largest patch section of one file that the deck sends to a display.
pub const MAX_DISPLAY_PATCH_FILE: u64 = 64 * 1024;
/// The largest serialized answer to a display.
pub const MAX_DISPLAY_CHANGES: u64 = 1024 * 1024;

/// The params of the link method `files.display_changes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct DisplayChangesQuery {
    pub session_id: SessionId,
    /// Also return the patch of each visible file.
    #[serde(default)]
    pub patch: bool,
    /// The station cap of the patch bytes; the station never goes above its
    /// own limit.
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayFileChange {
    /// `None` for a hidden entry.
    pub path: Option<String>,
    /// A protected path: no path and no content is given.
    pub hidden: bool,
    /// `None` for a binary file or a hidden entry.
    pub added: Option<u64>,
    pub removed: Option<u64>,
    pub binary: bool,
    /// The patch section; only with `patch` on and a section within the
    /// limit.
    #[serde(default)]
    pub patch: Option<String>,
    /// The patch section is above the limit and was left out.
    #[serde(default)]
    pub too_large: bool,
}

/// The answer of `files.display_changes` and of
/// `GET /api/displays/self/sessions/{station}/{session}/changes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayChanges {
    pub session_id: SessionId,
    pub scope: DiffScope,
    /// The station cut the answer.
    pub truncated: bool,
    pub summary: DiffSummary,
    pub files: Vec<DisplayFileChange>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_refuses_unknown_fields_and_a_hidden_file_has_no_path() {
        assert!(
            serde_json::from_str::<DisplayChangesQuery>(r#"{"session_id":"s","x":1}"#).is_err()
        );
        let q: DisplayChangesQuery = serde_json::from_str(r#"{"session_id":"s"}"#).unwrap();
        assert!(!q.patch);
        let hidden = DisplayFileChange {
            path: None,
            hidden: true,
            added: None,
            removed: None,
            binary: false,
            patch: None,
            too_large: false,
        };
        let v = serde_json::to_value(&hidden).unwrap();
        assert!(v["path"].is_null());
    }
}
