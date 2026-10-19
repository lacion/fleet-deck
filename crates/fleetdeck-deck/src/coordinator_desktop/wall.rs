//! The coordinator `wall_show` (R13 P6 §4.2, §6.2, §7, decisions OD-7 and
//! O12-4, the T5 brief items 6 and 12): a coordinator with the `desktop` class
//! shows one of its own sessions or artifacts on an existing owner-paired
//! wall. The call stores a coordinator directive; it launches nothing, sends no
//! `desktop.act` and returns only the directive generation.
//!
//! In order: the authorization (the caller), the checks of the display and of
//! the references as one read, the two live policies, the one admission
//! transaction, the audit row of the attempt, the deck-local recheck, the
//! `attempted` mark right before the write, the write
//! ([`crate::store::DeckStore::display_directive_coordinator`]), the final
//! operation state, the audit row of the outcome and the reply.
//!
//! An audit row names the display and the referenced identifiers only after
//! the operation exists; a refusal before that row records the code alone.

use std::time::Instant;

use fleetdeck_proto::coordinator_desktop::{WallContent, WallShowArguments};
use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::PolicyPurpose;
use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveFilter};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::CoordinatorBinding;
use serde_json::{Value, json};

use super::audit::{self, Facts};
use super::authorize::{Authorized, Held, local_pass, store_error, switches};
use super::calls::payload_hash;
use super::policy::{caller_policy, target_policy};
use super::queries::status_of;
use super::{reply_err, reply_ok};
use crate::link::DeckLink;
use crate::store::AuditResult;
use crate::store::coordinator_desktop::{
    Admission, DesktopOp, NewOp, OpOutcome, OpTool, request_key,
};
use crate::store::display_directives_coordinator::{CoordinatorRefs, CoordinatorSender};

/// The audit action of a coordinator directive (§12).
const ACTION: &str = "display.directive";

/// The directive of the call (§4.2): a session is a `focus` directive and an
/// artifact is an `artifact` directive, with a filter of the referenced
/// station only. The agent supplies no filter, batch, URL or other mode.
fn body_of(args: &WallShowArguments) -> DirectiveBody {
    match &args.content {
        WallContent::Session { session } => DirectiveBody::Focus {
            session: session.clone(),
            pin: args.pin,
            filter: Some(DirectiveFilter {
                stations: vec![session.station_id.clone()],
                repo: None,
            }),
        },
        WallContent::Artifact { artifact } => DirectiveBody::Artifact {
            artifact: artifact.clone(),
            pin: args.pin,
            filter: Some(DirectiveFilter {
                stations: vec![artifact.station_id.clone()],
                repo: None,
            }),
        },
    }
}

/// The referenced identifiers of the call, for an audit row after the
/// operation exists.
fn reference_of(args: &WallShowArguments) -> Value {
    match &args.content {
        WallContent::Session { session } => json!({
            "kind": "session",
            "station_id": session.station_id,
            "session_id": session.session_id,
        }),
        WallContent::Artifact { artifact } => json!({
            "kind": "artifact",
            "station_id": artifact.station_id,
            "artifact_id": artifact.artifact_id,
            "version": artifact.version,
        }),
    }
}

/// One call after its authorization.
struct Wall<'a> {
    link: &'a DeckLink,
    auth: &'a Authorized,
    args: &'a WallShowArguments,
}

