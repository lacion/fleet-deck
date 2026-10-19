//! The station protocol of one input operation (R13 P7 §7.3, steps 1 to 20).
//!
//! 1. validate the authority, the context, the feature and the grammar (the
//!    intake does; [`grammar::plan`] checks the action again);
//! 2. read the live local gates;
//! 3. take the station input slot, or refuse with `busy`;
//! 4. resolve the target: the preview of the caller through the registry; an
//!    `InputAny*` action first resolves a listed handle of the caller's
//!    binding (R13 P7b §7, §10.1; [`super::target`]);
//! 5. to 7. take the bounded snapshot, require an unlocked screen and a
//!    permitted monitor, apply the board barrier and the pointer checks;
//! 8. prepare the fixed command (nothing starts);
//! 9. claim the operation durably;
//! 10. check the authority (`execute`), then send the disclosure;
//! 11. check the authority again, revalidate the registry, then
//! 12. focus exactly the recorded address, once;
//! 13. take the final snapshot (the pointer point included);
//! 14. check the authority for the injection;
//! 15. read the active window (250 ms), the only await before the injection;
//! 16. require the recorded preview and apply the barrier again;
//! 17. read the synchronous gates, the observed monitor coverage against the
//!     live policy and the age limits, with no await;
//! 18. spawn `wtype` or dispatch the pointer movement;
//! 19. observe the end and the interruption, then the final focus;
//! 20. the caller stores the result and never resends the operation.
//!
//! The checks of steps 10, 11 and 14 are `check_phase(Execute)` calls: a
//! passing `Start::begin` is cached and would not check again. A refusal
//! before step 18 does not undo a notification or a focus change.
//!
//! A click (R13 P7b §10.2) shares steps 1 to 13, then continues in
//! [`super::click`]: the movement, a fourth check, the final observation
//! bundle and `wlrctl pointer click left`.

use std::time::Instant;

use fleetdeck_proto::capture::{code as capture_code, text as capture_text};
use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use fleetdeck_proto::coordinator_desktop_wire::AuthorityPhase;
use fleetdeck_proto::desktop::{DesktopAction, Urgency};

use super::barrier::{self, Names};
use super::focus::{self, Active, Verdict, read_active};
use super::grammar::{self, Plan};
use super::snapshot::{self, Client, Rect, Snapshot, TakeError};
use super::target::{self, ListedTarget};
use super::{ACTIVE_LIMIT, AUTHORITY_MAX_AGE, Claimed, Claimer, Env, FOCUS_MAX_AGE, fail};
use crate::desktop::actions::send_notify;
use crate::desktop::authority::{Admitted, Start};
use crate::desktop::capture::{Allowed, capture_gate};
use crate::desktop::coordinator::{preview_owner, refusal, unknown};
use crate::desktop::events::{PreviewOwner, Record};
use crate::desktop::run::{CALL_TIMEOUT, InputCommand, Runner};
use crate::desktop::{Ctx, Desktop, Fail, Gate};

/// The title of the fixed disclosure, as of the P6 screenshot.
const TITLE: &str = "Fleet Deck";

/// What step 8 prepared. Nothing runs yet.
pub(super) enum Prepared {
    Wtype {
        cmd: Box<InputCommand>,
        stdin: Option<Vec<u8>>,
    },
    Pointer,
    /// The fixed `wlrctl pointer click left` of a click; the movement is the
    /// same dispatch as a pointer movement.
    Click(Box<InputCommand>),
}

/// What a survey observed: the global pointer point (a pointer movement or a
/// click only), the names of the monitors that the target overlaps and the
/// rectangle of the target.
pub(super) struct Surveyed {
    pub(super) point: Option<(i64, i64)>,
    pub(super) covered: Vec<String>,
    pub(super) rect: Rect,
}

/// One resolved input operation.
pub(super) struct Op<'a> {
    pub(super) desktop: &'a Desktop,
    pub(super) ctx: Ctx,
    pub(super) gate: Gate<'a>,
    pub(super) start: Start,
    pub(super) admitted: &'a Admitted,
    pub(super) env: &'a Env,
    pub(super) claim: &'a dyn Claimer,
    pub(super) window_id: String,
    pub(super) plan: Plan,
    pub(super) owner: PreviewOwner,
    pub(super) label: String,
    /// The registry record of the preview at resolution (step 4); for a
    /// listed window the record that its handle names (kind `Listed`).
    pub(super) record: Record,
    /// The resolved handle of a listed window (R13 P7b §7); `None` for an
    /// own preview.
    pub(super) listed: Option<ListedTarget>,
    /// The action is one of the `InputAny*` variants.
    pub(super) any: bool,
}

