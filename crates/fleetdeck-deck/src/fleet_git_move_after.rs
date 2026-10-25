//! The steps of a move after the steps 1 to 4 (design §12.3, §12.4A; FG-9,
//! T4.4b): the poll of a `starting` move, the finish, the release, the abort
//! and "Undo move". Each runs one send of its step; the task loop calls it
//! again with the backoff of `next_step_at` until the station acknowledges.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::fleet_git::{
    MoveAbortAnswer, MoveDiscardAnswer, MoveFailAnswer, MoveIdParams, MoveInState,
    MoveReleaseAnswer, MoveReleaseState, MoveStatusAnswer, MoveStatusState,
};
use serde_json::json;

use crate::fleet_git_move_send::{Ask, Cmd, Fail, Run};
use crate::store::session_moves::MoveState;
use crate::store::session_moves_steps::MovePatch;

/// The text of a finish that the source refuses because it runs again.
const SOURCE_RUNNING: &str =
    "The source session runs again. Stop it, then Fleet Deck finishes the move.";

impl Run {
    /// `starting`: asks the target for its state (design §12.3).
    pub(crate) async fn poll(&self) {
        use MoveStatusState as S;
        let store = &self.ctx.store;
        let restart = self
            .ctx
            .restart
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.row.move_id);
        let d = self.ctx.opts.deadlines;
        let poll = i64::try_from(self.ctx.opts.poll.as_millis()).unwrap_or(10_000);
        let params = json!(MoveIdParams { move_id: self.id() });
        let answer: MoveStatusAnswer = match self
            .ask(&self.dst, "session.move_status", params, d.status)
            .await
        {
            Ask::Ok(a) => a,
            Ask::Err(_) | Ask::Failed(_) => {
                // A restart keeps its rule for the first answer that comes.
                if restart {
                    self.ctx
                        .restart
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(self.id());
                }
                let _ = store.move_next_at(self.id(), poll).await;
                return;
            }
        };
        let stored = |e: crate::store::DeckError| tracing::warn!(error = %e, "move status");
        match answer.state {
            S::Resumed | S::Released | S::Confirmed | S::ResumeFailed | S::Failed => {
                if let Err(e) = store
                    .move_advance(
                        self.id(),
                        &[MoveState::Starting],
                        MoveState::Committed,
                        MovePatch::default(),
                    )
                    .await
                {
                    stored(e);
                }
            }
            S::Abandoned | S::Discarded | S::LaunchRefused => {
                let code = answer.code.as_deref().unwrap_or("move_resume_failed");
                let text = answer.text.clone().unwrap_or_else(|| self.lost_text());
                self.abort_starting(code, &text).await;
            }
            S::Launching => {
                if self.row.step_attempts + 1 >= self.ctx.opts.polls_max {
                    self.abort_starting("move_resume_timeout", &self.timeout_text())
                        .await;
                } else if let Err(e) = store.move_retry_in(self.id(), poll, None).await {
                    stored(e);
                }
            }
            _ => {
                let late = self.ctx.store.now_ms().saturating_sub(self.row.created_at)
                    > i64::try_from(self.ctx.opts.starting_limit.as_millis()).unwrap_or(i64::MAX);
                if restart {
                    self.abort_starting("move_lost", &self.restart_text()).await;
                } else if late {
                    self.abort_starting("move_resume_timeout", &self.timeout_text())
                        .await;
                } else if let Err(e) = store.move_next_at(self.id(), poll).await {
                    stored(e);
                }
            }
        }
    }

    fn lost_text(&self) -> String {
        format!(
            "The target could not resume the conversation on {}. The session stays on {}.",
            self.dst_name, self.src_name
        )
    }

    fn timeout_text(&self) -> String {
        format!(
            "The target did not resume the conversation on {} in time. The session stays on {}.",
            self.dst_name, self.src_name
        )
    }

    fn restart_text(&self) -> String {
        format!(
            "The move stopped when the deck restarted. The session stays on {}.",
            self.src_name
        )
    }

    async fn abort_starting(&self, code: &str, text: &str) {
        let res = self
            .ctx
            .store
            .move_advance(
                self.id(),
                &[MoveState::Starting],
                MoveState::Aborting,
                MovePatch::note(code, text),
            )
            .await;
        if let Err(e) = res {
            tracing::warn!(error = %e, "move abort");
        }
    }

    /// The new session id: the row, else the status of the target.
    async fn dst_session(&self) -> Option<String> {
        if let Some(s) = &self.row.dst_session {
            return Some(s.clone());
        }
        let d = self.ctx.opts.deadlines;
        let params = json!(MoveIdParams { move_id: self.id() });
        let answer: MoveStatusAnswer = self
            .ask(&self.dst, "session.move_status", params, d.status)
            .await
            .fail()
            .ok()?;
        let session = answer.session_id?.0;
        let _ = self
            .ctx
            .store
            .move_set_dst(self.id(), session.clone())
            .await;
        Some(session)
    }

    async fn counted(&self) {
        let unit = self.ctx.opts.retry_unit_ms;
        if let Err(e) = self.ctx.store.move_sent(self.id(), unit).await {
            tracing::warn!(error = %e, "move backoff");
        }
    }

    async fn slow(&self, fail: Fail) {
        let slow = i64::try_from(self.ctx.opts.slow_retry.as_millis()).unwrap_or(60_000);
        let note = Some((fail.code, fail.text));
        if let Err(e) = self.ctx.store.move_retry_in(self.id(), slow, note).await {
            tracing::warn!(error = %e, "move retry");
        }
    }

    /// Step 5: `MoveFinish` to the source.
    pub(crate) async fn finish(&self) {
        let _ = self
            .ctx
            .store
            .move_advance(
                self.id(),
                &[MoveState::Committed],
                MoveState::Finishing,
                MovePatch::default(),
            )
            .await;
        let Some(to_session) = self.dst_session().await else {
            self.counted().await;
            return;
        };
        let command = SessionCommand::MoveFinish {
            session_id: self.session(),
            move_id: self.id(),
            to_station: self.dst.clone(),
            to_session: SessionId::from(to_session),
        };
        let d = self.ctx.opts.deadlines;
        match self
            .command(&self.src, &self.row.finish_command_id, command, d.step)
            .await
        {
            Cmd::Accepted(_) => {
                let res = self
                    .ctx
                    .store
                    .move_advance(
                        self.id(),
                        &[MoveState::Committed, MoveState::Finishing],
                        MoveState::Releasing,
                        MovePatch::clear(),
                    )
                    .await;
                if let Err(e) = res {
                    tracing::warn!(error = %e, "move finish");
                }
            }
            Cmd::Rejected(f) => {
                // The commit point passed: the deck never aborts.
                let fail = if f.code == "invalid_state" {
                    Fail::new("source_running", SOURCE_RUNNING)
                } else {
                    f
                };
                self.slow(fail).await;
            }
            Cmd::Failed(_) => self.counted().await,
        }
    }

    /// Step 6: `session.move_release` to the target.
    pub(crate) async fn release(&self) {
        let d = self.ctx.opts.deadlines;
        let params = json!(MoveIdParams { move_id: self.id() });
        match self
            .ask::<MoveReleaseAnswer>(&self.dst, "session.move_release", params, d.step)
            .await
        {
            Ask::Ok(a)
                if matches!(
                    a.state,
                    MoveReleaseState::Released | MoveReleaseState::Confirmed
                ) =>
            {
                let res = self
                    .ctx
                    .store
                    .move_advance(
                        self.id(),
                        &[MoveState::Releasing],
                        MoveState::Done,
                        MovePatch::clear(),
                    )
                    .await;
                if let Err(e) = res {
                    tracing::warn!(error = %e, "move release");
                }
            }
            Ask::Ok(_) | Ask::Failed(_) => self.counted().await,
            Ask::Err(f) if f.code == "invalid_state" => self.slow(f).await,
            Ask::Err(_) => self.counted().await,
        }
    }

    /// The abort (design §12.3): the discard on the target first, then the
    /// abort on the source. Both are idempotent, so each turn sends both.
    pub(crate) async fn aborting(&self) {
        let d = self.ctx.opts.deadlines;
        let params = json!(MoveIdParams { move_id: self.id() });
        let discard: Ask<MoveDiscardAnswer> = self
            .ask(&self.dst, "session.move_discard", params.clone(), d.discard)
            .await;
        let Ask::Ok(answer) = discard else {
            self.counted().await;
            return;
        };
        if !answer.is_ack() {
            // `settling` is not an acknowledgement.
            self.counted().await;
            return;
        }
        if answer.state == fleetdeck_proto::fleet_git::MoveDiscardState::Resumed {
            // The commit point passed: the move goes forward.
            let res = self
                .ctx
                .store
                .move_advance(
                    self.id(),
                    &[MoveState::Aborting],
                    MoveState::Committed,
                    MovePatch::clear(),
                )
                .await;
            if let Err(e) = res {
                tracing::warn!(error = %e, "move commit");
            }
            return;
        }
        match self
            .ask::<MoveAbortAnswer>(&self.src, "session.move_abort", params, d.abort)
            .await
        {
            Ask::Ok(_) => {
                let res = self
                    .ctx
                    .store
                    .move_advance(
                        self.id(),
                        &[MoveState::Aborting],
                        MoveState::Failed,
                        MovePatch::default(),
                    )
                    .await;
                if let Err(e) = res {
                    tracing::warn!(error = %e, "move failed");
                }
            }
            Ask::Err(_) | Ask::Failed(_) => self.counted().await,
        }
    }

    /// "Undo move" (design §12.4A): `session.move_fail` on the target, then
    /// `MoveUndo` on the source.
    pub(crate) async fn undoing(&self) {
        let d = self.ctx.opts.deadlines;
        let params = json!(MoveIdParams { move_id: self.id() });
        let failed: MoveFailAnswer = match self
            .ask(&self.dst, "session.move_fail", params, d.step)
            .await
        {
            Ask::Ok(a) => a,
            Ask::Err(_) | Ask::Failed(_) => {
                self.counted().await;
                return;
            }
        };
        if failed.state != MoveInState::Failed {
            let text = format!("The new session is in the state {:?}.", failed.state);
            self.slow(Fail::new("move_not_undoable", text)).await;
            return;
        }
        let Some(undo) = self.row.undo_command_id.clone() else {
            self.slow(Fail::new("internal", "The undo has no command id."))
                .await;
            return;
        };
        let command = SessionCommand::MoveUndo {
            session_id: self.session(),
            move_id: self.id(),
        };
        match self.command(&self.src, &undo, command, d.step).await {
            Cmd::Accepted(_) => {
                let res = self
                    .ctx
                    .store
                    .move_advance(
                        self.id(),
                        &[MoveState::Undoing],
                        MoveState::Undone,
                        MovePatch::clear(),
                    )
                    .await;
                if let Err(e) = res {
                    tracing::warn!(error = %e, "move undone");
                }
            }
            Cmd::Rejected(f) => self.slow(f).await,
            Cmd::Failed(_) => self.counted().await,
        }
    }
}
