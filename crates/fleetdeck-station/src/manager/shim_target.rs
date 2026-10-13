//! Sessions under shims in the session manager's effect target (ADR-005;
//! DL-113).
//!
//! With [`ShimSettings`] the target launches a session whose adapter has a
//! durable driver through a shim ([`super::super::shims::launch_shim`]) and
//! drives it with the durable driver: the driver commits the provider's
//! events itself (with the adapter state and the cursor), so an observer
//! task feeds the committed events of the session to its turn checkpoints
//! ([`SessionTurns`]) and ends the process when the session ends. Inputs
//! are written under the same workspace fence as with the direct host, so
//! the start checkpoint is published before the input (PLAN §6.6).
//!
//! A session under a shim survives a station stop: [`SessionTarget::stop_all`]
//! does not stop it, and the next station start restores it
//! ([`super::super::shim_station::recover_station`], then
//! [`SessionTarget::adopt`]).
//!
//! Revert (DL-116): a Codex session under a shim runs the same revert
//! executor as with the direct host ([`super::super::revert_run::run`]),
//! with the durable driver's `thread/rollback` as its conversation
//! rollback. A provider without a durable rollback (Claude has no
//! conversation rollback, so its revert is refused at admission) fails
//! the revert with [`REASON_REVERT_UNSUPPORTED`] before any I/O.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::new_id;
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_providers::ProviderAdapter;
use fleetdeck_providers::durable::{DurableConfig, DurableLaunch};
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimProcess};
use fleetdeck_providers::host::{DEFAULT_IDLE_GRACE, ProcessControl as _, StopPolicy};
use tokio::sync::Mutex;

use super::super::shim_claude::DurableClaude;
use super::super::shim_codex::DurableCodex;
use super::super::shim_station::{Durable, Restored};
use super::super::shims::{self, LaunchError, launch_shim, reason};
use super::super::turns::SessionTurns;
use super::{Live, REASON_PROCESS_EXIT, SessionTarget, Sessions};
use crate::store::{
    Dispatch, Effect, EffectPayload, InputRecord, ProcessIncarnation, ReadResult, RevertStep,
    ShimState, Store,
};

/// The reason of a revert effect on a session under a shim.
pub const REASON_REVERT_UNSUPPORTED: &str = "revert_not_supported_on_the_shim_host";
/// Events that the observer reads in one call.
const OBSERVE_BATCH: usize = 256;

/// The shim host of a station and its recovery wait.
#[derive(Debug, Clone)]
pub struct ShimSettings {
    pub host: Arc<ShimHost>,
    /// How long recovery asks a shim about an in-flight stdin write.
    pub in_flight_wait: Duration,
    /// How long a revert waits for the provider's rollback answer.
    pub rollback_timeout: Duration,
}

impl ShimSettings {
    /// The settings with the default waits.
    #[must_use]
    pub fn new(host: Arc<ShimHost>) -> Self {
        Self {
            host,
            in_flight_wait: super::super::shims::IN_FLIGHT_WAIT,
            rollback_timeout: fleetdeck_providers::codex::ROLLBACK_TIMEOUT,
        }
    }
}

/// The durable Codex driver's `thread/rollback` as a revert's
/// conversation rollback.
struct CodexRollback<'a> {
    driver: &'a DurableCodex,
    timeout: Duration,
}

#[async_trait::async_trait]
impl super::super::revert_run::Rollback for CodexRollback<'_> {
    async fn roll_back(&mut self, turns: u32) -> Result<(), fleetdeck_providers::ProviderError> {
        self.driver.rollback(turns, self.timeout).await
    }
}

/// The durable side of a live session.
pub(super) struct DurableLive {
    pub(super) driver: Durable,
    pub(super) stop: StopPolicy,
    /// The session's event observer; a station stop aborts it.
    pub(super) observer: tokio::task::AbortHandle,
}

impl SessionTarget {
    /// Launches new sessions through shims when their adapter has a durable
    /// driver (see the module documentation).
    #[must_use]
    pub fn with_shims(mut self, shims: ShimSettings) -> Self {
        self.shims = Some(shims);
        self
    }

    /// The durable launch of `spec`, if this target hosts shims and the
    /// adapter has a durable driver.
    pub(super) fn durable_launch(
        &self,
        adapter: &Arc<dyn ProviderAdapter>,
        spec: &SessionSpec,
    ) -> Option<(ShimSettings, DurableLaunch)> {
        let shims = self.shims.clone()?;
        adapter.durable_launch(spec).ok().map(|l| (shims, l))
    }

