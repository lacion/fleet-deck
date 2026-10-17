//! Rollout evidence: does the Codex thread history keep the host input ids?
//!
//! `thread/start` and `thread/resume` replies carry `thread.path`, the rollout
//! file of the thread. After a scenario the recorder reads each rollout file
//! and notes which `clientUserMessageId` values it contains. ADR-002 uses this
//! to decide how a station reconciles an `outcome_unknown` prompt after a crash
//! (PLAN §6.4).

use std::collections::BTreeMap;

use serde_json::{Value, json};
use spike_canon::fixture::Record;

/// Prefix of the note text.
pub const NOTE_PREFIX: &str = "rollout ";

/// One note per rollout file: the thread id and the found/missing input ids.
#[must_use]
pub fn notes(log: &[Record]) -> Vec<Record> {
    let mut paths: BTreeMap<String, String> = BTreeMap::new();
    let mut inputs: Vec<String> = Vec::new();
    for r in log {
        match r {
            Record::Out { frame } => {
                let thread = frame.pointer("/result/thread");
                if let (Some(id), Some(path)) = (
                    thread.and_then(|t| t.get("id")).and_then(Value::as_str),
                    thread.and_then(|t| t.get("path")).and_then(Value::as_str),
                ) {
                    paths.insert(id.to_owned(), path.to_owned());
                }
            }
            Record::Host { cmd } => {
                if let Some(id) = cmd.get("input_id").and_then(Value::as_str) {
                    inputs.push(id.to_owned());
                }
            }
            _ => {}
        }
    }
    paths
        .into_iter()
        .map(|(thread_id, path)| {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let (found, missing): (Vec<_>, Vec<_>) = inputs
                .iter()
                .cloned()
                .partition(|id| text.contains(&format!("\"{id}\"")));
            Record::Note {
                text: format!(
                    "{NOTE_PREFIX}{}",
                    json!({
                        "thread_id": thread_id,
                        "readable": !text.is_empty(),
                        "client_ids_found": found,
                        "client_ids_missing": missing,
                    })
                ),
            }
        })
        .collect()
}
