//! The move driver (design §12.3, §12.4A; FG-9, T4.4b). It reads the rows of
//! `session_moves` and sends the next step of each move to the two stations.
//! The two stations never connect to each other: every byte goes through the
//! two links of this deck.
//!
//! - A new row (`holding`) runs the steps 1 to 4 in one task
//!   ([`crate::fleet_git_move_send::forward`]): `MoveOut`, the export, the
//!   import with the stream pipe ([`crate::fleet_git_move_pipe`]), `MoveIn`.
//!   Any failure sets `aborting`.
//! - After `MoveIn` the row is `starting`: the task asks `session.move_status`
//!   every 10 s, and the `session.move_in` events (the read model) move the
//!   row at once.
//! - `committed`, `finishing`, `releasing`, `aborting` and `undoing` are
//!   resent with the backoff of `next_step_at` until acknowledged.
//! - A deck restart aborts `holding`, `exporting` and `importing`
//!   (`move_lost`) and asks the target about `starting` rows first.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::event::{Event, EventBody};
use tokio::sync::{Notify, watch};

use crate::link::DeckLink;
use crate::store::DeckStore;
use crate::store::session_moves::{MoveRow, MoveState};

/// The deadlines of the calls of a move (T4.4b Q8).
#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    pub move_out: Duration,
    pub export: Duration,
    pub prepare: Duration,
    /// The longest time without a data event on the pipe.
    pub pipe_idle: Duration,
    /// The longest time of the whole pipe.
    pub pipe_total: Duration,
    /// How long the deck waits for the upload end after it closed its half.
    pub upload_end: Duration,
    pub move_in: Duration,
    pub status: Duration,
    pub discard: Duration,
    pub abort: Duration,
    /// Finish, release, fail and undo.
    pub step: Duration,
    /// The longest wait of a coordinator move in the authority gate.
    pub gate_wait: Duration,
}

/// The time rules of the driver (a seam for the tests).
#[derive(Debug, Clone, Copy)]
pub struct MoveOptions {
    /// The first wait of the backoff in ms (2, 4, 8 ... 60 units).
    pub retry_unit_ms: i64,
    /// How often a `starting` move is asked (10 s).
    pub poll: Duration,
    /// How many `launching` answers end the wait (15).
    pub polls_max: u64,
    /// The retry of a rejected finish, release, fail or undo (60 s).
    pub slow_retry: Duration,
    /// The longest sleep when nothing is due.
    pub tick: Duration,
    /// How long a `starting` move waits for the target in all (10 min).
    pub starting_limit: Duration,
    pub deadlines: Deadlines,
}

impl MoveOptions {
    /// The rules of the design.
    pub const PRODUCTION: Self = Self {
        retry_unit_ms: 1_000,
        poll: Duration::from_secs(10),
        polls_max: 15,
        slow_retry: Duration::from_secs(60),
        tick: Duration::from_secs(30),
        starting_limit: Duration::from_secs(600),
        deadlines: Deadlines {
            move_out: Duration::from_secs(30),
            export: Duration::from_secs(40),
            prepare: Duration::from_secs(15),
            pipe_idle: Duration::from_secs(60),
            pipe_total: Duration::from_secs(600),
            upload_end: Duration::from_secs(30),
            move_in: Duration::from_secs(30),
            status: Duration::from_secs(10),
            discard: Duration::from_secs(30),
            abort: Duration::from_secs(15),
            step: Duration::from_secs(30),
            gate_wait: Duration::from_secs(300),
        },
    };
}

/// What wakes the move task: a new move, a link start, an event.
#[derive(Debug, Default)]
pub struct MoveKick {
    wake: Notify,
}

impl MoveKick {
    /// A row changed: the task looks at its rows.
    pub fn kick(&self) {
        self.wake.notify_one();
    }

    /// The link of a station started: send again.
    pub fn link_started(&self, _station: &str) {
        self.kick();
    }
}