    /// Takes over the sessions that a station start restored.
    pub async fn adopt(&self, restored: Vec<Restored>, stop: StopPolicy, from: u64) {
        for r in restored {
            let summary = self
                .store
                .summary(r.session_id.clone())
                .await
                .ok()
                .flatten();
            let cwd = summary
                .as_ref()
                .and_then(|s| s.cwd.clone())
                .unwrap_or_default();
            let turns = self.turns(&r.session_id, Path::new(&cwd)).await;
            turns.set_process(r.process.clone());
            // A session whose exit a restored driver already committed:
            // its observer still reads every event from `from` (turns that
            // the provider completed while the station was down), then ends
            // the session when it has caught up. It takes no commands.
            if summary.is_some_and(|s| matches!(s.state, SessionState::Exited | SessionState::Lost))
            {
                tokio::spawn(observe(
                    self.store.clone(),
                    Arc::clone(&self.sessions),
                    r.session_id.clone(),
                    r.process,
                    r.driver.process(),
                    turns,
                    from,
                    Drain::ToHead,
                ));
                continue;
            }
            // `from` is the head before the restoration: the observer sees
            // every event that the driver committed while it replayed.
            self.insert_durable(&r.session_id, r.process, turns, r.driver, stop, from);
        }
    }

    async fn turns(&self, session_id: &SessionId, cwd: &Path) -> Arc<SessionTurns> {
        SessionTurns::open(
            self.store.clone(),
            Arc::clone(&self.ids),
            self.git.clone(),
            self.limits,
            &self.fences,
            session_id.clone(),
            cwd,
        )
        .await
    }

    fn insert_durable(
        &self,
        session_id: &SessionId,
        process: ProcessIncarnation,
        turns: Arc<SessionTurns>,
        driver: Durable,
        stop: StopPolicy,
        from: u64,
    ) {
        // The observer is spawned while this lock is held: its removal of
        // the entry at the session's end takes the same lock, so it never
        // runs before the entry exists.
        let Ok(mut map) = self.sessions.lock() else {
            return;
        };
        let observer = tokio::spawn(observe(
            self.store.clone(),
            Arc::clone(&self.sessions),
            session_id.clone(),
            process.clone(),
            driver.process(),
            Arc::clone(&turns),
            from,
            Drain::ToTerminal,
        ))
        .abort_handle();
        map.insert(
            session_id.clone(),
            Arc::new(Mutex::new(Live {
                process,
                session: None,
                turns,
                durable: Some(DurableLive {
                    driver,
                    stop,
                    observer,
                }),
            })),
        );
    }

    /// A station stop leaves every session under a shim running: its
    /// driver and observer stop, its shim and provider do not (ADR-005).
    pub(super) async fn detach_durable(live: &mut Live) {
        if let Some(durable) = live.durable.as_ref() {
            durable.observer.abort();
            durable.driver.abandon().await;
        }
    }

