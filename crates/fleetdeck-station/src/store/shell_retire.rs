//! The retirement of a shell effect settles its row (B8, design
//! `bang-commands.md` §2.0000000.2).
//!
//! Whenever an effect whose payload is a shell command is retired as not
//! dispatched, by any path, the same transaction settles its `shell_runs`
//! row `cancelled` with `advance_pending = 1`. [`super::effects::transition`]
//! calls [`on_retired`], so the hook covers the dispatch refusals, the
//! recovery and every generic retirement.

use rusqlite::Connection;

use super::Result;
use super::effects::{Effect, EffectPayload};
use super::shell_runs::{self, ShellState};

/// Settles the shell row of `effect` `cancelled`, if the effect is a shell
/// command. A row that is terminal already stays as it is.
pub(super) fn on_retired(conn: &Connection, effect: &Effect, now: i64) -> Result<()> {
    if matches!(effect.payload, EffectPayload::ShellCommand { .. }) {
        shell_runs::settle(
            conn,
            effect.effect_id.as_str(),
            ShellState::Cancelled,
            None,
            None,
            None,
            now,
        )?;
    }
    Ok(())
}
