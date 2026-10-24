//! B11: the command list of one Claude CLI process (`session.slash_commands`).
//!
//! Three frames carry the list: the `initialize` answer (`commands`, objects),
//! `system/commands_changed` (the same objects) and each root `system/init`
//! (`slash_commands`, `skills`, `terminal_slash_commands`, names). The core
//! emits the event only when the sorted pair changed (DL-410).

use fleetdeck_proto::Event;
use serde_json::{Map, Value};

use super::core::{ClaudeCore, Output};

/// The most names in one list.
pub const MAX_COMMANDS: usize = 500;
/// The longest name, in bytes.
pub const MAX_COMMAND_NAME: usize = 128;

/// What the core knows about the command list of its process.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct CommandState {
    /// `terminal_slash_commands` of the last root `init`, bare names. Empty before it.
    pub terminal: Vec<String>,
    /// `skills` of the last root `init`, normalized. Empty before it.
    pub skills: Vec<String>,
    /// The last emitted `(commands, skills)`; `None` before the first event.
    pub emitted: Option<(Vec<String>, Vec<String>)>,
}

/// Bare, valid, not terminal-only, sorted, unique, at most `MAX_COMMANDS`.
pub(super) fn normalize(
    names: impl IntoIterator<Item = String>,
    terminal: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = names
        .into_iter()
        .map(|n| n.strip_prefix('/').map_or(n.clone(), str::to_owned))
        .filter(|n| {
            !n.is_empty()
                && n.len() <= MAX_COMMAND_NAME
                && !n.chars().any(char::is_whitespace)
                && !terminal.contains(n)
        })
        .collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_COMMANDS);
    out
}

/// The strings of the array `key`; `None` when the key is missing or not an array.
fn strings(body: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    body.get(key).and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

/// `commands` of an `initialize` answer or a `system/commands_changed` frame:
/// the `name` of each object. Not an array: no output.
pub(super) fn from_objects(core: &mut ClaudeCore, commands: Option<&Value>) -> Vec<Output> {
    let Some(list) = commands.and_then(Value::as_array) else {
        return Vec::new();
    };
    let names = list
        .iter()
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .map(str::to_owned);
    let commands = normalize(names, &core.commands.terminal);
    let skills = core.commands.skills.clone();
    emit_if_changed(core, commands, skills)
}

/// A root `init`: stores `terminal_slash_commands` and `skills`, then emits
/// `slash_commands` (minus terminal) with the skills. Neither `slash_commands`
/// nor `skills` is an array: stores what it can, no output.
pub(super) fn from_init(core: &mut ClaudeCore, body: &Map<String, Value>) -> Vec<Output> {
    if let Some(terminal) = strings(body, "terminal_slash_commands") {
        core.commands.terminal = normalize(terminal, &[]);
    }
    let listed = strings(body, "slash_commands");
    let skills = strings(body, "skills");
    if let Some(skills) = &skills {
        core.commands.skills = normalize(skills.iter().cloned(), &[]);
    }
    if listed.is_none() && skills.is_none() {
        return Vec::new();
    }
    // A missing `slash_commands` keeps the last emitted commands.
    let commands = listed.unwrap_or_else(|| {
        core.commands
            .emitted
            .as_ref()
            .map(|(c, _)| c.clone())
            .unwrap_or_default()
    });
    let commands = normalize(commands, &core.commands.terminal);
    let skills = core.commands.skills.clone();
    emit_if_changed(core, commands, skills)
}

/// Emits `Event::SlashCommands` when the pair differs from `state.emitted`.
fn emit_if_changed(
    core: &mut ClaudeCore,
    commands: Vec<String>,
    skills: Vec<String>,
) -> Vec<Output> {
    let pair = (commands, skills);
    if core.commands.emitted.as_ref() == Some(&pair) {
        return Vec::new();
    }
    core.commands.emitted = Some(pair.clone());
    vec![Output::Emit(Event::SlashCommands {
        commands: pair.0,
        skills: pair.1,
    })]
}
