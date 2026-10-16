//! The background task type, the run origin and the input text bound of the
//! event stream (split from `event.rs` for the module size rule, B6, B10).

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

/// Where a run came from. `local` is the station-local CLI; `provider` is a
/// turn that the provider started without host input (background work, L7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum RunOrigin {
    Board,
    Mail,
    Coordinator,
    Resume,
    Local,
    Provider,
    /// The station's fixed continuation input after a restart (L19,
    /// ADR-007 §2.3). Older peers decode it as unknown.
    Continuation,
}

/// The most bytes of `run.started.input_text`, before the `…` (B10).
const INPUT_TEXT_LIMIT: usize = 32 * 1024;

/// The text of `run.started.input_text` for the text of an input: `None` for
/// an empty text, else the text cut at the largest character boundary of at
/// most 32,768 bytes, with `…` appended when it was cut (B10).
#[must_use]
pub fn input_text_of(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if text.len() <= INPUT_TEXT_LIMIT {
        return Some(text.to_owned());
    }
    let mut end = INPUT_TEXT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = text[..end].to_owned();
    cut.push('…');
    Some(cut)
}
