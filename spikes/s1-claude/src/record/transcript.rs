//! Reads the CLI's own session transcript after a recording
//! (`~/.claude/projects/<encoded cwd>/<session_id>.jsonl`) and turns the client
//! `uuid`s of the persisted user prompts into a fixture note. This is the
//! evidence for the §6.4 recovery rule: an `outcome_unknown` prompt effect can be
//! reconciled by looking up its `uuid` in the transcript, without a resend.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use spike_canon::fixture::Record;

/// Prefix of the note that [`notes`] writes.
pub const NOTE_PREFIX: &str = "transcript ";

/// One note per session of the recording: the `uuid`s of the user prompts that
/// the CLI persisted (tool results excluded).
#[must_use]
pub fn notes(log: &[Record]) -> Vec<Record> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let projects = home.join(".claude").join("projects");
    session_ids(log)
        .into_iter()
        .map(|sid| {
            let uuids = find(&projects, &sid).map(|p| prompt_uuids(&p));
            let body = match uuids {
                Some(u) => json!({"session_id": sid, "prompt_uuids": u}),
                None => json!({"session_id": sid, "prompt_uuids": null}),
            };
            Record::Note {
                text: format!("{NOTE_PREFIX}{body}"),
            }
        })
        .collect()
}

fn session_ids(log: &[Record]) -> BTreeSet<String> {
    log.iter()
        .filter_map(|r| match r {
            Record::Out { frame } if frame["type"] == "system" && frame["subtype"] == "init" => {
                frame["session_id"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

fn find(projects: &Path, sid: &str) -> Option<PathBuf> {
    std::fs::read_dir(projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|d| d.path().join(format!("{sid}.jsonl")))
        .find(|p| p.is_file())
}

fn prompt_uuids(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "user" && is_prompt(&v["message"]["content"]))
        .filter_map(|v| v["uuid"].as_str().map(str::to_owned))
        .collect()
}

fn is_prompt(content: &Value) -> bool {
    match content {
        Value::String(_) => true,
        Value::Array(blocks) => !blocks.iter().any(|b| b["type"] == "tool_result"),
        _ => false,
    }
}