    /// Starts a session under a shim and writes its first input.
    pub(super) async fn start_durable(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
        prompt: Option<&InputRecord>,
        shims: &ShimSettings,
        launch: DurableLaunch,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Dispatch {
        let session_id = effect.session_id.clone();
        if super::setup_task::cancelled(cancel) {
            // A stop came during the setup: no shim is launched.
            self.start_failed(&session_id, crate::workspace::setup::STOPPED_MESSAGE)
                .await;
            return Dispatch::NotDispatched(super::REASON_SETUP_STOPPED.into());
        }
        let turns = self.turns(&session_id, Path::new(&spec.cwd)).await;
        let from = *self.store.subscribe().borrow();
        let attached = match launch_shim(
            &self.store,
            &shims.host,
            &session_id,
            &launch.process,
            AckMode::Manual,
        )
        .await
        {
            Ok(attached) => attached,
            Err(LaunchError::Prepare(e)) => {
                self.start_failed(&session_id, &e.to_string()).await;
                return Dispatch::NotDispatched(format!("start_failed: {e}"));
            }
            Err(LaunchError::Settled { error, settled })
                if settled == reason::SHIM_NEVER_STARTED =>
            {
                self.start_failed(&session_id, &error).await;
                return Dispatch::NotDispatched(format!("start_failed: {error}"));
            }
            // The provider may have run: the start stays uncertain.
            Err(e) => return Dispatch::Unknown(e.to_string()),
        };
        let control = Arc::clone(&attached.process);
        let pid = attached.record.provider_identity.as_ref().map(|i| i.pid);
        let process: ProcessIncarnation = new_id(self.ids.as_ref());
        if let Err(e) = self
            .store
            .process_started(session_id.clone(), process.clone(), pid)
            .await
        {
            tracing::error!(error = %e, "the process could not be recorded; stopping it");
            let _report = control.kill().await;
            return Dispatch::Unknown(format!("store: {e}"));
        }
        turns.set_process(process.clone());
        let driver = match launch.core {
            DurableConfig::Claude(config) => DurableClaude::open(&self.store, attached, config)
                .await
                .map(Durable::Claude),
            DurableConfig::Codex { config, thread } => {
                DurableCodex::open(&self.store, attached, config, &thread)
                    .await
                    .map(Durable::Codex)
            }
        };
        let driver = match driver {
            Ok(driver) => driver,
            Err(e) => {
                // The driver's pump commits the exit of the killed provider.
                let _report = control.kill().await;
                self.observe_end(&session_id, &process).await;
                return Dispatch::NotDispatched(format!("start_failed: {e}"));
            }
        };
        self.insert_durable(
            &session_id,
            process,
            Arc::clone(&turns),
            driver.clone(),
            launch.stop,
            from,
        );
        if prompt.is_some() && super::setup_task::cancelled(cancel) {
            // A stop came during the setup: the first input is not sent.
            let not_sent = Dispatch::NotDispatched(super::REASON_SETUP_STOPPED.into());
            if let Err(e) = self
                .store
                .first_input_failed(effect.effect_id.clone(), not_sent)
                .await
            {
                tracing::error!(error = %e, "the first input failure was not stored");
            }
            return Dispatch::Confirmed;
        }
        if let Some(input) = prompt {
            let fence = turns.fence().await;
            turns.begin_input(&fence, &input.input_id).await;
            // The fence and the checkpoint work can wait for long: the last
            // check is right before the delivery, with no await between.
            if super::setup_task::cancelled(cancel) {
                drop(fence);
                turns.input_not_sent(&input.input_id).await;
                let not_sent = Dispatch::NotDispatched(super::REASON_SETUP_STOPPED.into());
                if let Err(e) = self
                    .store
                    .first_input_failed(effect.effect_id.clone(), not_sent)
                    .await
                {
                    tracing::error!(error = %e, "the first input failure was not stored");
                }
                return Dispatch::Confirmed;
            }
            turns.input_sent(&input.input_id);
            let sent = match driver.first_input(effect).await {
                Ok(()) => Dispatch::Sent,
                Err(e) => Dispatch::NotDispatched(e.to_string()),
            };
            drop(fence);
            if matches!(sent, Dispatch::NotDispatched(_)) {
                turns.input_not_sent(&input.input_id).await;
            }
            if !matches!(sent, Dispatch::Sent) {
                // The spawn is the provider's acceptance of the start; the
                // first input gets its own durable evidence (GATE-M2 P2-a).
                if let Err(e) = self
                    .store
                    .first_input_failed(effect.effect_id.clone(), sent)
                    .await
                {
                    tracing::error!(error = %e, "the first input failure was not stored");
                }
            }
        }
        Dispatch::Confirmed
    }

    async fn observe_end(&self, session_id: &SessionId, process: &ProcessIncarnation) {
        if let Err(e) = self
            .store
            .process_ended(process.clone(), "start_failed".to_owned())
            .await
        {
            tracing::error!(error = %e, session = %session_id, "process end not stored");
        }
    }

    /// One claimed effect of a session under a shim.
    pub(super) async fn operate_durable(
        &self,
        effect: &Effect,
        durable: &DurableLive,
        turns: &Arc<SessionTurns>,
    ) -> Dispatch {
        match &effect.payload {
            EffectPayload::Stop { how } => {
                let process = durable.driver.process();
                let (how, mut policy) = (*how, durable.stop);
                if how == StopMode::Graceful && durable.driver.idle() {
                    // A user's async hook can keep an idle CLI alive after
                    // stdin EOF (DL-141).
                    policy.grace = DEFAULT_IDLE_GRACE.min(policy.grace);
                }
                // `session.ended` confirms the stop (the driver commits it).
                tokio::spawn(async move {
                    match how {
                        StopMode::Graceful => drop(process.stop(policy).await),
                        StopMode::Kill => drop(process.kill().await),
                    }
                });
                Dispatch::Sent
            }
            EffectPayload::Prompt { input } | EffectPayload::FollowUp { input } => {
                let fence = turns.fence().await;
                turns.begin_input(&fence, &input.input_id).await;
                turns.input_sent(&input.input_id);
                let sent = durable.driver.apply(effect).await;
                if matches!(sent, Dispatch::NotDispatched(_)) {
                    turns.input_not_sent(&input.input_id).await;
                }
                sent
            }
            EffectPayload::Steer { .. } => {
                let _fence = turns.fence().await;
                durable.driver.apply(effect).await
            }
            EffectPayload::Revert { revert_id, .. } => {
                if let (Durable::Codex(driver), Some(process)) =
                    (&durable.driver, effect.process_incarnation.as_ref())
                {
                    let env = super::super::revert_run::RevertEnv {
                        store: &self.store,
                        git: &self.git,
                        limits: self.limits,
                    };
                    let timeout = self
                        .shims
                        .as_ref()
                        .map_or(fleetdeck_providers::codex::ROLLBACK_TIMEOUT, |s| {
                            s.rollback_timeout
                        });
                    let mut rollback = CodexRollback { driver, timeout };
                    return super::super::revert_run::run(
                        env,
                        turns,
                        &mut rollback,
                        process,
                        revert_id,
                    )
                    .await;
                }
                if let Err(e) = self
                    .store
                    .advance_revert(
                        revert_id.clone(),
                        &[RevertStep::Admitted],
                        RevertStep::Failed,
                        Some(REASON_REVERT_UNSUPPORTED.to_owned()),
                    )
                    .await
                {
                    return Dispatch::Unknown(format!("store: {e}"));
                }
                Dispatch::NotDispatched(REASON_REVERT_UNSUPPORTED.into())
            }
            EffectPayload::Start { .. } | EffectPayload::Resume { .. } => {
                Dispatch::NotDispatched("not_a_live_operation".into())
            }
            _ => durable.driver.apply(effect).await,
        }
    }
}

/// Feeds the committed events of a session under a shim to its turn
/// checkpoints, from the store sequence `from`. When the session ends, the
/// session leaves the live set, its process ends, and its shim row is
/// `exited`.
/// When an observer stops reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drain {
    /// At the session's terminal event (a live session).
    ToTerminal,
    /// Also when it has read up to the head (a session that ended before
    /// its adoption; its terminal event can be older than `from`).
    ToHead,
}

