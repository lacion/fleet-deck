//! The target side of the coordinator input tools `key_press`, `type_text`
//! and `pointer_move` (R13 P7, feature `desktop_input_mcp.v1`;
//! `docs/v2/design/omarchy-p7.md` §5, §7, §8, §10, §11, §13).
//!
//! - [`grammar`]: the validation and the fixed `wtype` argv of a chord.
//! - [`slot`]: the one input slot of the station.
//! - [`barrier`]: the board-window barrier of §7.2.
//! - [`snapshot`] and [`focus`]: the bounded compositor observations.
//! - [`exec`]: the 20 steps of §7.3.
//! - [`watch`]: the interruption of §7.5 while `wtype` runs.
//!
//! The text of `type_text` travels only on a private stdin pipe of `wtype`
//! (argv `-`): never in an argv, an environment variable, a file that Fleet
//! Deck writes, a log or an error. The checks reduce risk; they do not make
//! the focus, the authority and the delivery of the events one atomic step
//! (§7.4), and the agent runs as the owner's OS user.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};

use super::authority::Admitted;
use super::coordinator::{Live, p6_fail};
use super::{Desktop, Fail, Gate};

pub(crate) mod barrier;
pub mod claim;
mod exec;
pub(crate) mod focus;
pub(crate) mod grammar;
pub mod slot;
pub(crate) mod snapshot;
mod watch;

pub use slot::InputSlot;

/// The final active-window query (§10).
pub const ACTIVE_LIMIT: Duration = Duration::from_millis(250);
/// The oldest remote authority answer at injection start (§10).
pub const AUTHORITY_MAX_AGE: Duration = Duration::from_millis(500);
/// The oldest successful focus snapshot at injection start (§10).
pub const FOCUS_MAX_AGE: Duration = Duration::from_millis(100);
/// One `wtype` invocation (§10).
pub const WTYPE_LIMIT: Duration = Duration::from_secs(15);
/// The pointer dispatch (§10).
pub const POINTER_LIMIT: Duration = Duration::from_secs(1);
/// The whole station input handler (§10).
pub const HANDLER_LIMIT: Duration = Duration::from_secs(30);
/// The least time between two polls of the active window while `wtype`
/// runs (§7.5).
pub const POLL_EVERY: Duration = Duration::from_millis(100);
/// How often the synchronous observers (registry, policy, link) look.
pub const TICK: Duration = Duration::from_millis(10);

/// The clock of the age limits. A test replaces it.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// What an input operation needs of its station besides the desktop.
#[derive(Clone)]
pub struct Env {
    /// The live local settings, read at each check.
    pub live: Arc<dyn Fn() -> Live + Send + Sync>,
    /// Whether the link of the action is still current and not ended.
    pub link_alive: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The lower case host names of the known deck origins (§7.2).
    pub deck_hosts: Vec<String>,
    /// The clock of the age limits.
    pub clock: Clock,
    /// The limit of one `wtype` invocation ([`WTYPE_LIMIT`]; a test makes it
    /// short).
    pub wtype_limit: Duration,
    /// The limit of the whole handler ([`HANDLER_LIMIT`]; a test makes it
    /// short).
    pub handler_limit: Duration,
}

impl Env {
    /// An environment with the system clock.
    #[must_use]
    pub fn new(
        live: Arc<dyn Fn() -> Live + Send + Sync>,
        link_alive: Arc<dyn Fn() -> bool + Send + Sync>,
        deck_hosts: Vec<String>,
    ) -> Self {
        Self {
            live,
            link_alive,
            deck_hosts,
            clock: Arc::new(Instant::now),
            wtype_limit: WTYPE_LIMIT,
            handler_limit: HANDLER_LIMIT,
        }
    }
}

/// What the durable claim of an operation decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claimed {
    /// A new claim: the operation runs.
    New,
    /// The same operation finished before: nothing runs again.
    Replay,
}

/// The durable claim of one operation (§6.3, §11), written before the
/// disclosure, the focus and the injection (§7.3 step 9).
#[async_trait]
pub trait Claimer: Send + Sync {
    /// Claims the operation.
    ///
    /// # Errors
    /// `authority_unknown` for an operation that was seen before and did not
    /// finish; the failure of the claim write.
    async fn claim(&self) -> Result<Claimed, Fail>;
}

/// The failure with the fixed P7 code and its exact text.
pub(super) fn fail(code: &'static str) -> Fail {
    p6_fail(code)
}

impl Desktop {
    /// One input action of a coordinator (§7.3). The caller checked the
    /// grammar with [`grammar::check`] and wrote the admission; this runs the
    /// live gates, the slot, the preview resolution, the barrier, the claim,
    /// the disclosure, the focus, the final checks and the injection. The
    /// whole call has the handler limit of 30 s.
    ///
    /// # Errors
    /// The refusals of §13 and the failures of the input tool. A failure
    /// after the injection started can follow partial input.
    pub async fn input(
        &self,
        gate: Gate<'_>,
        action: DesktopAction,
        admitted: Admitted,
        env: &Env,
        claim: &dyn Claimer,
    ) -> Result<(), Fail> {
        let name = super::actions::name(&action);
        let run = exec::run(self, gate, action, &admitted, env, claim);
        let result = match tokio::time::timeout(env.handler_limit, run).await {
            Ok(result) => result,
            Err(_) => Err(fail(p6_code::INPUT_TIMEOUT)),
        };
        match &result {
            Ok(()) => tracing::info!(action = name, result = "ok", "desktop action"),
            Err(e) => tracing::info!(action = name, result = e.code, "desktop action"),
        }
        result
    }

    /// Marks the preview `window_id` as opened on a known deck origin: it
    /// loses its input eligibility for good (R13 P7 §7.2).
    pub fn mark_board_origin(&self, window_id: &str) {
        self.board_marks.mark(window_id);
    }

    /// Whether the preview `window_id` was marked as opened on a deck origin.
    #[must_use]
    pub fn is_board_origin(&self, window_id: &str) -> bool {
        self.board_marks.is_marked(window_id)
    }

    /// The answer of a finished input operation: the cached desktop info
    /// (at most 10 s old) and no handle, no address and no capture.
    ///
    /// # Errors
    /// `unsupported` without a desktop; `failed` when the info cannot be read.
    pub async fn input_result(&self, gate: Gate<'_>) -> Result<DesktopActResult, Fail> {
        let info = self.info(gate).await?;
        Ok(DesktopActResult {
            address: None,
            window_id: None,
            info,
            capture: None,
            info_fresh: None,
        })
    }
}

#[cfg(test)]
mod exec_gate_tests;
#[cfg(test)]
mod exec_pointer_tests;
#[cfg(test)]
mod exec_refusal_tests;
#[cfg(test)]
mod exec_tests;
#[cfg(test)]
mod interrupt_tests;
#[cfg(test)]
mod rig;
