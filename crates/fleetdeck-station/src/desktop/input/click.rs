//! The click of R13 P7b §10.2, steps 1 to 10, and the check of the tool and
//! the compositor that runs before the claim (§11.1).
//!
//! 1. the third explicit execute check (movement authorization);
//! 2. the read of the active window, the barrier and the last synchronous
//!    checks, as for a pointer movement;
//! 3. one `hyprctl dispatch movecursor`, no unrelated await before it;
//! 4. the fourth explicit execute check (button authorization);
//! 5. the final bundle, all queries started together and bounded as a whole:
//!    the active window, the cursor position and a snapshot with the layers;
//! 6. the same target identity, the same rectangle and the same global point;
//! 7. the cursor at exactly that point;
//! 8. the synchronous gates and the age limits, with no await;
//! 9. the spawn of the prepared `wlrctl pointer click left`;
//! 10. the supervision, then the final observation. Nothing is compensated,
//!     restored or replayed.

use std::ffi::OsStr;
use std::time::Instant;

use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use serde_json::Value;

use super::exec::{Op, click_spawn_fail};
use super::focus::{Active, read_active};
use super::snapshot::{self, Rect, Snapshot, TakeError};
use super::{AUTHORITY_MAX_AGE, CLICK_FINAL_BUNDLE, CLICK_LIMIT, CLICK_OBSERVATION_MAX_AGE, fail};
use crate::desktop::Fail;
use crate::desktop::run::{CALL_TIMEOUT, InputCommand, Runner};

/// What the final bundle observed, with the monotonic time at which each
/// query was dispatched.
struct Bundle {
    active: Active,
    cursor: (i64, i64),
    snap: Snapshot,
    dispatched: [Instant; 3],
}

/// The answer of `hyprctl -j cursorpos`: `{"x": <int>, "y": <int>}`.
fn parse_cursor(out: &str) -> Option<(i64, i64)> {
    let v: Value = serde_json::from_str(out).ok()?;
    Some((v.get("x")?.as_i64()?, v.get("y")?.as_i64()?))
}