pub(super) fn not_found() -> Fail {
    fail(p6_code::DESKTOP_WINDOW_NOT_FOUND)
}

/// The owner and the caller label of the authenticated coordinator context.
fn identity(start: &Start) -> Result<(PreviewOwner, String), Fail> {
    match (start.coordinator(), start.link_extras()) {
        (Some(ctx), Some(extras)) => Ok((
            preview_owner(&extras.deck_fp, ctx),
            ctx.caller_label.clone(),
        )),
        _ => Err(refusal()),
    }
}

/// Steps 1 to 4, then the rest of the protocol.
pub(super) async fn run(
    desktop: &Desktop,
    gate: Gate<'_>,
    action: DesktopAction,
    admitted: &Admitted,
    env: &Env,
    claim: &dyn Claimer,
) -> Result<(), Fail> {
    let (window_id, plan) = grammar::plan(&action)?;
    let any = grammar::is_any(&action);
    let start = Start::new(admitted.clone(), &action);
    let (owner, label) = identity(&start)?;
    admitted.local_gates()?;
    if !gate() {
        return Err(Fail::forbidden());
    }
    let _slot = desktop.input.try_take()?;
    let ctx = desktop.context().await?;
    let found = target::resolve(desktop, &ctx, &start, &window_id, &owner, any, env)?;
    let op = Op {
        desktop,
        ctx,
        gate,
        start,
        admitted,
        env,
        claim,
        window_id,
        plan,
        owner,
        label,
        record: found.record,
        listed: found.listed,
        any,
    };
    op.go().await
}

