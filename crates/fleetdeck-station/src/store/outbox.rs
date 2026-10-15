//! The [`Store`] API of the effect outbox, the provider process lifecycle,
//! runtime requests and start recovery (PLAN §6.4, §6.5, A10; M2 P2-5).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use fleetdeck_proto::{EventEnvelope, RequestId, SessionId};
use rusqlite::Connection;
use tokio::sync::watch;

use super::effects::{self, Dispatch, Effect, EffectId, EffectState, ProcessIncarnation};
use super::recovery::{self, RecoveryReport};
use super::requests::{self, RuntimeRequest};
use super::{Result, Store, call};

impl Store {
    /// Runs `f` in one transaction that may append events, then publishes
    /// the new head.
    pub(super) async fn write<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, i64, &mut Vec<EventEnvelope>) -> Result<R> + Send + 'static,
    ) -> Result<(R, Vec<EventEnvelope>)> {
        self.write_then(f, |_| {}).await
    }

    /// [`Self::write`] with a step that runs on the database thread right
    /// after the commit succeeded, before the next job (DL-194 §9.15). It
    /// does not run when the transaction fails.
    pub(super) async fn write_then<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, i64, &mut Vec<EventEnvelope>) -> Result<R> + Send + 'static,
        after_commit: impl FnOnce(&R) + Send + 'static,
    ) -> Result<(R, Vec<EventEnvelope>)> {
        let now = self.now();
        let (value, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut events = Vec::new();
            let value = f(&tx, now, &mut events)?;
            tx.commit()?;
            after_commit(&value);
            Ok((value, events))
        })
        .await?;
        if let Some(last) = events.last() {
            self.publish(last.station_seq);
        }
        Ok((value, events))
    }

    /// Station start recovery (A10, ADR-001 §3.2): `live` lists the provider
    /// processes that survived the restart (none for direct children). After
    /// it commits, [`Self::decide`] admits commands.
    ///
    /// # Errors
    /// A database error; the store then stays closed for commands.
    pub async fn recover(&self, live: Vec<ProcessIncarnation>) -> Result<RecoveryReport> {
        let (report, _) = self
            .write(move |conn, now, out| recovery::recover(conn, &live, now, out))
            .await?;
        self.inner.recovered.store(true, Ordering::Release);
        self.wake_worker();
        Ok(report)
    }

    /// True after [`Self::recover`] committed.
    #[must_use]
    pub fn is_recovered(&self) -> bool {
        self.inner.recovered.load(Ordering::Acquire)
    }

    /// Records a new provider process of a session; effects queued from now
    /// on target it.
    ///
    /// # Errors
    /// [`super::StoreError::UnknownSession`] or a database error.
    pub async fn process_started(
        &self,
        session_id: SessionId,
        process: ProcessIncarnation,
        pid: Option<u32>,
    ) -> Result<()> {
        self.write(move |conn, now, _| {
            recovery::process_started(conn, &session_id, &process, pid, now)
        })
        .await
        .map(|_| ())
    }

    /// The provider process is gone while the station runs: its queued
    /// effects retire, its claims become `outcome_unknown`, its open requests
    /// expire with `process_lost`.
    ///
    /// # Errors
    /// A database error.
    pub async fn process_ended(
        &self,
        process: ProcessIncarnation,
        reason: String,
    ) -> Result<Vec<EventEnvelope>> {
        self.write(move |conn, now, out| recovery::process_ended(conn, &process, &reason, now, out))
            .await
            .map(|((), events)| events)
    }

    /// The live provider process of a session.
    ///
    /// # Errors
    /// A database error.
    pub async fn live_process(&self, session_id: SessionId) -> Result<Option<ProcessIncarnation>> {
        call(&self.inner.db, move |conn| {
            effects::live_process(conn, &session_id)
        })
        .await
    }

    /// Starts a new effect worker generation and returns its fence token.
    /// Claims of older workers become `outcome_unknown`.
    ///
    /// # Errors
    /// A database error.
    pub async fn register_worker(&self) -> Result<u64> {
        self.write(effects::register_worker).await.map(|(f, _)| f)
    }

    /// Claims the oldest queued effect. The claim is committed before the
    /// caller does any I/O.
    ///
    /// # Errors
    /// [`super::StoreError::Fenced`] when a newer worker exists, or a
    /// database error.
    pub async fn claim_next(&self, fence: u64) -> Result<Option<Effect>> {
        self.claim_next_held(fence, false).await
    }

    /// [`Self::claim_next`] that leaves a queued start or resume effect
    /// queued when `hold_starts` is true (the restart freeze).
    ///
    /// # Errors
    /// See [`Self::claim_next`].
    pub async fn claim_next_held(&self, fence: u64, hold_starts: bool) -> Result<Option<Effect>> {
        self.write(move |conn, now, out| effects::claim_next(conn, fence, hold_starts, now, out))
            .await
            .map(|(claimed, _)| claimed)
    }

    /// Records the dispatch result of a claimed effect.
    ///
    /// # Errors
    /// [`super::StoreError::Fenced`] or a database error.
    pub async fn settle(
        &self,
        effect_id: EffectId,
        fence: u64,
        dispatch: Dispatch,
    ) -> Result<Option<Effect>> {
        self.write(move |conn, now, out| {
            effects::settle(conn, &effect_id, fence, &dispatch, now, out)
        })
        .await
        .map(|(e, _)| e)
    }

    /// Records the failure of the first input of a start effect that
    /// spawned its process (GATE-M2 P2-a): the effect reason and the input
    /// evidence events, in one transaction. Returns false when the effect
    /// has no first input or `dispatch` is not a failure.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn first_input_failed(
        &self,
        effect_id: EffectId,
        dispatch: Dispatch,
    ) -> Result<bool> {
        self.write(move |conn, now, out| {
            effects::first_input_failed(conn, &effect_id, &dispatch, now, out)
        })
        .await
        .map(|(done, _)| done)
    }

    /// One effect.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn effect(&self, effect_id: EffectId) -> Result<Option<Effect>> {
        call(&self.inner.db, move |conn| {
            effects::effect(conn, &effect_id)
        })
        .await
    }

    /// The effects of a session, oldest first.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn session_effects(&self, session_id: SessionId) -> Result<Vec<Effect>> {
        call(&self.inner.db, move |conn| {
            effects::session_effects(conn, &session_id)
        })
        .await
    }

    /// The start and resume effects that are queued: accepted, and not
    /// claimed by the worker yet.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn queued_start_effects(&self) -> Result<Vec<Effect>> {
        call(&self.inner.db, |conn| effects::queued_start_effects(conn)).await
    }

    /// Ends every queued start or resume effect for a planned restart, in
    /// one transaction: each effect is retired (`station_restart`) and its
    /// session ends as a failed start with `message`. Returns the sessions.
    ///
    /// # Errors
    /// A database error; nothing is stored.
    pub async fn fail_queued_starts(&self, message: String) -> Result<Vec<SessionId>> {
        let (sessions, _) = self
            .write(move |conn, now, out| effects::fail_queued_starts(conn, &message, now, out))
            .await?;
        Ok(sessions)
    }

    /// Ends a claimed start effect that the restart discards, in one
    /// transaction: the effect is retired (`station_restart`) and its
    /// session ends as a failed start with `message`. False, and nothing
    /// stored, when the effect is no longer claimed with this fence.
    ///
    /// # Errors
    /// [`super::StoreError::Fenced`] or a database error.
    pub async fn fail_claimed_start(
        &self,
        effect_id: EffectId,
        fence: u64,
        message: String,
    ) -> Result<bool> {
        let (done, _) = self
            .write(move |conn, now, out| {
                fail_claimed_start(conn, &effect_id, fence, &message, now, out)
            })
            .await?;
        Ok(done)
    }

    /// One runtime request.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn runtime_request(&self, request_id: RequestId) -> Result<Option<RuntimeRequest>> {
        call(&self.inner.db, move |conn| {
            requests::request(conn, &request_id)
        })
        .await
    }

    /// The offline `deny_after` sweep (§6.5): queues a deny answer, `by:
    /// timeout`, for each overdue request. The session manager calls it
    /// while no deck is connected.
    ///
    /// # Errors
    /// A database error.
    pub async fn deny_overdue(&self) -> Result<Vec<Effect>> {
        let ids = Arc::clone(&self.inner.ids);
        let (queued, _) = self
            .write(move |conn, now, out| requests::deny_overdue(conn, ids.as_ref(), now, out))
            .await?;
        if !queued.is_empty() {
            self.wake_worker();
        }
        Ok(queued)
    }

    /// A counter that moves after each commit that queued effects.
    #[must_use]
    pub fn subscribe_outbox(&self) -> watch::Receiver<u64> {
        self.inner.outbox.subscribe()
    }

    pub(super) fn wake_worker(&self) {
        self.inner.outbox.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Wakes the effect worker: it looks at the outbox again (the end of a
    /// restart freeze).
    pub fn nudge_worker(&self) {
        self.wake_worker();
    }
}

/// Ends a start effect that this worker claimed and has not dispatched, for
/// a planned restart, in the caller's transaction: the effect is retired
/// (`RetiredNotDispatched`, [`effects::REASON_STATION_RESTART`]) and its session
/// gets the events of a failed start. False, and nothing stored, when the
/// effect is no longer `Claimed` with this fence token.
///
/// # Errors
/// [`StoreError::Fenced`] or a database error.
fn fail_claimed_start(
    conn: &Connection,
    id: &EffectId,
    fence: u64,
    message: &str,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<bool> {
    effects::check_fence(conn, fence)?;
    let Some(found) = effects::effect(conn, id)? else {
        return Ok(false);
    };
    if found.state != EffectState::Claimed || found.fence_token != Some(fence) {
        return Ok(false);
    }
    effects::transition(
        conn,
        &found,
        EffectState::RetiredNotDispatched,
        effects::REASON_STATION_RESTART,
        now,
        out,
    )?;
    effects::start_failed_events(conn, &found.session_id, message, now, out)?;
    Ok(true)
}