impl Op<'_> {
    /// The tool and compositor check that runs before the claim: `hyprctl
    /// instances -j` must hold exactly one entry for the instance of the
    /// desktop, and its Wayland socket must be the one of the desktop. The
    /// same compositor and seat then receive the movement and the button.
    ///
    /// # Errors
    /// `input_click_tool_unsupported` for no entry, more than one entry or a
    /// socket that differs (verified evidence); `input_failed` when the query
    /// fails, passes its limit or cannot be parsed.
    pub(super) async fn compositor_matches(&self, runner: &Runner<'_>) -> Result<(), Fail> {
        let args = ["instances", "-j"].map(str::to_owned).to_vec();
        let run = tokio::time::timeout(
            super::ACTIVE_LIMIT,
            runner.run_ok("hyprctl", &args, CALL_TIMEOUT),
        )
        .await;
        let out = match run {
            Ok(Ok(out)) => out,
            Ok(Err(e)) if e.code == crate::desktop::code::FORBIDDEN => return Err(e),
            Ok(Err(_)) | Err(_) => return Err(fail(p6_code::INPUT_FAILED)),
        };
        let list: Value =
            serde_json::from_str(&out.stdout).map_err(|_| fail(p6_code::INPUT_FAILED))?;
        let list = list.as_array().ok_or_else(|| fail(p6_code::INPUT_FAILED))?;
        let unsupported = || fail(p6_code::INPUT_CLICK_TOOL_UNSUPPORTED);
        let signature = self
            .ctx
            .env
            .get("HYPRLAND_INSTANCE_SIGNATURE")
            .and_then(OsStr::to_str)
            .ok_or_else(unsupported)?;
        let display = self
            .ctx
            .env
            .get("WAYLAND_DISPLAY")
            .and_then(OsStr::to_str)
            .ok_or_else(unsupported)?;
        let mut same = list
            .iter()
            .filter(|i| i.get("instance").and_then(Value::as_str) == Some(signature));
        match (same.next(), same.next()) {
            (Some(entry), None)
                if entry.get("wl_socket").and_then(Value::as_str) == Some(display) =>
            {
                Ok(())
            }
            _ => Err(unsupported()),
        }
    }

    /// Steps 1 to 10 of §10.2 for a click on the global point `point`; `rect`
    /// is the rectangle of the target at the survey that preceded the
    /// movement, and `covered` the monitors that the survey observed.
    pub(super) async fn click(
        &self,
        runner: &Runner<'_>,
        cmd: InputCommand,
        point: (i64, i64),
        rect: Rect,
        covered: &[String],
    ) -> Result<(), Fail> {
        // Steps 1 and 2: the third check, the active window, the barrier and
        // the last synchronous checks, as for a pointer movement.
        self.before_input(runner, covered).await?;
        // Step 3: one movement.
        self.move_pointer(runner, point.0, point.1).await?;
        // Step 4: the button authorization.
        self.check().await?;
        let authority_at = (self.env.clock)();
        // Step 5: the final bundle.
        let Bundle {
            active,
            cursor,
            snap,
            dispatched,
        } = self.bundle(runner).await?;
        // Step 6: the same identity, rectangle and global point.
        self.judge(&active)?;
        let seen = self.examine(&snap)?;
        if seen.rect != rect || seen.point != Some(point) {
            return Err(fail(p6_code::INPUT_DESTINATION_UNAVAILABLE));
        }
        // Step 7: the cursor is where the movement put it.
        if cursor != point {
            return Err(fail(p6_code::INPUT_DESTINATION_UNAVAILABLE));
        }
        // Step 8, then step 9: no await between them but the spawn itself.
        self.final_checks(&seen.covered, authority_at, &dispatched)?;
        let mut child = runner.spawn_input(cmd, None).map_err(click_spawn_fail)?;
        // Step 10.
        self.supervise(runner, &mut child, &seen.covered, CLICK_LIMIT)
            .await?;
        self.finally(runner).await
    }

    /// Step 5: the active window, the cursor position and a snapshot with the
    /// layers, started together. The whole bundle has [`CLICK_FINAL_BUNDLE`];
    /// there is no retry. The monotonic time of each dispatch is recorded.
    async fn bundle(&self, runner: &Runner<'_>) -> Result<Bundle, Fail> {
        let clock = &self.env.clock;
        let active = async {
            let at = clock();
            (read_active(runner, CLICK_FINAL_BUNDLE).await, at)
        };
        let cursor = async {
            let at = clock();
            (read_cursor(runner).await, at)
        };
        let snap = async {
            let at = clock();
            (snapshot::take(runner, true).await, at)
        };
        let (active, cursor, snap) = tokio::time::timeout(CLICK_FINAL_BUNDLE, async {
            tokio::join!(active, cursor, snap)
        })
        .await
        .map_err(|_| fail(p6_code::INPUT_FOCUS_UNKNOWN))?;
        let unknown = || fail(p6_code::INPUT_FOCUS_UNKNOWN);
        Ok(Bundle {
            active: active.0.map_err(|_| unknown())?,
            cursor: cursor.0.ok_or_else(unknown)?,
            snap: snap.0.map_err(|e: TakeError| self.unreadable(e))?,
            dispatched: [active.1, cursor.1, snap.1],
        })
    }

    /// Step 8: the synchronous gates, the registry and the epoch, the
    /// coverage, the link and the start deadline, then the age limits: the
    /// authority answer at most [`AUTHORITY_MAX_AGE`] old and every
    /// observation of the bundle at most [`CLICK_OBSERVATION_MAX_AGE`] after
    /// its dispatch. No await.
    fn final_checks(
        &self,
        covered: &[String],
        authority_at: Instant,
        dispatched: &[Instant; 3],
    ) -> Result<(), Fail> {
        self.gates_now(covered)?;
        let now = (self.env.clock)();
        if now.saturating_duration_since(authority_at) > AUTHORITY_MAX_AGE {
            return Err(crate::desktop::coordinator::unknown());
        }
        if dispatched
            .iter()
            .any(|at| now.saturating_duration_since(*at) > CLICK_OBSERVATION_MAX_AGE)
        {
            return Err(fail(p6_code::INPUT_FOCUS_UNKNOWN));
        }
        Ok(())
    }
}

/// `hyprctl -j cursorpos`; `None` when the program fails or the answer is not
/// `{"x": <int>, "y": <int>}`.
async fn read_cursor(runner: &Runner<'_>) -> Option<(i64, i64)> {
    let args = ["-j", "cursorpos"].map(str::to_owned).to_vec();
    let out = runner.run_ok("hyprctl", &args, CALL_TIMEOUT).await.ok()?;
    parse_cursor(&out.stdout)
}

#[cfg(test)]
mod tests {
    use super::parse_cursor;

    #[test]
    fn the_cursor_answer_is_two_integers_and_nothing_else() {
        assert_eq!(parse_cursor(r#"{"x": 150, "y": -4}"#), Some((150, -4)));
        for bad in [
            "",
            "garbage",
            "[]",
            r#"{"x": 1}"#,
            r#"{"x": "1", "y": 2}"#,
            r#"{"x": 1.5, "y": 2}"#,
        ] {
            assert_eq!(parse_cursor(bad), None, "{bad}");
        }
    }
}
