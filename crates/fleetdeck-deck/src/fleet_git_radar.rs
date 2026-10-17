//! The radar task (design §10.2, §10.6; FG-7): for each sync project with at
//! least one live pushed branch, the deck calls `project.radar` on the radar
//! home and keeps the pairs in `radar_pairs`.
//!
//! - A run starts when the project was never run, 5 minutes after its last
//!   run started, 30 s after the first `session.push` with `state = pushed`
//!   since then (one run for a burst), and when the link of the radar home
//!   starts. Any run restarts the 5 minute timer.
//! - The radar home must be a `ready` home on a station linked with
//!   `fleet_git.land.v1`; else nothing is sent and the old rows stay.
//! - A result with a `code` keeps the rows and sets the notice of the
//!   project; a result without one replaces the rows and clears the notice.
//! - A project without a live pushed branch loses its rows and its notice
//!   with no station call. So does a project that is removed or has sync off.
//! - `radar_busy`, any other wire error, a timeout and a lost link change
//!   nothing: the next run asks again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::StationId;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::fleet_git::{PushState, RadarParams, RadarResult, code};
use fleetdeck_proto::link::Outcome;
use tokio::sync::{Notify, broadcast, watch};
use tokio::time::Instant;

use crate::link::DeckLink;
use crate::store::DeckStore;
use crate::store::radar::{RadarInput, RadarNotice};

/// The deadline of one `project.radar` call: the station stops at 120 s
/// after a 30 s fetch (decision Q14).
pub const DEADLINE: Duration = Duration::from_secs(150);
/// How long a project that cannot run waits before the next try.
const SKIP_RETRY: Duration = Duration::from_secs(30);
/// The shortest sleep, so that a store error never spins the loop.
const FLOOR: Duration = Duration::from_millis(50);

/// The time rules of the task (a seam for the tests).
#[derive(Debug, Clone, Copy)]
pub struct RadarOptions {
    /// How long after the start of a run the next one starts.
    pub every: Duration,
    /// How long after the first `session.push` a run starts.
    pub after_push: Duration,
}

impl RadarOptions {
    /// Every 5 minutes, and 30 s after a push.
    pub const PRODUCTION: Self = Self {
        every: Duration::from_secs(300),
        after_push: Duration::from_secs(30),
    };
}

/// The notices of the radar, by project (design §10.6, decision Q15). They
/// live in memory: a restart clears them and the next run sets them again.
#[derive(Debug, Default)]
pub struct RadarState {
    notices: Mutex<BTreeMap<String, RadarNotice>>,
}

impl RadarState {
    fn set(&self, project: &str, code: &str, text: String) {
        self.notices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                project.to_owned(),
                RadarNotice {
                    project_id: project.to_owned(),
                    code: code.to_owned(),
                    text,
                },
            );
    }

    fn clear(&self, project: &str) -> bool {
        self.notices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(project)
            .is_some()
    }

    fn retain(&self, keep: &BTreeSet<String>) {
        self.notices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|project, _| keep.contains(project));
    }

    /// The notices now, by project id.
    #[must_use]
    pub fn notices(&self) -> Vec<RadarNotice> {
        self.notices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
}

/// What wakes the task: a link start. One permit is kept, so a kick before
/// the task waits is not lost.
#[derive(Debug, Default)]
pub struct RadarKick {
    notify: Notify,
    /// The stations whose link started since the last turn.
    started: Mutex<BTreeSet<String>>,
}

impl RadarKick {
    /// The link of `station` started: its projects run once.
    pub fn link_started(&self, station: &str) {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(station.to_owned());
        self.notify.notify_one();
    }

