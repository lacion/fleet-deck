//! The target-station rules of a fleet-control command (ADR-008 §5, §6.2).
//!
//! A command with `authority` is one that a coordinator sent through the
//! deck. Three places on the station check it:
//!
//! - [`check`], in the decision: the live setting `allow_fleet_control`,
//!   the target is not the caller (§4.2 rule 2), the target holds no active
//!   binding with a mutating class (rule 3), and no resume or mode change
//!   goes into a bypass mode (§5.2). A refusal is a stored `forbidden`.
//! - [`recheck`], at the dispatch of an effect that carries the authority,
//!   right before the provider write or launch: the same live setting and
//!   rules 3 and 2, then the round trip `coordinator.authority_check` to
//!   the deck (with the launch mode), then the station state again. The
//!   effect is never retried with a new id.
//! - [`repeated`] and [`admit_answer`]: the answer rules of §5.3 for a
//!   coordinator.

use fleetdeck_proto::command::{ApprovalDecision, CommandOrigin, CommandResult, SessionCommand};
use fleetdeck_proto::event::{ProviderKind, RequestKind, ResolvedBy, ResponseChannel};
use fleetdeck_proto::mutation::{
    AuthorityCheck, CommandAuthority, LaunchMode, PolicyNow, PolicyNowAnswer,
};
use fleetdeck_proto::{Reject, RejectCode, SessionId};
use fleetdeck_providers::codex::validate_question_answer;
use serde_json::Value;

use super::decide::{reject, result};
use super::policy::Policy;
use crate::coordinator::bindings::{Authority, Bindings};
use crate::store::{DecideError, Decision, Effect, RuntimeRequest};

/// The reason of an effect that the deck did not let run.
pub(super) const AUTHORITY_LOST: &str = "authority_lost";
/// The reason of an effect that the deck could not confirm.
pub(super) const AUTHORITY_UNKNOWN: &str = "authority_unknown";

const OFF: &str = "fleet control is off on this station";
const TARGET_IS_COORDINATOR: &str = "the target session is a coordinator";
const TARGET_IS_CALLER: &str = "a coordinator cannot act on its own session";
const BYPASS: &str = "a coordinator cannot use a bypass mode";
const EVERY_QUESTION: &str = "every question needs an answer";
const COORDINATOR_OPTION: &str =
    "a coordinator answers a question with its answers or text, never an option";

fn forbidden(message: &str) -> DecideError {
    reject(RejectCode::Forbidden, message)
}

/// True for a mode that skips the permission checks (ADR-008 §5.2): Claude
/// `bypassPermissions`, or Codex approval `never` with the sandbox
/// `danger-full-access`.
fn is_bypass(kind: Option<ProviderKind>, mode: Option<&str>) -> bool {
    use fleetdeck_providers::{claude, codex};
    match (kind, mode) {
        (Some(ProviderKind::Claude), Some(mode)) => mode == claude::BYPASS_MODE,
        (Some(ProviderKind::Codex), Some(mode)) => codex::Mode::parse(mode).is_some_and(|m| {
            m.approval_policy == "never" && m.sandbox == codex::FULL_ACCESS_SANDBOX
        }),
        _ => false,
    }
}

/// The decision rules of a command with authority (ADR-008 §6.2). A command
/// without authority passes unchanged.
///
/// # Errors
/// A stored `forbidden` rejection, or a database error.
pub(super) fn check(
    d: &Decision<'_>,
    policy: &Policy,
    bindings: &Bindings,
    command: &SessionCommand,
) -> Result<(), DecideError> {
    let Some(authority) = d.authority() else {
        return Ok(());
    };
    if !policy.allow_fleet_control() {
        return Err(forbidden(OFF));
    }
    let Some(target) = command.session_id() else {
        return Ok(());
    };
    if bindings.holds_mutating(target) {
        return Err(forbidden(TARGET_IS_COORDINATOR));
    }
    if *target == authority.caller_session {
        return Err(forbidden(TARGET_IS_CALLER));
    }
    // The mode that the session really gets: the new mode of `set_mode`, or
    // the stored mode of a resume. The ceiling is the deck's check.
    let mode = match command {
        SessionCommand::SetMode { mode, .. } => Some(Some(mode.clone())),
        SessionCommand::Resume { .. } => None,
        _ => return Ok(()),
    };
    let Some(summary) = d.summary(target)? else {
        return Ok(());
    };
    let mode = mode.unwrap_or_else(|| summary.mode.clone());
    if is_bypass(summary.provider, mode.as_deref()) {
        return Err(forbidden(BYPASS));
    }
    Ok(())
}

/// A coordinator answer to a request that has an answer already: the stored
/// rejection `already_answered` with the winner's `by` (ADR-008 §5.3).
fn already_answered(by: ResolvedBy) -> DecideError {
    let winner = serde_json::to_value(by)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    DecideError::Reject(Reject::new(
        RejectCode::AlreadyAnswered,
        format!("already answered by {winner}"),
    ))
}

