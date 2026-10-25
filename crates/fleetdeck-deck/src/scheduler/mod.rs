//! The scheduler of the automations (R5, DL-168).
//!
//! A deck task wakes every [`TICK`] and looks at each enabled schedule. The
//! due times after the cursor (the later of the schedule's `armed_at` and its
//! newest scheduled run) up to now are walked; only the newest one counts,
//! because an older one is overtaken by it:
//!
//! - older than the schedule's grace period: one `missed` run is recorded;
//! - else the run is claimed (one row for each `(schedule, due time)`) and
//!   [`run::Runner`] executes it: the precheck on the station, then a
//!   follow-up to the session of the last run (`reuse_session`) or a new
//!   session in a worktree.
//!
//! A restart of the deck evaluates again from the stored cursor, so a due
//! time that the deck missed while it was off runs when it is inside the
//! grace period. A due time inside the grace period whose station is not
//! linked waits for the next tick; it is `missed` when the grace ends.

pub mod cron;
pub mod run;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::link::DeckLink;
use crate::store::DeckStore;

pub use run::Runner;

/// How often the scheduler looks at the schedules.
pub const TICK: Duration = Duration::from_secs(30);
/// The most due times that one evaluation walks (a deck that was off for a
/// long time with an every-minute schedule).
const MAX_WALK: u32 = 20_000;

/// What the due times of a schedule ask for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// No due time passed since the cursor.
    Nothing,
    /// The newest due time is past the grace period; `count` due times
    /// passed in all.
    Missed { due: i64, count: u32 },
    /// The newest due time is inside the grace period.
    Run { due: i64 },
}

/// The plan for a cron after `cursor`, at `now`, with `grace` in ms.
#[must_use]
pub fn plan(cron: &cron::Cron, offset: i32, cursor: i64, now: i64, grace: i64) -> Plan {
    let mut newest = None;
    let mut count = 0_u32;
    let mut at = cursor;
    while count < MAX_WALK
        && let Some(due) = cron.next_after(at, offset)
        && due <= now
    {
        newest = Some(due);
        count += 1;
        at = due;
    }
    match newest {
        None => Plan::Nothing,
        Some(due) if now - due > grace => Plan::Missed { due, count },
        Some(due) => Plan::Run { due },
    }
}

/// Starts the scheduler task. It stops when `shutdown` turns true.
pub fn spawn(
    store: DeckStore,
    link: Arc<DeckLink>,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let runner = Runner::new(store, link);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    // A run can take minutes (a precheck, a start): the next
                    // tick must not wait for it. The claim of a due time makes
                    // an overlap harmless.
                    let runner = runner.clone();
                    tokio::spawn(async move { runner.tick().await });
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cron::Cron;

    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    /// 2026-10-13 00:00 UTC.
    const D0: i64 = 20_739 * DAY;

    fn daily() -> Cron {
        Cron::parse("0 9 * * *").unwrap()
    }

    #[test]
    fn nothing_is_due_before_the_first_due_time() {
        assert_eq!(plan(&daily(), 0, D0, D0 + 8 * HOUR, HOUR), Plan::Nothing);
    }

    #[test]
    fn a_due_time_inside_the_grace_runs() {
        assert_eq!(
            plan(&daily(), 0, D0, D0 + 9 * HOUR + 40 * MIN, HOUR),
            Plan::Run { due: D0 + 9 * HOUR }
        );
        // The grace edge is inclusive.
        assert_eq!(
            plan(&daily(), 0, D0, D0 + 10 * HOUR, HOUR),
            Plan::Run { due: D0 + 9 * HOUR }
        );
    }

    #[test]
    fn a_due_time_past_the_grace_is_missed() {
        assert_eq!(
            plan(&daily(), 0, D0, D0 + 10 * HOUR + MIN, HOUR),
            Plan::Missed {
                due: D0 + 9 * HOUR,
                count: 1
            }
        );
    }

    #[test]
    fn only_the_newest_due_time_counts() {
        // The deck was off from midnight for 3 days: four 09:00 due times,
        // the newest is 40 min old and runs; the older ones are overtaken.
        let now = D0 + 3 * DAY + 9 * HOUR + 40 * MIN;
        assert_eq!(
            plan(&daily(), 0, D0, now, HOUR),
            Plan::Run {
                due: D0 + 3 * DAY + 9 * HOUR
            }
        );
        // Off for 3 days, back late in the evening: missed, four due times.
        let late = D0 + 3 * DAY + 20 * HOUR;
        assert_eq!(
            plan(&daily(), 0, D0, late, HOUR),
            Plan::Missed {
                due: D0 + 3 * DAY + 9 * HOUR,
                count: 4
            }
        );
    }

    #[test]
    fn the_cursor_makes_a_run_happen_once() {
        let due = D0 + 9 * HOUR;
        let now = due + 5 * MIN;
        assert_eq!(plan(&daily(), 0, D0, now, HOUR), Plan::Run { due });
        // After the run is recorded the cursor is the due time.
        assert_eq!(plan(&daily(), 0, due, now, HOUR), Plan::Nothing);
    }

    #[test]
    fn the_offset_moves_the_due_time() {
        // 09:00 at +02:00 is 07:00 UTC.
        assert_eq!(
            plan(&daily(), 120, D0, D0 + 7 * HOUR + MIN, HOUR),
            Plan::Run { due: D0 + 7 * HOUR }
        );
    }

    #[test]
    fn a_long_walk_is_bounded() {
        let every_minute = Cron::parse("* * * * *").unwrap();
        let now = D0 + 40 * DAY;
        match plan(&every_minute, 0, D0, now, HOUR) {
            Plan::Missed { count, .. } => assert_eq!(count, MAX_WALK),
            other => panic!("{other:?}"),
        }
    }
}