/// What the tasks of one move share.
pub(crate) struct Ctx {
    pub store: DeckStore,
    pub links: Arc<DeckLink>,
    pub kick: Arc<MoveKick>,
    pub opts: MoveOptions,
    /// The moves that a task drives now.
    active: Mutex<HashSet<String>>,
    /// The `starting` rows of a restart: the first status uses the restart
    /// rule (design §12.3, "Deck restart").
    pub restart: Mutex<HashSet<String>>,
}

impl Ctx {
    fn claim(&self, id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned())
    }

    fn release(&self, id: &str) {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    fn is_active(&self, id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id)
    }
}

/// Runs the move task until `shutdown` is set.
pub async fn run(
    store: DeckStore,
    links: Arc<DeckLink>,
    kick: Arc<MoveKick>,
    opts: MoveOptions,
    mut shutdown: watch::Receiver<bool>,
) {
    let ctx = Arc::new(Ctx {
        store: store.clone(),
        links,
        kick: Arc::clone(&kick),
        opts,
        active: Mutex::default(),
        restart: Mutex::default(),
    });
    let mut live = store.subscribe_live();
    match store.move_recover().await {
        Ok(starting) => {
            ctx.restart
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(starting);
        }
        Err(e) => tracing::warn!(error = %e, "move recovery"),
    }
    loop {
        if *shutdown.borrow() {
            return;
        }
        let wait = dispatch(&ctx).await;
        let sleep = tokio::time::sleep(wait.min(opts.tick));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = kick.wake.notified() => break,
                () = &mut sleep => break,
                batch = live.recv() => match batch {
                    // A batch with a `session.move_in` event can move a row.
                    Ok(b) if !b.events.iter().any(|e| {
                        matches!(&e.event, EventBody::Known(Event::SessionMoveIn(_)))
                    }) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    _ => break,
                },
                changed = shutdown.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
            }
        }
    }
}

/// Starts a task for each row that is due. Returns the time to the next row
/// that becomes due.
async fn dispatch(ctx: &Arc<Ctx>) -> Duration {
    let rows = match ctx.store.move_open().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "move rows");
            return Duration::from_secs(1);
        }
    };
    let now = ctx.store.now_ms();
    let mut next: Option<i64> = None;
    for row in rows {
        match row.state {
            MoveState::ResumeFailed => continue,
            // A row of these states runs in its own task; one that has none
            // lost it (a failed task): it aborts.
            MoveState::Exporting | MoveState::Importing if !ctx.is_active(&row.move_id) => {
                abort_lost(ctx, &row).await;
                continue;
            }
            MoveState::Exporting | MoveState::Importing => continue,
            _ => {}
        }
        let at = row.next_step_at.unwrap_or(0);
        if at > now {
            next = Some(next.map_or(at, |n| n.min(at)));
            continue;
        }
        if !ctx.claim(&row.move_id) {
            continue;
        }
        let task = Arc::clone(ctx);
        tokio::spawn(async move {
            let id = row.move_id.clone();
            crate::fleet_git_move_send::handle(&task, row).await;
            task.release(&id);
            task.kick.kick();
        });
    }
    next.map_or(Duration::from_secs(3600), |at| {
        Duration::from_millis(u64::try_from(at.saturating_sub(now)).unwrap_or(0).max(20))
    })
}

async fn abort_lost(ctx: &Ctx, row: &MoveRow) {
    let name = ctx.store.move_station_name(&row.src_station).await;
    let text = format!("The move stopped when the deck restarted. The session stays on {name}.");
    let note = crate::store::session_moves_steps::MovePatch::note("move_lost", &text);
    if let Err(e) = ctx
        .store
        .move_advance(
            row.move_id.clone(),
            &[MoveState::Exporting, MoveState::Importing],
            MoveState::Aborting,
            note,
        )
        .await
    {
        tracing::warn!(error = %e, "move abort");
    }
}