impl Op<'_> {
    pub(super) fn runner(&self) -> Runner<'_> {
        Runner {
            env: &self.ctx.env,
            gate: Some(self.gate),
            start: None,
        }
    }

    fn pointer(&self) -> bool {
        matches!(self.plan, Plan::Pointer { .. } | Plan::Click { .. })
    }

    /// An explicit execute check of the authority and the live gates.
    pub(super) async fn check(&self) -> Result<(), Fail> {
        self.start
            .check_phase(AuthorityPhase::Execute, &self.ctx.env)
            .await
    }

    /// The failure of an observation that cannot be read: a program that
    /// failed keeps its failure; an unreadable answer fails closed.
    pub(super) fn unreadable(&self, e: TakeError) -> Fail {
        match e {
            TakeError::Run(fail) => fail,
            TakeError::Malformed if self.pointer() => fail(p6_code::INPUT_DESTINATION_UNAVAILABLE),
            TakeError::Malformed => fail(p6_code::INPUT_FOCUS_UNKNOWN),
        }
    }

    /// Every monitor that the window covers must be permitted by the live
    /// `capture_monitors`; a window on no enabled monitor fails closed.
    /// Returns the names of the covered monitors that the snapshot observed.
    fn coverage(&self, snap: &Snapshot, client: &Client) -> Result<Vec<String>, Fail> {
        let covered: Vec<String> = snap
            .covered(client)
            .into_iter()
            .map(|monitor| monitor.name.clone())
            .collect();
        if covered.is_empty() {
            return Err(self.unreadable(TakeError::Malformed));
        }
        self.covered_now(&covered)?;
        Ok(covered)
    }

    /// Reads the live `capture_monitors` again: every observed monitor of
    /// `covered` must still be permitted (an empty list permits all).
    pub(super) fn covered_now(&self, covered: &[String]) -> Result<(), Fail> {
        let monitors = (self.env.live)().monitors;
        for name in covered {
            capture_gate(
                &Allowed {
                    desktop: true,
                    capture: true,
                    monitors: monitors.clone(),
                },
                Some(name),
            )?;
        }
        Ok(())
    }

    /// Steps 5 to 7 (and 13): the snapshot, the lock, the monitors, the
    /// barrier and, for a pointer movement, the global point. Also returns
    /// the monitors that the snapshot observed the preview on.
    async fn survey(&self, runner: &Runner<'_>) -> Result<Surveyed, Fail> {
        let snap = snapshot::take(runner, self.pointer())
            .await
            .map_err(|e| self.unreadable(e))?;
        self.examine(&snap)
    }

    /// What a snapshot says about the target: the client, the lock, the
    /// monitors, the barrier and, for a pointer movement or a click, the
    /// global point (the checks of steps 5 to 7; a click repeats them on its
    /// final bundle).
    pub(super) fn examine(&self, snap: &Snapshot) -> Result<Surveyed, Fail> {
        let client = snap
            .client(&self.record.address, &self.record.class)
            .ok_or_else(not_found)?;
        if snap.locked != Some(false) {
            return Err(Fail::new(
                capture_code::SCREEN_LOCKED,
                capture_text::SCREEN_LOCKED,
            ));
        }
        let covered = self.coverage(snap, client)?;
        let names = Names {
            class: &client.class,
            initial_class: &client.initial_class,
            title: &client.title,
            initial_title: &client.initial_title,
        };
        if self.marked()
            || barrier::is_board(self.barrier_record().kind, &names, &self.env.deck_hosts)
        {
            return Err(fail(p6_code::INPUT_BOARD_FORBIDDEN));
        }
        let point = match self.plan {
            Plan::Pointer { x, y } | Plan::Click { x, y } => Some(
                snap.destination(client, x, y)
                    .ok_or_else(|| fail(p6_code::INPUT_DESTINATION_UNAVAILABLE))?,
            ),
            _ => None,
        };
        Ok(Surveyed {
            point,
            covered,
            rect: client.rect,
        })
    }

    /// Step 8: the fixed command. A missing `wtype` is `input_tool_missing`;
    /// a click needs `wlrctl` (`input_click_tool_missing`) and a verified
    /// correspondence between the Hyprland instance and the Wayland socket
    /// (`input_click_tool_unsupported`, [`Op::compositor_matches`]).
    async fn prepare(&self, runner: &Runner<'_>) -> Result<Prepared, Fail> {
        let (argv, stdin) = match &self.plan {
            Plan::Key { argv } => (argv.clone(), None),
            Plan::Text { bytes } => (grammar::text_argv(), Some(bytes.clone())),
            Plan::Pointer { .. } => return Ok(Prepared::Pointer),
            Plan::Click { .. } => {
                let argv = ["pointer", "click", "left"].map(str::to_owned).to_vec();
                let cmd = runner
                    .prepare_input("wlrctl", &argv)
                    .map_err(click_spawn_fail)?;
                self.compositor_matches(runner).await?;
                return Ok(Prepared::Click(Box::new(cmd)));
            }
        };
        let cmd = runner.prepare_input("wtype", &argv).map_err(spawn_fail)?;
        Ok(Prepared::Wtype {
            cmd: Box::new(cmd),
            stdin,
        })
    }

    /// Step 10: the fixed disclosure through the notify path. Its failure is
    /// the failure of the operation and nothing is focused or injected.
    async fn disclose(&self) -> Result<(), Fail> {
        let text = self.plan.disclosure(&self.label, self.any);
        send_notify(&self.runner(), TITLE, &text, Urgency::Normal).await
    }

    /// Step 12: focus exactly the recorded address, once.
    async fn focus(&self, runner: &Runner<'_>) -> Result<(), Fail> {
        let target = format!("address:{}", self.record.address);
        let args = ["dispatch", "focuswindow", target.as_str()]
            .map(str::to_owned)
            .to_vec();
        match runner.run_ok("hyprctl", &args, CALL_TIMEOUT).await {
            Ok(_) => Ok(()),
            // The focus could not be set: the input is not started.
            Err(e) if e.code == crate::desktop::code::FORBIDDEN => Err(e),
            Err(_) => Err(fail(p6_code::INPUT_FOCUS_UNKNOWN)),
        }
    }

    /// Step 16 and step 19: what an observation of the active window means.
    pub(super) fn judge(&self, active: &Active) -> Result<(), Fail> {
        match focus::verdict(active, &self.barrier_record(), &self.env.deck_hosts) {
            Verdict::Same if self.marked() => Err(fail(p6_code::INPUT_BOARD_FORBIDDEN)),
            Verdict::Same => Ok(()),
            Verdict::Foreign => Err(fail(p6_code::FOCUS_CHANGED)),
            Verdict::Board => Err(fail(p6_code::INPUT_BOARD_FORBIDDEN)),
        }
    }

    /// The synchronous gates of step 17, with no await: the local gates, the
    /// observed monitor coverage against the live policy, the start deadlines
    /// and the connection, the link and the registry.
    pub(super) fn gates_now(&self, covered: &[String]) -> Result<(), Fail> {
        self.admitted.local_gates()?;
        self.covered_now(covered)?;
        self.start.still_valid()?;
        if !(self.env.link_alive)() {
            return Err(unknown());
        }
        self.same_record()
    }

    /// Step 17: the synchronous gates, the observed monitor coverage against
    /// the live policy, the link, the start deadlines, the registry and the
    /// age limits. No await.
    pub(super) fn last_checks(
        &self,
        covered: &[String],
        authority_at: Instant,
        focus_at: Instant,
    ) -> Result<(), Fail> {
        self.gates_now(covered)?;
        let now = (self.env.clock)();
        if now.saturating_duration_since(authority_at) > AUTHORITY_MAX_AGE {
            return Err(unknown());
        }
        if now.saturating_duration_since(focus_at) > FOCUS_MAX_AGE {
            return Err(fail(p6_code::INPUT_FOCUS_UNKNOWN));
        }
        Ok(())
    }

    /// Steps 14 to 17: the authority check for the injection, the read of the
    /// active window and the last synchronous checks. No await follows the
    /// last checks.
    pub(super) async fn before_input(
        &self,
        runner: &Runner<'_>,
        covered: &[String],
    ) -> Result<(), Fail> {
        self.check().await?;
        let authority_at = (self.env.clock)();
        let active = read_active(runner, ACTIVE_LIMIT)
            .await
            .map_err(|_| fail(p6_code::INPUT_FOCUS_UNKNOWN))?;
        let focus_at = (self.env.clock)();
        self.judge(&active)?;
        // Step 17 and step 18: no await between them but the spawn itself.
        self.last_checks(covered, authority_at, focus_at)
    }

    /// Steps 5 to 20 of the protocol.
    async fn go(&self) -> Result<(), Fail> {
        let runner = self.runner();
        self.survey(&runner).await?;
        let prepared = self.prepare(&runner).await?;
        match self.claim.claim().await? {
            Claimed::New => {}
            Claimed::Replay => return Ok(()),
        }
        self.check().await?;
        self.disclose().await?;
        self.check().await?;
        self.same_record()?;
        self.focus(&runner).await?;
        let Surveyed {
            point,
            covered,
            rect,
        } = self.survey(&runner).await?;
        self.same_record()?;
        match prepared {
            Prepared::Wtype { cmd, stdin } => {
                self.before_input(&runner, &covered).await?;
                let mut child = runner.spawn_input(*cmd, stdin).map_err(spawn_fail)?;
                self.supervise(&runner, &mut child, &covered, self.env.wtype_limit)
                    .await?;
            }
            Prepared::Pointer => {
                self.before_input(&runner, &covered).await?;
                let (x, y) = point.ok_or_else(|| fail(p6_code::INPUT_DESTINATION_UNAVAILABLE))?;
                self.move_pointer(&runner, x, y).await?;
            }
            Prepared::Click(cmd) => {
                let point = point.ok_or_else(|| fail(p6_code::INPUT_DESTINATION_UNAVAILABLE))?;
                return self.click(&runner, *cmd, point, rect, &covered).await;
            }
        }
        self.finally(&runner).await
    }

    /// Step 19, the pointer: one `movecursor` dispatch within 1 s.
    pub(super) async fn move_pointer(
        &self,
        runner: &Runner<'_>,
        x: i64,
        y: i64,
    ) -> Result<(), Fail> {
        let args = ["dispatch", "movecursor"]
            .map(str::to_owned)
            .into_iter()
            .chain([x.to_string(), y.to_string()])
            .collect::<Vec<_>>();
        match tokio::time::timeout(
            super::POINTER_LIMIT,
            runner.run_ok("hyprctl", &args, CALL_TIMEOUT),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err(fail(p6_code::INPUT_FAILED)),
            Err(_) => Err(fail(p6_code::INPUT_TIMEOUT)),
        }
    }

    /// Step 19, after a clean end: the final focus and identity observation.
    /// An observation that cannot be made leaves the result open
    /// (`input_interrupted`: some input may have been sent).
    pub(super) async fn finally(&self, runner: &Runner<'_>) -> Result<(), Fail> {
        match read_active(runner, ACTIVE_LIMIT).await {
            Ok(active) => self.judge(&active),
            Err(_) => Err(fail(p6_code::INPUT_INTERRUPTED)),
        }
    }
}

/// The failure of `wlrctl` that cannot start: not installed is
/// `input_click_tool_missing` (R13 P7b §13).
pub(super) fn click_spawn_fail(e: Fail) -> Fail {
    match e.code {
        crate::desktop::code::UNSUPPORTED => fail(p6_code::INPUT_CLICK_TOOL_MISSING),
        crate::desktop::code::FORBIDDEN => e,
        _ => fail(p6_code::INPUT_FAILED),
    }
}

/// The failure of a program that cannot start.
fn spawn_fail(e: Fail) -> Fail {
    match e.code {
        crate::desktop::code::UNSUPPORTED => fail(p6_code::INPUT_TOOL_MISSING),
        crate::desktop::code::FORBIDDEN => e,
        _ => fail(p6_code::INPUT_FAILED),
    }
}
