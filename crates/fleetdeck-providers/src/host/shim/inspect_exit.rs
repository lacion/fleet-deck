//! The exit census of the shim (DL-431): the census with a pinned leader,
//! and the list of live members that the exit sweep signals. A child module
//! of `inspect.rs`, so that it stays under the module size rule.

use std::path::Path;
use std::time::Duration;

use super::{EXIT_BUDGET, PASS_PAUSE, PROC, ProcStat, belongs, census, live_members};

/// The live members of the provider's process group or session (the leader
/// excluded), read until a pass settles the question. `None` when the
/// census is incomplete or the platform has no reader.
pub(super) fn live_with(
    root: &Path,
    leader: u32,
    pinned: bool,
    pause: Duration,
    between_passes: &mut dyn FnMut(),
    after_listing: &mut dyn FnMut(),
) -> Option<Vec<ProcStat>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let procs = census(
        root,
        (leader, EXIT_BUDGET, pinned),
        pause,
        (between_passes, after_listing),
        &|procs| live_members(procs, leader) > 0,
    )
    .ok()??;
    Some(
        procs
            .into_iter()
            .filter(|p| !p.is_zombie() && belongs(p, leader))
            .collect(),
    )
}

/// The count of live members of the provider's process group or session at
/// a provider exit. `reap` (the shim's own `waitpid` of its ended children)
/// runs between two passes. With `pinned` the leader is a zombie that the
/// caller keeps unreaped (`LeaderWait::Ended`), and its zombie does not make
/// a pass unclean. `None` when the census is incomplete.
#[must_use]
pub fn exit_census(leader: u32, pinned: bool, reap: &mut dyn FnMut()) -> Option<u32> {
    let live = live_with(
        Path::new(PROC),
        leader,
        pinned,
        PASS_PAUSE,
        reap,
        &mut || {},
    )?;
    Some(u32::try_from(live.len()).unwrap_or(u32::MAX))
}

/// The live members themselves, as [`exit_census`] counts them.
#[must_use]
pub fn exit_members(leader: u32, pinned: bool, reap: &mut dyn FnMut()) -> Option<Vec<ProcStat>> {
    live_with(
        Path::new(PROC),
        leader,
        pinned,
        PASS_PAUSE,
        reap,
        &mut || {},
    )
}

#[cfg(test)]
#[path = "inspect_pinned_tests.rs"]
mod tests;
