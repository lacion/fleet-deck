//! The interruption of an input operation while `wtype` runs (R13 P7 §7.5).
//!
//! The observers: the preview record (a close, an event gap or a new
//! compositor remove or change it), the live local gates, the live monitor
//! coverage of the observed monitors, the link, and the
//! active window (a poll at most once per 100 ms, one at a time). On a
//! detected loss the stdin writer stops, the child group gets `SIGKILL`, the
//! child is reaped and the fixed outcome is returned. The station never sends
//! a compensating key (no Escape, BackSpace or undo), never retries and never
//! restores the focus or the pointer.

use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use tokio::time::Instant;

use super::exec::Op;
use super::focus::read_active;
use super::{ACTIVE_LIMIT, POLL_EVERY, TICK, fail};
use crate::desktop::Fail;
use crate::desktop::run::{InputChild, Runner};

/// How the supervised child ended.
enum Exit {
    Done(std::io::Result<std::process::ExitStatus>),
    Timeout,
    Lost(Fail),
}

impl Op<'_> {
    /// Watches the child until it ends, passes its 15 s limit or a loss is
    /// seen. `Ok` only for a clean exit.
    ///
    /// # Errors
    /// `input_timeout`, `input_failed`, `input_interrupted`, `focus_changed`
    /// or `input_board_forbidden`; every one may follow partial input.
    pub(super) async fn supervise(
        &self,
        runner: &Runner<'_>,
        child: &mut InputChild,
        covered: &[String],
    ) -> Result<(), Fail> {
        let exit = {
            let limit = tokio::time::sleep(self.env.wtype_limit);
            tokio::pin!(limit);
            tokio::select! {
                status = child.wait() => Exit::Done(status),
                () = &mut limit => Exit::Timeout,
                lost = self.watch(runner, covered) => Exit::Lost(lost),
            }
        };
        match exit {
            Exit::Done(Ok(status)) if status.success() => child
                .writer_result()
                .await
                .map_err(|_| fail(p6_code::INPUT_FAILED)),
            Exit::Done(_) => Err(fail(p6_code::INPUT_FAILED)),
            Exit::Timeout => {
                child.kill_and_reap().await;
                Err(fail(p6_code::INPUT_TIMEOUT))
            }
            Exit::Lost(lost) => {
                child.kill_and_reap().await;
                Err(lost)
            }
        }
    }

    /// Returns the failure of the first loss that it sees; it never returns
    /// otherwise.
    async fn watch(&self, runner: &Runner<'_>, covered: &[String]) -> Fail {
        let mut next_poll = Instant::now() + POLL_EVERY;
        loop {
            tokio::time::sleep(TICK).await;
            if self.same_record().is_err()
                || self.admitted.local_gates().is_err()
                || self.covered_now(covered).is_err()
                || !(self.env.link_alive)()
            {
                return fail(p6_code::INPUT_INTERRUPTED);
            }
            if Instant::now() < next_poll {
                continue;
            }
            let seen = read_active(runner, ACTIVE_LIMIT).await;
            next_poll = Instant::now() + POLL_EVERY;
            match seen.map(|active| self.judge(&active)) {
                Ok(Ok(())) => {}
                Ok(Err(lost)) => return lost,
                // A malformed or failed observation stops the input.
                Err(_) => return fail(p6_code::INPUT_INTERRUPTED),
            }
        }
    }
}
