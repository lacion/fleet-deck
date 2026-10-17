//! The workspace service (PLAN §6.6): Git access, turn checkpoints as hidden
//! refs, diffs between checkpoints, the restore of tracked content, and the
//! consistent checkpoint boundary of a session.

pub mod base;
pub mod boundary;
pub mod branches;
pub mod checkout;
pub mod checkpoint;
pub mod checkpoint_refs;
pub mod clone_reuse;
pub mod coordinator_diff;
pub mod files;
pub mod fleet_git;
pub mod git;
pub mod paste;
pub mod precheck;
pub mod preflight;
pub mod private_paths;
pub mod private_registry;
pub mod provision;
pub mod restore;
pub mod session_diff;
pub mod settle;
pub mod setup;
pub mod setup_cleanup;
mod stage;
pub mod trash;
pub mod worktrees;

pub use checkpoint::{
    CAPTURE_VERIFIED, Captured, Checkpoint, CheckpointError, CheckpointKey, Limits, Phase,
    REF_ROOT, SCOPE_TRACKED, Snapshot, Unavailable, Worktree,
};
pub use git::{Git, GitError, GitOptions};
pub use private_paths::{PrivateEnv, PrivatePaths};
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
#[cfg(test)]
#[path = "worktrees_tests.rs"]
mod worktrees_tests;