impl Wall<'_> {
    fn facts<'f>(&'f self, operation_id: Option<&'f str>) -> Facts<'f> {
        Facts {
            action: ACTION,
            ctx: self.auth.ctx(),
            target: self.auth.target(),
            operation_id,
            window_id: None,
            monitor: None,
        }
    }

    /// The fixed keys that an audit row holds once the operation exists.
    fn named(&self) -> [(&'static str, Value); 2] {
        [
            ("display_id", json!(self.args.display_id)),
            ("reference", reference_of(self.args)),
        ]
    }

    /// A refusal before the operation exists: its audit row (the code alone)
    /// and its reply.
    async fn refuse(&self, refusal: &'static str) -> Outcome {
        let row = self
            .facts(None)
            .row(audit::REFUSED, AuditResult::Refused, Some(refusal), None);
        audit::write_or_log(&self.link.store, row).await;
        reply_err(refusal, None)
    }

    /// The caller policy and the target policy (purpose `desktop`, no
    /// monitor), one after the other, with the time that is left.
    async fn policies(
        &self,
        binding: &CoordinatorBinding,
        deadline: Instant,
    ) -> Result<(), &'static str> {
        let ctx = self.auth.ctx();
        caller_policy(
            self.link,
            &ctx.caller_station,
            &ctx.caller_session,
            binding,
            deadline,
        )
        .await?;
        target_policy(
            self.link,
            self.auth.route(),
            PolicyPurpose::Desktop,
            None,
            deadline,
        )
        .await
    }

    /// The sender of the directive: the grant, its binding generation and the
    /// caller.
    fn sender(&self) -> CoordinatorSender {
        let ctx = self.auth.ctx();
        CoordinatorSender::new(
            ctx.grant_id.clone(),
            ctx.generation,
            ctx.caller_station.clone(),
            ctx.caller_session.clone(),
        )
    }

    fn refs(&self) -> CoordinatorRefs {
        CoordinatorRefs {
            target: self.auth.target().clone(),
        }
    }

    /// Admits the call: the deck switches, then the one admission
    /// transaction.
    async fn admit(&self) -> Result<Admission, &'static str> {
        switches(self.link)?;
        let ctx = self.auth.ctx();
        let target = self.auth.target();
        let args = serde_json::to_value(self.args).map_err(|_| code::INVALID_PARAMS)?;
        let new = NewOp {
            operation_id: self.link.store.new_command_id().as_str().to_owned(),
            grant_id: ctx.grant_id.clone(),
            request_key: request_key(&self.args.request_id),
            payload_hash: payload_hash(args, target),
            tool: OpTool::WallShow,
            class: self.auth.class(),
            ctx: ctx.clone(),
            target_station: target.clone(),
            action_hash: None,
            resource: json!({ "display_id": self.args.display_id }),
            seconds: None,
            input_length: None,
        };
        self.link
            .store
            .desktop_admit(new, self.link.store.now_ms())
            .await
            .map_err(|e| store_error(&e))
    }

    /// Ends the operation and answers. The outcome row is written only for
    /// the first transition, so a stage is written once.
    async fn end(
        &self,
        op_id: &str,
        outcome: OpOutcome,
        directive_generation: Option<u64>,
    ) -> Outcome {
        let first = match self
            .link
            .store
            .desktop_op_finish(op_id, outcome.clone(), self.link.store.now_ms())
            .await
        {
            Ok(first) => first,
            Err(e) => {
                tracing::warn!(error = %e, "coordinator wall finish");
                false
            }
        };
        let (result, failure) = match &outcome {
            OpOutcome::Done(_) => (AuditResult::Ok, None),
            OpOutcome::Failed(c) => (AuditResult::Failed, Some(*c)),
            OpOutcome::Unknown => (AuditResult::Failed, Some(code::DESKTOP_TIMEOUT)),
        };
        if first {
            let mut extra = self.named().to_vec();
            if let Some(n) = directive_generation {
                extra.push(("directive_generation", json!(n)));
            }
            let row =
                self.facts(Some(op_id))
                    .row_with(audit::OUTCOME, result, failure, None, &extra);
            audit::write_or_log(&self.link.store, row).await;
        }
        match (outcome, failure) {
            (OpOutcome::Done(value), _) => reply_ok(value),
            (_, Some(c)) => reply_err(c, Some(op_id)),
            (_, None) => reply_err(code::DESKTOP_FAILED, Some(op_id)),
        }
    }

    /// Runs an accepted operation: the attempt row, the deck-local recheck,
    /// `attempted`, the write and the end.
    async fn run(&self, op: &DesktopOp) -> Outcome {
        let id = op.operation_id.as_str();
        let attempt = self.facts(Some(id)).row_with(
            audit::ATTEMPT,
            AuditResult::Ok,
            None,
            Some(op.charged),
            &self.named(),
        );
        // No operation starts without its decision record (§12).
        if let Err(e) = audit::write_required(&self.link.store, attempt).await {
            tracing::warn!(error = %e, "coordinator desktop audit");
            return self
                .end(id, OpOutcome::Failed(code::AUTHORITY_UNKNOWN), None)
                .await;
        }
        // The deck-local checks, right before the write (§6.2 step 10).
        let ctx = self.auth.ctx();
        let held = Held {
            grant_id: &ctx.grant_id,
            binding_id: &ctx.binding_id,
            generation: ctx.generation,
            caller: &ctx.caller_station,
            target: self.auth.target(),
            class: self.auth.class(),
            tool: None,
        };
        if let Err(refusal) = local_pass(self.link, &held).await {
            return self.end(id, OpOutcome::Failed(refusal), None).await;
        }
        // The operation is marked right before the write, as `desktop.act`
        // marks it before the send, so a crash leaves the same states.
        match self
            .link
            .store
            .desktop_op_attempted(id, self.link.store.now_ms())
            .await
        {
            Ok(true) => {}
            Ok(false) => return reply_err(code::AUTHORITY_UNKNOWN, Some(id)),
            Err(e) => {
                tracing::warn!(error = %e, "coordinator wall attempted");
                return reply_err(code::AUTHORITY_UNKNOWN, Some(id));
            }
        }
        let written = self
            .link
            .store
            .display_directive_coordinator(
                self.args.display_id.clone(),
                self.sender(),
                body_of(self.args),
                self.refs(),
                self.link.store.now_ms(),
            )
            .await;
        match written {
            Ok(Ok(view)) => {
                let done = OpOutcome::Done(json!({ "generation": view.generation }));
                self.end(id, done, Some(view.generation)).await
            }
            Ok(Err(refusal)) => self.end(id, OpOutcome::Failed(refusal), None).await,
            Err(e) => {
                let unknown = OpOutcome::Failed(store_error(&e));
                self.end(id, unknown, None).await
            }
        }
    }
}

