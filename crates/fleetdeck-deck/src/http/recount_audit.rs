//! The audit rows of a usage recount (B20, design `usage-cost.md` §4.3 item
//! 3). The generic command route writes `ok` with the station target before
//! the receipt; for `session.usage_recount` that row only says that the deck
//! sent the command. So every row of a recount has the action
//! `session.usage_recount`, the target `<station>/<session>`, the command id
//! and a stage:
//!
//! - `stage=submitted` (`stage=submitted duplicate` for a repeated command
//!   id) and `stage=refused code=<code>` come from the generic route;
//! - `stage=outcome` comes from a watcher ([`watch_recount`]) that waits for
//!   the receipt or the delivery state of a new command. It does not depend
//!   on a browser that polls.

use std::sync::Arc;

use fleetdeck_proto::StationId;
use fleetdeck_proto::command::{DeckCommandState, ReceiptState, SessionCommand};
use fleetdeck_proto::{CommandId, board::CommandRequest};

use super::Board;
use crate::store::{AuditResult, DeckCommand, NewAudit};

/// The audit action of a recount.
const ACTION: &str = "session.usage_recount";

/// The longest code that the deck copies from a station's reject message.
const CODE_MAX: usize = 64;

/// What the audit of a recount names: the session and the command id.
#[derive(Clone)]
pub(super) struct RecountAudit {
    session: String,
    command: CommandId,
}

impl RecountAudit {
    /// The audit subject of a `usage_recount` request; `None` for any other
    /// command.
    pub(super) fn of(req: &CommandRequest) -> Option<Self> {
        let SessionCommand::UsageRecount { session_id } = &req.command else {
            return None;
        };
        Some(Self {
            session: session_id.as_str().to_owned(),
            command: req.command_id.clone(),
        })
    }

    /// The target of every row: `<station>/<session>`.
    pub(super) fn target(&self, station: &str) -> String {
        format!("{station}/{}", self.session)
    }

    /// The generic refusal row of the route, with the session target and
    /// the detail `stage=refused code=<code>`.
    pub(super) fn refused_row(&self, row: NewAudit, station: &str, code: &str) -> NewAudit {
        row.target(self.target(station))
            .detail(format!("stage=refused code={code}"))
    }

    fn row(&self, actor: &str, station: &str, result: AuditResult, detail: String) -> NewAudit {
        NewAudit::new(actor, ACTION, result)
            .target(self.target(station))
            .command(self.command.clone())
            .detail(detail)
    }
}

/// The code of a reject message that follows the station's form
/// `<code>: <text>`: the text before the first `": "` when it is only
/// `[a-z_]` and short, else `refused`. The station cannot put free text into
/// the audit.
fn reject_code(message: &str) -> &str {
    message
        .split_once(": ")
        .map(|(code, _)| code)
        .filter(|code| {
            !code.is_empty()
                && code.len() <= CODE_MAX
                && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
        .unwrap_or("refused")
}

/// The result and the detail of the `stage=outcome` row of a command, or
/// `None` while the command is still `pending`.
fn outcome(cmd: &DeckCommand) -> Option<(AuditResult, String)> {
    match cmd.state {
        DeckCommandState::Pending => None,
        DeckCommandState::Failed => Some((
            AuditResult::Failed,
            "stage=outcome delivery=failed".to_owned(),
        )),
        DeckCommandState::Ambiguous => Some((
            AuditResult::Failed,
            "stage=outcome delivery=ambiguous".to_owned(),
        )),
        DeckCommandState::Done => match cmd.receipt.as_ref() {
            Some(r) if r.state == ReceiptState::Accepted => {
                Some((AuditResult::Ok, "stage=outcome".to_owned()))
            }
            Some(r) => {
                let message = r.reject.as_ref().map_or("", |x| x.message.as_str());
                Some((
                    AuditResult::Refused,
                    format!("stage=outcome code={}", reject_code(message)),
                ))
            }
            // A done command has a receipt; without one, nothing is seen.
            None => None,
        },
    }
}

/// Watches a new recount command without a browser: waits for its receipt
/// or its delivery state for at most `board.stop_watch.limit` (the limit of a
/// stop outcome), then writes one row. A command that is still `pending` at
/// the limit, or an error of the wait, gives `stage=outcome_unobserved`.
pub(super) fn watch_recount(
    board: Arc<Board>,
    actor: String,
    station: StationId,
    audit: RecountAudit,
) {
    tokio::spawn(async move {
        let waited = board
            .link
            .wait_command(audit.command.clone(), board.stop_watch.limit)
            .await;
        let (result, detail) = match waited {
            Ok(cmd) => outcome(&cmd)
                .unwrap_or((AuditResult::Failed, "stage=outcome_unobserved".to_owned())),
            Err(e) => {
                tracing::warn!(error = %e, "the recount outcome was not observed");
                (AuditResult::Failed, "stage=outcome_unobserved".to_owned())
            }
        };
        board
            .store
            .audit_or_log(audit.row(&actor, &station.0, result, detail))
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::reject_code;

    #[test]
    fn a_reject_code_is_the_text_before_the_first_colon() {
        assert_eq!(
            reject_code("session_running: This session may still run."),
            "session_running"
        );
        assert_eq!(reject_code("no code here"), "refused");
        assert_eq!(reject_code("Not A Code: text"), "refused");
        assert_eq!(reject_code(": text"), "refused");
        assert_eq!(reject_code(&format!("{}: x", "a".repeat(65))), "refused");
    }
}
