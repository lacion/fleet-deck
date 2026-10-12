//! The workspace service (PLAN §6.6): Git access, turn checkpoints as hidden
//! refs, diffs between checkpoints, and the restore of tracked content.

pub mod checkpoint;
pub mod git;
pub mod restore;

pub use checkpoint::{
    Captured, Checkpoint, CheckpointError, CheckpointKey, Limits, Phase, REF_ROOT, SCOPE_TRACKED,
    Unavailable, Worktree,
};
pub use git::{Git, GitError, GitOptions};
pub use restore::{Diff, Rejected, RestorePlan, RestoreReport};

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod checkpoint_tests;