/// One `wall_show` call after its authorization. `deadline` is the handler
/// deadline.
pub(super) async fn run(
    link: &DeckLink,
    auth: &Authorized,
    binding: &CoordinatorBinding,
    args: &WallShowArguments,
    deadline: Instant,
) -> Outcome {
    let this = Wall { link, auth, args };
    // The display and the references, as one read: the display is the
    // station's own (an unknown, a revoked and a foreign display are one
    // answer), then the grant and the reference rules.
    let checked = link
        .store
        .display_coordinator_precheck(
            args.display_id.clone(),
            this.sender(),
            body_of(args),
            this.refs(),
            link.store.now_ms(),
        )
        .await;
    match checked {
        Ok(Ok(())) => {}
        Ok(Err(refusal)) => return this.refuse(refusal).await,
        Err(e) => return this.refuse(store_error(&e)).await,
    }
    if let Err(refusal) = this.policies(binding, deadline).await {
        return this.refuse(refusal).await;
    }
    match this.admit().await {
        Err(refusal) => this.refuse(refusal).await,
        Ok(Admission::Existing(op)) => {
            reply_ok(serde_json::to_value(status_of(&args.request_id, &op)).unwrap_or(Value::Null))
        }
        Ok(Admission::Conflict) => this.refuse(code::REQUEST_ID_CONFLICT).await,
        Ok(Admission::Rejected { code, .. }) => this.refuse(code).await,
        Ok(Admission::Accepted(op)) => this.run(&op).await,
    }
}