/// How long the end of a session waits for the shim's exit report.
const EXIT_REPORT_WAIT: Duration = Duration::from_secs(10);

// The observer task owns every value it needs; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
async fn observe(
    store: Store,
    sessions: Sessions,
    session_id: SessionId,
    process: ProcessIncarnation,
    shim: Arc<ShimProcess>,
    turns: Arc<SessionTurns>,
    from: u64,
    drain: Drain,
) {
    let mut head = store.subscribe();
    let mut cursor = from;
    loop {
        let read = store.read_after(cursor, OBSERVE_BATCH).await;
        let (events, last) = match read {
            Ok(ReadResult::Events { events, head }) => {
                let last = events.last().map_or(head, |e| e.station_seq);
                (events, last)
            }
            Ok(ReadResult::SnapshotRequired { head }) => (Vec::new(), head),
            Err(e) => {
                tracing::error!(error = %e, session = %session_id, "observer stopped");
                return;
            }
        };
        let fetched = cursor != last;
        cursor = last.max(cursor);
        let observed: Vec<Event> = events
            .into_iter()
            .filter(|e| e.session_id == session_id)
            .filter_map(|e| match e.event {
                EventBody::Known(event) => Some(event),
                EventBody::Unknown { .. } => None,
            })
            .collect();
        let ended = observed.iter().any(Event::is_terminal_for_session);
        if !observed.is_empty() && turns.observe(&observed).await {
            tokio::spawn(Arc::clone(&turns).capture_due());
        }
        if ended || (!fetched && drain == Drain::ToHead) {
            break;
        }
        if !fetched && head.changed().await.is_err() {
            return;
        }
    }
    end_observed(&store, &sessions, &session_id, process, &shim).await;
}

/// The end of a session under a shim: it leaves the live set, its process
/// ends, and its open shim rows are `exited`. The row reason keeps the
/// shim's evidence (DL-120): `exited` only when the shim's exit report
/// shows no surviving member of the provider's process group or session;
/// a surviving member or a missing report is not a proven end, so a later
/// `session.resume` stays refused.
async fn end_observed(
    store: &Store,
    sessions: &Sessions,
    session_id: &SessionId,
    process: ProcessIncarnation,
    shim: &ShimProcess,
) {
    // The terminal event can be committed before the shim's report is
    // read; wait for the attachment's end (bounded).
    let _ended = tokio::time::timeout(EXIT_REPORT_WAIT, shim.wait()).await;
    let why = shims::exit_reason(shim.exit_state().as_ref());
    if let Ok(mut map) = sessions.lock() {
        map.remove(session_id);
    }
    if let Err(e) = store
        .process_ended(process, REASON_PROCESS_EXIT.to_owned())
        .await
    {
        tracing::error!(error = %e, "process end not stored");
    }
    if let Ok(rows) = store.shims_of_session(session_id.clone()).await {
        for shim_id in rows {
            let _ended = store
                .shim_end(shim_id, ShimState::Exited, why.to_owned())
                .await;
        }
    }
}
