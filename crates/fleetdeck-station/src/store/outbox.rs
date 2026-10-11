//! The [`Store`] API of the effect outbox, the provider process lifecycle,
//! runtime requests and start recovery (PLAN §6.4, §6.5, A10; M2 P2-5).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use fleetdeck_proto::{EventEnvelope, RequestId, SessionId};
use rusqlite::Connection;
use tokio::sync::watch;

use super::effects::{self, Dispatch, Effect, EffectId, ProcessIncarnation};
use super::recovery::{self, RecoveryReport};
use super::requests::{self, RuntimeRequest};
use super::{Result, Store, call};

impl Store {
    /// Runs `f` in one transaction that may append events, then publishes
    /// the new head.
    async fn write<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, i64, &mut Vec<EventEnvelope>) -> Result<R> + Send + 'static,
    ) -> Result<(R, Vec<EventEnvelope>)> {
        let now = self.now();
        let (value, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut events = Vec::new();
            let value = f(&tx, now, &mut events)?;
            tx.commit()?;
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
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let claimed = effects::claim_next(&tx, fence, now)?;
            tx.commit()?;
            Ok(claimed)
        })
        .await
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
}
