//! The workspace service (PLAN §6.6): Git access, turn checkpoints as hidden
//! refs, diffs between checkpoints, the restore of tracked content, and the
//! consistent checkpoint boundary of a session.

pub mod boundary;
pub mod checkpoint;
pub mod git;
pub mod restore;
pub mod settle;

pub use checkpoint::{
    Captured, Checkpoint, CheckpointError, CheckpointKey, Limits, Phase, REF_ROOT, SCOPE_TRACKED,
    Unavailable, Worktree,
};
pub use git::{Git, GitError, GitOptions};
pub use restore::{Diff, Rejected, RestorePlan, RestoreReport};
pub use settle::{Resolution, SettleReport, resolve, settle_unsettled};

#[cfg(test)]
#[path = "boundary_tests.rs"]
mod boundary_tests;
#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod checkpoint_tests;
#[cfg(test)]
#[path = "settle_tests.rs"]
mod settle_tests;