/// The result of a second answer to one request. A board answer keeps the
/// first answer's outcome (L3); a coordinator answer is `already_answered`.
///
/// # Errors
/// The stored rejection `already_answered`, for a coordinator.
pub(super) fn repeated(
    session_id: &SessionId,
    origin: CommandOrigin,
    first: &crate::store::AnswerReceipt,
) -> Result<CommandResult, DecideError> {
    if origin == CommandOrigin::Coordinator {
        return Err(already_answered(first.by));
    }
    Ok(result(session_id, None))
}

/// Checks the payload of a first answer to an open request against the
/// provider's own rules, before the answer is claimed. Only questions have
/// payload rules here: an RPC question of Codex needs the answer map that
/// [`validate_question_answer`] accepts, and the free answer (`answer`) of
/// a message question needs a text. Other requests pass unchanged. Missing
/// request metadata fails closed.
///
/// A coordinator answers a question with the option `answer`, never with
/// another option (ADR-008 §5.3): a Codex question must have every
/// question answered; a Claude question needs `question_answers` (in
/// `updated_input`) that match the questions, and the decision is rewritten
/// here, before the claim, to the one-time `allow` with the answers in the
/// tool input and no permission rules.
pub(super) fn admit_answer(
    d: &Decision<'_>,
    session_id: &SessionId,
    request: &RuntimeRequest,
    decision: &mut ApprovalDecision,
    origin: CommandOrigin,
) -> Result<(), DecideError> {
    let coordinator = origin == CommandOrigin::Coordinator || d.authority().is_some();
    if request.kind != RequestKind::Question {
        return Ok(());
    }
    if coordinator && decision.option_id != CODEX_FREE_ANSWER {
        return Err(forbidden(COORDINATOR_OPTION));
    }
    if !coordinator
        && !request
            .options
            .iter()
            .any(|o| o.option_id == decision.option_id)
    {
        // An unknown option keeps its own refusal from the claim.
        return Ok(());
    }
    let missing = || {
        reject(
            RejectCode::InvalidCommand,
            "the station has no record of this question's input",
        )
    };
    let summary = d.summary(session_id)?.ok_or_else(missing)?;
    match summary.provider {
        Some(ProviderKind::Claude) if coordinator => {
            let open = summary
                .open_requests
                .iter()
                .find(|r| r.request_id == request.request_id)
                .ok_or_else(missing)?;
            *decision = claude_question_decision(open.input.as_ref(), decision)?;
            return Ok(());
        }
        Some(ProviderKind::Claude) => return Ok(()),
        Some(ProviderKind::Codex) => {}
        Some(ProviderKind::Unknown) | None => return Err(missing()),
    }
    // A coordinator option that the request does not offer keeps its own
    // refusal from the claim.
    if !request
        .options
        .iter()
        .any(|o| o.option_id == decision.option_id)
    {
        return Ok(());
    }
    let open = summary
        .open_requests
        .iter()
        .find(|r| r.request_id == request.request_id)
        .ok_or_else(missing)?;
    match open.response {
        ResponseChannel::RpcReply => {
            let params = open.input.as_ref().ok_or_else(missing)?;
            validate_question_answer(params, decision.updated_input.as_ref())
                .map_err(|e| reject(RejectCode::InvalidCommand, e.to_string()))?;
            match origin {
                CommandOrigin::Coordinator => every_question_answered(params, decision),
                _ => Ok(()),
            }
        }
        ResponseChannel::Message => {
            // The same rule as the Codex core: the free answer needs a
            // non-empty text; an offered option is its own answer.
            if decision.option_id == CODEX_FREE_ANSWER
                && decision.text.as_ref().is_none_or(String::is_empty)
            {
                return Err(reject(
                    RejectCode::InvalidCommand,
                    "the answer needs a message",
                ));
            }
            Ok(())
        }
    }
}

/// The option id of a Codex free answer (`codex/requests.rs`); the deck sends
/// it for every coordinator answer to a question.
const CODEX_FREE_ANSWER: &str = "answer";

/// The longest answer of a Claude question, in characters.
const MAX_CLAUDE_ANSWER_CHARS: usize = 4096;

