//! Station Git operations of Fleet Git (design `docs/v2/design/fleet-git.md`,
//! task T1.2: FG-1 to FG-4). This module only declares and re-exports the
//! public items; every helper is private to its file.
//!
//! - `redact`: the redacted error line (§2.5).
//! - `identity`: the home check and the destination checks (§3.8).
//! - `fetch`: the fetch before a start and the start point (§4).
//! - `push`: the refspec, the backoff, the outcome of a push and the one
//!   serialized writer (§5).
//! - `push_outcome`: the classification of a push (§5.5, M2).
//! - `radar`: the predictive conflict radar (§10).
//! - `include`: the copy of ignored files into a new worktree (§7.2).
//! - `home`: `project.ensure_home` and `project.home_status` (§8.4).

mod fetch;
mod home;
mod identity;
mod include;
mod push;
mod push_outcome;
mod radar;
mod redact;

pub use fetch::{BaseStale, StartPoint, fetch_for_start};
pub use home::{ensure_home, git_version, home_status};
pub use identity::{PushDestination, Refusal, VerifiedUrl, check_home, effective_push_url};
pub use include::copy_includes;
pub use push::{
    OptionsProvider, PushFailure, PushJob, PushResult, PushWriter, RefspecError, backoff_delay,
    refspec_for,
};
pub use push_outcome::{PushOutcome, push_outcome};
pub use radar::{RadarEnv, RadarGuard, branch_name_ok, check_branches, run_radar};
pub use redact::redact;
