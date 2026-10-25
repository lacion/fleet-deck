//! The dispatch of a `!` shell command of a Codex session (B8, DL-196).
//!
//! Codex runs the command itself (`thread/shellCommand`). The station
//! checks the row, takes the folder fence, advances the folder generation and
//! hands the effect to the provider write (`operate_provider`). The events
//! of the provider then settle the row (`store::shell_observe`).

use fleetdeck_proto::SessionId;

use super::SessionTarget;
use super::shell_run::{CAPTURE_BUSY, CAPTURE_WAIT};
use crate::store::shell_runs::ShellState;
use crate::store::{Dispatch, Effect, EffectPayload, EffectState};

impl SessionTarget {
    /// Settles a row that did not run: `state`, `text` as its stderr, then
    /// the wake of the advance loop.
    async fn codex_shell_stop(&self, id: &str, state: ShellState, text: Option<&str>) {
        let settled = self
            .store
            .shell_settle(id.to_owned(), state, None, None, text.map(str::to_owned))
            .await;
        if let Err(e) = settled {
            tracing::error!(error = %e, effect = %id, "a shell row was not settled");
        }
        self.shell_notify().notify_one();
    }

    /// Dispatches a Codex shell effect: the checks of the station, then the
    /// provider write under the folder fence.
    pub(super) async fn dispatch_codex_shell(&self, effect: &Effect) -> Dispatch {
        let EffectPayload::ShellCommand { worktree_root, .. } = &effect.payload else {
            return Dispatch::NotDispatched("not_a_shell_command".into());
        };
        let id = effect.effect_id.as_str();
        let session: &SessionId = &effect.session_id;
        match self.store.shell_set_running(id.to_owned()).await {
            Ok(true) => {}
            // A cancel is stored (or the row ended); the retirement settles it.
            Ok(false) => return Dispatch::NotDispatched("shell_cancelled".into()),
            Err(e) => {
                tracing::error!(error = %e, effect = %id, "a shell row did not start");
                return Dispatch::Unknown("shell_row_unreadable".into());
            }
        }
        let Some(live) = self.live(session) else {
            let text = "The session has no running provider.";
            self.codex_shell_stop(id, ShellState::Failed, Some(text))
                .await;
            return Dispatch::NotDispatched("no_process".into());
        };
        let turns = live.lock().await.turns.clone();
        // The wait ends when no capture is open or due; the guard stays as
        // the fence of the advance and the write.
        let Ok(fence) = tokio::time::timeout(CAPTURE_WAIT, turns.fence()).await else {
            self.codex_shell_stop(id, ShellState::Failed, Some(CAPTURE_BUSY))
                .await;
            return Dispatch::NotDispatched("shell_capture_busy".into());
        };
        if let Err(e) = self
            .store
            .shell_advance(id.to_owned(), worktree_root.clone(), true)
            .await
        {
            let text = format!("The folder generation did not advance: {e}");
            self.codex_shell_stop(id, ShellState::Failed, Some(&text))
                .await;
            return Dispatch::NotDispatched("shell_advance_failed".into());
        }
        let stored_cancel = match self.store.shell_run(id.to_owned()).await {
            Ok(row) => row.is_none_or(|r| r.cancel_requested),
            Err(e) => {
                let text = format!("The command row is unreadable: {e}");
                self.codex_shell_stop(id, ShellState::Failed, Some(&text))
                    .await;
                return Dispatch::NotDispatched("shell_row_unreadable".into());
            }
        };
        if stored_cancel {
            self.codex_shell_stop(id, ShellState::Cancelled, None).await;
            return Dispatch::NotDispatched("shell_cancelled".into());
        }
        if let Err(text) = self.shell_cwd(session).await {
            self.codex_shell_stop(id, ShellState::Cancelled, Some(&text))
                .await;
            return Dispatch::NotDispatched("shell_policy".into());
        }
        // The fence is still held while the provider gets the command.
        let sent = self.operate_provider(effect).await;
        drop(fence);
        match sent {
            Dispatch::Sent => {
                self.watch_codex_shell(effect);
                Dispatch::Sent
            }
            Dispatch::NotDispatched(reason) => {
                // The guarded settle wins over the `cancelled` of the retirement.
                let text = format!("The command was not sent: {reason}");
                self.codex_shell_stop(id, ShellState::Failed, Some(&text))
                    .await;
                Dispatch::NotDispatched(reason)
            }
            Dispatch::Unknown(reason) => {
                self.watch_codex_shell(effect);
                Dispatch::Unknown(reason)
            }
            other @ (Dispatch::Confirmed | Dispatch::Deferred) => other,
        }
    }

    /// After the wait, a command with no evidence yet awaits its proof.
    fn watch_codex_shell(&self, effect: &Effect) {
        let (wait, effect_id) = (self.shell_watch(), effect.effect_id.clone());
        let store = self.store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let id = effect_id.as_str().to_owned();
            let open = matches!(
                store.effect(effect_id).await,
                Ok(Some(e)) if matches!(e.state, EffectState::Claimed | EffectState::OutcomeUnknown)
            );
            let running = matches!(
                store.shell_run(id.clone()).await,
                Ok(Some(row)) if row.state == ShellState::Running
            );
            if open && running {
                let moved = store
                    .shell_await_proof(id.clone(), ShellState::Running)
                    .await;
                if let Err(e) = moved {
                    tracing::error!(error = %e, effect = %id, "a shell row did not await proof");
                }
            }
        });
    }
}