/// The decision of a coordinator answer to a Claude `AskUserQuestion`: the
/// one-time `allow` (never `allow_always`) whose `updated_input` is the
/// tool input with its `answers` set, the shape of the board's answer form.
/// The `question_answers` of the command are in `updated_input`; they must
/// be an object whose keys are the question texts of the request and whose
/// values are strings.
fn claude_question_decision(
    input: Option<&Value>,
    decision: &ApprovalDecision,
) -> Result<ApprovalDecision, DecideError> {
    let invalid = |m: &str| reject(RejectCode::InvalidCommand, m);
    let Some(answers) = decision.updated_input.as_ref() else {
        return Err(invalid("a Claude question needs question_answers"));
    };
    let Some(Value::Object(tool_input)) = input else {
        return Err(invalid(
            "the station has no record of this question's input",
        ));
    };
    let mismatch = || invalid("the answers do not match the questions");
    let Some(given) = answers.as_object() else {
        return Err(mismatch());
    };
    let asked: Vec<&str> = tool_input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|q| q.get("question").and_then(Value::as_str))
        .collect();
    let fits = given.iter().all(|(key, value)| {
        asked.contains(&key.as_str())
            && value
                .as_str()
                .is_some_and(|v| v.chars().count() <= MAX_CLAUDE_ANSWER_CHARS)
    });
    // Every question needs its answer: an empty or partial map would let
    // the tool run with questions nobody answered.
    let complete = !asked.is_empty() && asked.iter().all(|q| given.contains_key(*q));
    if !fits || !complete {
        return Err(mismatch());
    }
    let mut updated = tool_input.clone();
    updated.insert("answers".to_owned(), answers.clone());
    Ok(ApprovalDecision {
        option_id: "allow".to_owned(),
        updated_input: Some(Value::Object(updated)),
        ..decision.clone()
    })
}

/// Every question that the request asked has an answer in the answer map.
fn every_question_answered(params: &Value, decision: &ApprovalDecision) -> Result<(), DecideError> {
    let answers = decision
        .updated_input
        .as_ref()
        .and_then(|input| input.get("answers"))
        .and_then(Value::as_object);
    let complete = params
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|q| q.get("id").and_then(Value::as_str))
        .all(|id| answers.is_some_and(|a| a.contains_key(id)));
    if complete {
        Ok(())
    } else {
        Err(reject(RejectCode::InvalidCommand, EVERY_QUESTION))
    }
}

/// Why an effect with authority does not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The authority ended (`authority_lost`).
    Lost,
    /// The deck did not confirm it (`authority_unknown`).
    Unknown,
}

impl Refusal {
    /// The `error` event code and the dispatch reason.
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Lost => AUTHORITY_LOST,
            Self::Unknown => AUTHORITY_UNKNOWN,
        }
    }

    /// The `error` event message.
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Lost => "the coordinator's authority ended before this action ran",
            Self::Unknown => "the deck did not confirm the coordinator's authority",
        }
    }
}

/// The station's own live state at the dispatch: the live setting
/// `allow_fleet_control` (read by `live` at this moment), rule 3 (the
/// target holds no active binding with a mutating class) and rule 2 (the
/// target is not the caller). No await: it never waits.
fn station_holds(
    bindings: &Bindings,
    live: &(dyn Fn() -> bool + Sync),
    effect: &Effect,
    authority: &CommandAuthority,
) -> Result<(), Refusal> {
    if !live()
        || bindings.holds_mutating(&effect.session_id)
        || authority.caller_session == effect.session_id
    {
        return Err(Refusal::Lost);
    }
    Ok(())
}

/// The dispatch recheck of an effect that carries an authority (ADR-008
/// §6.2). The caller runs it right before the provider write or launch,
/// after every awaited preparation. The station checks its own live
/// setting (`live` reads it each time it is called), rule 3 and rule 2,
/// then asks the deck (`coordinator.authority_check`, 5 s), then checks
/// the same station state again with no await after it. `launch` is the
/// provider launch or mode change that the effect is about to make (ADR-008
/// §5.2). No answer, or an answer that is not `valid`, refuses the effect.
/// An effect without authority passes.
pub(super) async fn recheck(
    bindings: &Bindings,
    live: &(dyn Fn() -> bool + Sync),
    effect: &Effect,
    launch: Option<LaunchMode>,
) -> Result<(), Refusal> {
    let Some(authority) = effect.payload.authority() else {
        return Ok(());
    };
    station_holds(bindings, live, effect, authority)?;
    let Some(command_id) = effect.command_id.clone() else {
        return Err(Refusal::Lost);
    };
    let check = AuthorityCheck {
        grant_id: authority.grant_id.clone(),
        generation: authority.generation,
        caller_station: authority.caller_station.clone(),
        caller_session: authority.caller_session.clone(),
        target_session: effect.session_id.clone(),
        command_id,
        launch,
    };
    match bindings.authority_check(&check).await {
        // The round trip can take seconds: the station state is read again.
        Authority::Valid => station_holds(bindings, live, effect, authority),
        Authority::Lost => Err(Refusal::Lost),
        Authority::Unknown => Err(Refusal::Unknown),
    }
}

/// The answer to `coordinator.policy_now` (ADR-008 §6.2): the live setting
/// `allow_fleet_control` of this station, and whether the caller session
/// still holds the binding of a grant here.
#[must_use]
pub fn policy_now_answer(
    policy: &Policy,
    bindings: &Bindings,
    query: &PolicyNow,
) -> PolicyNowAnswer {
    PolicyNowAnswer {
        fleet_control: policy.allow_fleet_control(),
        bound: bindings.holds_grant(&query.caller_session),
        binding_matches: query
            .binding
            .as_ref()
            .map(|b| bindings.binding_matches(&query.caller_session, b)),
    }
}
