//! The commit of a moved-in session (FG-9, design §12.3 "The commit point",
//! T4.2d): what the target must see before it stores `resumed`, for each
//! provider. The task reads the store (`Store::subscribe`, the live process,
//! the native id); it needs no change in the pump or the providers.
//!
//! - Claude: the process is alive `liveness` after the spawn, with no exit.
//! - Codex: the provider reported its native id within `resume`; it must be
//!   the expected id of a native move.
//!
//! A failure before the commit is `abandoned` (`move_resume_failed`,
//! `identity_changed`, `move_resume_timeout`) and ends the process.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::StopMode;
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::fleet_git::{MoveInState, code};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::manager::SessionTarget;
use crate::manager::land::move_in::MoveState;
use crate::store::Dispatch;
use crate::store::move_runs::Tombstone;
use crate::workspace::fleet_git::redact;

/// The longest pause between two looks at the store.
const POLL: Duration = Duration::from_millis(100);

/// What the commit of one move needs.
pub(super) struct Watch<'a> {
    pub move_id: &'a str,
    pub session: &'a SessionId,
    pub provider: ProviderKind,
    /// The native id of a native move; `None` for a branch-only move.
    pub native: Option<&'a str>,
    /// This station and the source station, for the texts.
    pub names: (&'a str, &'a str),
    pub moves: &'a Arc<MoveState>,
}

/// The name of a provider for the texts of a move.
#[must_use]
pub(in crate::manager) fn provider_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "Claude",
        ProviderKind::Codex => "Codex",
        ProviderKind::Unknown => "The provider",
    }
}

/// `move_resume_failed` before the commit point (design §12.7).
#[must_use]
pub(in crate::manager) fn resume_text(
    kind: ProviderKind,
    station: &str,
    source: &str,
    line: &str,
) -> String {
    format!(
        "{} could not resume the conversation on {station}: {line}. The session stays on {source}.",
        provider_name(kind)
    )
}

impl SessionTarget {
    /// Ends the provider process of `session_id` (graceful, then kill) and
    /// waits for its end. Nothing happens when no process lives.
    pub(in crate::manager) async fn end_process(&self, session_id: &SessionId) {
        self.stop_started(session_id, StopMode::Graceful).await;
    }

    /// True when the session has no live process, or has ended.
    async fn gone(&self, session: &SessionId) -> bool {
        match self.store.live_process(session.clone()).await {
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(e) => tracing::warn!(error = %e, "the live process is unreadable"),
        }
        match self.store.summary(session.clone()).await {
            Ok(Some(summary)) => matches!(summary.state, SessionState::Exited | SessionState::Lost),
            Ok(None) => true,
            Err(e) => {
                tracing::warn!(error = %e, "the session is unreadable");
                false
            }
        }
    }

    /// The last line of an exit: the last error of the session, redacted and
    /// cut, else a fixed text (T4.2 Q9).
    pub(in crate::manager) async fn exit_line(&self, session: &SessionId) -> String {
        let last = match self.store.summary(session.clone()).await {
            Ok(summary) => summary.and_then(|s| s.last_error),
            Err(e) => {
                tracing::warn!(error = %e, "the session is unreadable");
                None
            }
        };
        let line = last.map(|e| redact(&e.message)).unwrap_or_default();
        if line.is_empty() {
            "the process exited".to_owned()
        } else {
            line
        }
    }

    /// Waits for the commit and stores it. The dispatch is the settle of the
    /// effect: `Confirmed` (a process ran), also when the move was cancelled
    /// or abandoned.
    pub(super) async fn commit_watch(
        &self,
        w: &Watch<'_>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Dispatch {
        let timeouts = w.moves.timeouts();
        let claude = w.provider == ProviderKind::Claude;
        let limit = if claude {
            timeouts.liveness
        } else {
            timeouts.resume
        };
        let deadline = Instant::now() + limit;
        let mut head = self.store.subscribe();
        loop {
            if *cancel.borrow() {
                return Dispatch::Confirmed;
            }
            if !claude {
                match self.store.session_native_id(w.session.clone()).await {
                    Ok(Some(id)) => {
                        return match w.native {
                            Some(expected) if id != expected => {
                                self.abandon_move(w, code::IDENTITY_CHANGED).await
                            }
                            _ => self.commit(w).await,
                        };
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "the native id is unreadable"),
                }
            }
            if self.gone(w.session).await {
                return self.abandon_move(w, code::MOVE_RESUME_FAILED).await;
            }
            if Instant::now() >= deadline {
                if claude {
                    return self.commit(w).await;
                }
                return self.abandon_move(w, code::MOVE_RESUME_TIMEOUT).await;
            }
            tokio::select! {
                _ = head.changed() => {}
                () = tokio::time::sleep_until(deadline.min(Instant::now() + POLL)) => {}
                _ = cancel.changed() => {}
            }
        }
    }

    /// The commit transaction under the per-move lock (design §12.3): zero
    /// changed rows (a discard came first) end the process.
    async fn commit(&self, w: &Watch<'_>) -> Dispatch {
        let guard = w.moves.locks().lock(w.move_id).await;
        let committed = self
            .store
            .move_in_advance(
                w.move_id.to_owned(),
                MoveInState::Launching,
                MoveInState::Resumed,
                None,
                Tombstone::Refuse,
            )
            .await;
        drop(guard);
        match committed {
            Ok(true) => {}
            Ok(false) => self.end_process(w.session).await,
            Err(e) => {
                tracing::error!(error = %e, "the commit of a move was not stored");
                self.end_process(w.session).await;
            }
        }
        Dispatch::Confirmed
    }

    /// A failure before the commit: the process ends, the row is `abandoned`
    /// with `why` and its text (design §12.7).
    async fn abandon_move(&self, w: &Watch<'_>, why: &str) -> Dispatch {
        let (station, source) = w.names;
        let name = provider_name(w.provider);
        let text = if why == code::IDENTITY_CHANGED {
            format!(
                "{name} resumed another conversation on {station}. The session stays on {source}."
            )
        } else if why == code::MOVE_RESUME_TIMEOUT {
            format!(
                "{name} did not resume on {station} within 120 s. The session stays on {source}."
            )
        } else {
            let line = self.exit_line(w.session).await;
            resume_text(w.provider, station, source, &line)
        };
        self.end_process(w.session).await;
        let moved = self
            .store
            .move_in_advance(
                w.move_id.to_owned(),
                MoveInState::Launching,
                MoveInState::Abandoned,
                Some((why.to_owned(), text)),
                Tombstone::Ignore,
            )
            .await;
        if let Err(e) = moved {
            tracing::warn!(error = %e, "the abandoned move was not stored");
        }
        Dispatch::Confirmed
    }
}