    fn take_started(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.started.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The timers of the projects.
#[derive(Default)]
struct Timers {
    /// When the last run of each project started.
    last: HashMap<String, Instant>,
    /// When the run after a push is due.
    push_due: HashMap<String, Instant>,
}

/// Runs the radar until `shutdown` is set.
pub async fn run(
    store: DeckStore,
    links: Arc<DeckLink>,
    state: Arc<RadarState>,
    kick: Arc<RadarKick>,
    mut shutdown: watch::Receiver<bool>,
    options: RadarOptions,
) {
    let mut timers = Timers::default();
    let mut live = store.subscribe_live();
    loop {
        if *shutdown.borrow() {
            return;
        }
        let mut lagged = false;
        loop {
            match live.try_recv() {
                Ok(batch) => {
                    note_pushes(
                        &store,
                        &batch.events,
                        batch.station_id.clone(),
                        &mut timers,
                        options,
                    )
                    .await
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => lagged = true,
                Err(broadcast::error::TryRecvError::Closed) => return,
            }
        }
        let started = kick.take_started();
        let wait = turn(
            &store,
            &links,
            &state,
            &started,
            lagged,
            &mut timers,
            options,
        )
        .await;
        tokio::select! {
            () = kick.notify.notified() => {}
            () = tokio::time::sleep(wait) => {}
            batch = live.recv() => match batch {
                Ok(b) => {
                    note_pushes(&store, &b.events, b.station_id.clone(), &mut timers, options).await;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // A push may be lost: every project gets its 30 s.
                    let now = Instant::now();
                    for project in timers.last.keys().cloned().collect::<Vec<_>>() {
                        timers.push_due.entry(project).or_insert(now + options.after_push);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            changed = shutdown.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Starts the 30 s of the project of each `session.push` with the state
/// `pushed`; a later push inside the window does not move it.
async fn note_pushes(
    store: &DeckStore,
    events: &[fleetdeck_proto::EventEnvelope],
    station: StationId,
    timers: &mut Timers,
    options: RadarOptions,
) {
    for e in events {
        let EventBody::Known(Event::SessionPush {
            state: PushState::Pushed,
            ..
        }) = &e.event
        else {
            continue;
        };
        match store
            .radar_project_of(&station, e.session_id.as_str())
            .await
        {
            Ok(Some(project)) => {
                timers
                    .push_due
                    .entry(project)
                    .or_insert(Instant::now() + options.after_push);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(error = %err, "radar: project of a push"),
        }
    }
}

/// One pass over the projects. Returns how long to sleep.
async fn turn(
    store: &DeckStore,
    links: &Arc<DeckLink>,
    state: &RadarState,
    started: &BTreeSet<String>,
    lagged: bool,
    timers: &mut Timers,
    options: RadarOptions,
) -> Duration {
    let inputs = match store.radar_inputs().await {
        Ok(inputs) => inputs,
        Err(e) => {
            tracing::warn!(error = %e, "radar inputs");
            return options.every.min(SKIP_RETRY);
        }
    };
    let ids: Vec<String> = inputs.iter().map(|i| i.project_id.clone()).collect();
    if let Err(e) = store.radar_prune(ids.clone()).await {
        tracing::warn!(error = %e, "radar prune");
    }
    let keep: BTreeSet<String> = ids.into_iter().collect();
    state.retain(&keep);
    timers.last.retain(|p, _| keep.contains(p));
    timers.push_due.retain(|p, _| keep.contains(p));
    let mut wait = options.every;
    let mut retry = false;
    for input in &inputs {
        let project = &input.project_id;
        if input.branches.is_empty() {
            if let Err(e) = store.radar_delete(project.clone()).await {
                tracing::warn!(error = %e, project = %project, "radar delete");
            }
            if state.clear(project) {
                store.board_changed();
            }
            timers.last.remove(project);
            timers.push_due.remove(project);
            continue;
        }
        let now = Instant::now();
        if lagged {
            timers
                .push_due
                .entry(project.clone())
                .or_insert(now + options.after_push);
        }
        let linked = input
            .station
            .as_ref()
            .is_some_and(|s| started.contains(s.as_str()));
        let due = linked
            || timers
                .last
                .get(project)
                .is_none_or(|t| now >= *t + options.every)
            || timers.push_due.get(project).is_some_and(|t| now >= *t);
        if due {
            let Some(station) = &input.station else {
                // Nothing is sent; the old rows stay; try again soon.
                retry = true;
                continue;
            };
            timers.last.insert(project.clone(), now);
            timers.push_due.remove(project);
            run_project(store, links, state, input, station).await;
        }
        let next = [
            timers.last.get(project).map(|t| *t + options.every),
            timers.push_due.get(project).copied(),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(at) = next {
            wait = wait.min(at.saturating_duration_since(Instant::now()));
        }
    }
    if retry {
        wait = wait.min(SKIP_RETRY.min(options.every));
    }
    wait.max(FLOOR)
}

/// One `project.radar` call and what its answer does.
async fn run_project(
    store: &DeckStore,
    links: &Arc<DeckLink>,
    state: &RadarState,
    input: &RadarInput,
    station: &StationId,
) {
    let project = &input.project_id;
    let Some(control) = links.control_for(station) else {
        return;
    };
    let params = RadarParams {
        project_id: project.clone(),
        branches: input.branches.iter().map(|(b, _)| b.clone()).collect(),
    };
    let Ok(params) = serde_json::to_value(params) else {
        return;
    };
    let reply =
        tokio::time::timeout(DEADLINE, control.request("project.radar", None, params)).await;
    let result: RadarResult = match reply {
        Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value(v) {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(project = %project, error = %e, "project.radar does not parse");
                return;
            }
        },
        Ok(Ok(Outcome::Err(e))) => {
            // `radar_busy` is not shown: the next run comes.
            tracing::debug!(project = %project, code = %e.code, "project.radar refused");
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(project = %project, error = %e, "project.radar: link ended");
            return;
        }
        Err(_) => {
            tracing::debug!(project = %project, "project.radar timed out");
            return;
        }
    };
    if let Some(c) = &result.code {
        let line = result.text.clone().unwrap_or_default();
        let text = match c.as_str() {
            code::FETCH_FAILED => {
                format!("Radar: the fetch failed on {}: {line}", input.home_name)
            }
            code::GIT_TOO_OLD => format!("Radar: Git on {} is older than 2.45.", input.home_name),
            _ if line.is_empty() => "Radar: The radar run failed.".to_owned(),
            _ => format!("Radar: {line}"),
        };
        state.set(project, c, text);
        store.board_changed();
        return;
    }
    let sides: BTreeMap<String, String> = input
        .branches
        .iter()
        .map(|(b, s)| (b.key.clone(), s.as_str().to_owned()))
        .collect();
    if state.clear(project) {
        store.board_changed();
    }
    if let Err(e) = store.radar_replace(project.clone(), result, sides).await {
        tracing::warn!(project = %project, error = %e, "radar rows");
    }
}
