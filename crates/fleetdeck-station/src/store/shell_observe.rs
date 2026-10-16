//! The events of a Codex `!` command settle its row (B8, DL-196).
//!
//! [`observe`] runs in the append transaction of every event, after
//! `effects::observe`. Only the node `shell-<effect id>` of a row whose
//! provider is Codex counts: `tool.started` confirms the effect (the provider
//! took the command), `tool.completed` settles the row by `provider_refs.shell`.
//! Claude rows settle in their own task and are never touched here.

use fleetdeck_proto::event::Event;
use fleetdeck_proto::{EventBody, EventEnvelope};
use rusqlite::{Connection, OptionalExtension, params};

use super::Result;
use super::shell_runs::{self, ShellState};

/// The effect id of a node `shell-<effect id>` whose row is a Codex row.
fn codex_effect(conn: &Connection, node: &str) -> Result<Option<String>> {
    let Some(id) = node.strip_prefix("shell-") else {
        return Ok(None);
    };
    Ok(conn
        .query_row(
            "SELECT effect_id FROM shell_runs WHERE effect_id = ?1 AND provider = 'codex'",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

/// The same confirmation as `effects::observe` gives to a started input.
fn confirm(conn: &Connection, effect_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE effects SET state = 'confirmed', updated_at = ?2
         WHERE effect_id = ?1 AND state IN ('claimed', 'outcome_unknown')",
        params![effect_id, now],
    )?;
    Ok(())
}

/// The row state of the end that `provider_refs.shell` names.
fn end_state(shell: Option<&str>, exit_code: Option<i32>) -> ShellState {
    match shell {
        Some("done") if exit_code == Some(0) => ShellState::Done,
        Some("cancelled") => ShellState::Cancelled,
        Some("unknown") => ShellState::Unknown,
        _ => ShellState::Failed,
    }
}

/// Folds the shell node events of Codex into the row and the effect.
pub(super) fn observe(conn: &Connection, envelope: &EventEnvelope, now: i64) -> Result<()> {
    let EventBody::Known(event) = &envelope.event else {
        return Ok(());
    };
    match event {
        Event::ToolStarted { node_id, .. } => {
            if let Some(id) = codex_effect(conn, node_id.as_str())? {
                confirm(conn, &id, now)?;
            }
        }
        Event::ToolCompleted {
            node_id,
            exit_code,
            provider_refs,
            ..
        } => {
            if let Some(id) = codex_effect(conn, node_id.as_str())? {
                let shell = provider_refs
                    .get("shell")
                    .and_then(serde_json::Value::as_str);
                let state = end_state(shell, *exit_code);
                shell_runs::settle(conn, &id, state, *exit_code, None, None, now)?;
                confirm(conn, &id, now)?;
            }
        }
        _ => {}
    }
    Ok(())
}
