//! The fleet operations of `coordinator.call` that need the whole session
//! list: group mail (R2.5) and a worker's question to its coordinator
//! (R2.3). They run inside the decision transaction of
//! [`super::coordinator`], like the other ops of [`super::coordinator_ops`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use fleetdeck_proto::command::{
    ApprovalDecision, MAX_TITLE_CHARS, SessionCommand, SessionSpec, UserInput, WorkspaceRequest,
};
use fleetdeck_proto::event::{OptionKind, RequestKind};
use fleetdeck_proto::mutation::{CoordinatorDelivery, CoordinatorOp, ProjectTarget};
use fleetdeck_proto::orchestration::{
    MailDelivery, MailGroup, TaskCompletion, TaskState, mail_frame, question_frame,
};
use fleetdeck_proto::session::OpenRequest;
use fleetdeck_proto::{
    EventEnvelope, ProviderInstanceId, RejectCode, RequestId, SessionId, SessionSummary, StationId,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::atomic::Ordering;

use super::Result;
use super::coordinator::strict_target;
use super::coordinator_ops::{
    Context, Decision, check_text, enqueue, family_of, is_bypass, mode_within, session_known,
    spawn_target, start,
};
use super::fleet::{NewMail, group_mail_id, group_members, insert_mail, write_completion};
use super::grants::Grant;
use super::outbox::NewOutboxItem;
use crate::link::router::CommandKind;

/// How long a coordinator intent waits in the deck outbox: 24 h.
const INTENT_TTL_MS: i64 = 24 * 60 * 60 * 1000;

fn no(code: RejectCode, message: impl Into<String>) -> Result<Decision> {
    Ok(Decision::Rejected(code, message.into()))
}

fn mail_item(station: &StationId, command: SessionCommand, now: i64) -> Result<NewOutboxItem> {
    Ok(NewOutboxItem {
        station_id: station.clone(),
        origin: "coordinator".to_owned(),
        payload: serde_json::to_value(CommandKind::Session { command })?,
        expires_at: now.saturating_add(INTENT_TTL_MS),
    })
}

fn follow_up(session: &SessionId, framed: String) -> SessionCommand {
    SessionCommand::FollowUp {
        session_id: session.clone(),
        input: UserInput::new(framed),
    }
}

/// `send_mail` to a group: one mail per live session of the group on the
/// grant's stations, without the caller. The mail ids are `<command_id>-<n>`.
/// No session in the group is a rejection, so the coordinator learns that
/// nobody got the mail.
pub(super) fn send_group(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    (group, text, delivery): (MailGroup, &str, CoordinatorDelivery),
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    if let Some(d) = check_text(text) {
        return Ok(d);
    }
    let sender = format!("coordinator {}", grant.session_id);
    let members: Vec<_> = group_members(&ctx.sessions, group)
        .into_iter()
        .filter(|s| grant.stations.contains(&s.station_id))
        .filter(|s| !(s.station_id == grant.station_id && s.summary.session_id == grant.session_id))
        .collect();
    if members.is_empty() {
        return no(RejectCode::InvalidState, "no live session is in the group");
    }
    if let Some(d) = members
        .iter()
        .find_map(|m| super::coordinator::v2_refusal(grant, ctx, &m.station_id))
    {
        return Ok(d);
    }
    let (mail_delivery, steer) = match delivery {
        CoordinatorDelivery::FollowUp => (MailDelivery::FollowUp, false),
        CoordinatorDelivery::Steer => (MailDelivery::Steer, true),
    };
    for (i, member) in members.iter().enumerate() {
        let n = i + 1;
        let session_id = member.summary.session_id.clone();
        let input = UserInput::new(mail_frame(&sender, text.trim()));
        let command = if steer {
            SessionCommand::Steer {
                session_id: session_id.clone(),
                input,
            }
        } else {
            SessionCommand::FollowUp {
                session_id: session_id.clone(),
                input,
            }
        };
        let mail = NewMail {
            mail_id: group_mail_id(command_id, n),
            sender: sender.clone(),
            session_id,
            delivery: mail_delivery,
            text: text.trim().to_owned(),
            item: mail_item(&member.station_id, command, now)?,
        };
        insert_mail(tx, &mail, &format!("{}-{n}", ctx.outbox_id), now)?;
    }
    Ok(Decision::Accepted(None))
}

/// `ask_coordinator` of a coordinator grant: [`ask_for`] the grant's session.
pub(super) fn ask(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    question: &str,
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    ask_for(
        tx,
        ctx,
        (&grant.station_id, &grant.session_id),
        question,
        (command_id, false),
        now,
    )
}

/// `ask_coordinator`: a `question` completion of the newest open task of
/// `(station, session)`, and the framed question as mail to the coordinator
/// session that created the task. A caller without an open task is
/// rejected. A task that the owner made, or whose coordinator session is
/// gone, keeps the question for the board and sends no mail. With
/// `coordinated` (a worker without a grant) only a task that has a
/// coordinator session counts.
fn ask_for(
    tx: &Connection,
    ctx: &Context,
    (caller_station, caller_session): (&StationId, &SessionId),
    question: &str,
    (command_id, coordinated): (&str, bool),
    now: i64,
) -> Result<Decision> {
    if let Some(d) = check_text(question) {
        return Ok(d);
    }
    let coordinated_only = if coordinated {
        "AND coordinator_session IS NOT NULL"
    } else {
        ""
    };
    let row: Option<(String, Option<String>, Option<String>)> = tx
        .query_row(
            &format!(
                "SELECT task_id, coordinator_station, coordinator_session FROM tasks
                 WHERE station_id = ?1 AND session_id = ?2 AND state IN ('open', 'question')
                   {coordinated_only}
                 ORDER BY created_at DESC, task_id DESC LIMIT 1"
            ),
            params![caller_station.as_str(), caller_session.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((task_id, station, session)) = row else {
        return if coordinated {
            no(
                RejectCode::InvalidState,
                "no_coordinator: no open task of this session has a coordinator",
            )
        } else {
            no(RejectCode::InvalidState, "the caller has no open task")
        };
    };
    let completion = TaskCompletion {
        state: TaskState::Question,
        result: None,
        reason: Some("the worker asks its coordinator".to_owned()),
        question: Some(question.trim().to_owned()),
    };
    if let Err(message) = completion.check() {
        return no(RejectCode::InvalidCommand, message);
    }
    write_completion(tx, &task_id, &completion, now)?;
    let (Some(station), Some(session)) = (station, session) else {
        return Ok(Decision::Accepted(None));
    };
    let (station, session) = (StationId::from(station), SessionId::from(session));
    if !session_known(tx, &station, &session)? {
        return Ok(Decision::Accepted(None));
    }
    let worker = ctx
        .sessions
        .iter()
        .find(|s| &s.station_id == caller_station && &s.summary.session_id == caller_session)
        .and_then(|s| s.summary.callsign.clone())
        .unwrap_or_else(|| caller_session.0.clone());
    let command = follow_up(&session, question_frame(&worker, question.trim()));
    let mail = NewMail {
        mail_id: command_id.to_owned(),
        sender: format!("worker {worker}"),
        session_id: session.clone(),
        delivery: MailDelivery::FollowUp,
        text: question.trim().to_owned(),
        item: mail_item(&station, command, now)?,
    };
    insert_mail(tx, &mail, &ctx.outbox_id, now)?;
    Ok(Decision::Accepted(Some(session)))
}

/// Worker questions per session in one window (R2.3).
pub(super) const ASK_LIMIT: usize = 10;
/// The window of [`ASK_LIMIT`]: one hour.
const ASK_WINDOW_MS: i64 = 60 * 60 * 1000;

/// The `ask_coordinator` calls of the last hour per worker session, in
/// memory only.
#[derive(Debug, Default)]
pub(super) struct AskLimiter(Mutex<HashMap<(StationId, SessionId), VecDeque<i64>>>);

impl AskLimiter {
    /// Records a call at `now`; false when the window holds [`ASK_LIMIT`]
    /// calls of this session already.
    fn reserve(&self, station: &StationId, session: &SessionId, now: i64) -> bool {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, q| q.back().is_some_and(|t| *t > now - ASK_WINDOW_MS));
        let q = map.entry((station.clone(), session.clone())).or_default();
        while q.front().is_some_and(|t| *t <= now - ASK_WINDOW_MS) {
            q.pop_front();
        }
        if q.len() >= ASK_LIMIT {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// A `coordinator.call` of a session without a grant (R2.3): only
/// `ask_coordinator`. The session must exist under the calling station,
/// and it may ask at most [`ASK_LIMIT`] times per hour.
pub(super) fn decide_worker(
    tx: &Connection,
    limiter: &AskLimiter,
    ctx: &Context,
    (station, session): (&StationId, &SessionId),
    op: &CoordinatorOp,
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    let CoordinatorOp::AskCoordinator { question } = op else {
        return no(
            RejectCode::Forbidden,
            "forbidden: a session without a coordinator grant may only call `ask_coordinator`",
        );
    };
    if !session_known(tx, station, session)? {
        return no(
            RejectCode::Forbidden,
            "forbidden: the session is not a session of this station",
        );
    }
    if !limiter.reserve(station, session, now) {
        return no(
            RejectCode::InvalidState,
            format!("rate_limited: at most {ASK_LIMIT} questions per session per hour"),
        );
    }
    ask_for(
        tx,
        ctx,
        (station, session),
        question,
        (command_id, true),
        now,
    )
}

// ---------------------------------------------------------------------------
// Fleet control (ADR-008): the gate, the target rules and the new ops.
// ---------------------------------------------------------------------------

/// The provider family and the mode of a session from the read model.
fn session_family(
    ctx: &Context,
    station: &StationId,
    session: &SessionId,
) -> Option<(String, Option<String>)> {
    let card = ctx
        .sessions
        .iter()
        .find(|s| &s.station_id == station && &s.summary.session_id == session)?;
    let instance = card.summary.provider_instance.as_ref()?;
    let family = family_of(ctx.infos.get(station), instance)?;
    Some((family, card.summary.mode.clone()))
}

/// The mode rules of ADR-008 §5.2 for a session that moves into `mode`
/// (`None`: the provider default): the family has a ceiling, the mode is
/// within it and it is not a bypass mode.
fn mode_refusal(grant: &Grant, family: Option<&str>, mode: Option<&str>) -> Option<Decision> {
    let forbidden = |m: String| Some(Decision::Rejected(RejectCode::Forbidden, m));
    let Some(family) = family else {
        return forbidden("the provider of the session is not known".to_owned());
    };
    let Some(ceiling) = grant.max_modes.get(family) else {
        return forbidden(format!("the grant has no mode ceiling for `{family}`"));
    };
    if is_bypass(family, mode) {
        return forbidden("a coordinator never moves a session into a bypass mode".to_owned());
    }
    if !mode_within(family, mode, ceiling) {
        return forbidden(format!("the mode is above the grant's ceiling `{ceiling}`"));
    }
    None
}

/// `resume_session`: the effective resumed mode is the stored mode, or the
/// provider default (ADR-008 §5.2).
pub(super) fn resume(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    (station, session): (&StationId, &SessionId),
    now: i64,
) -> Result<Decision> {
    if let Some(d) = strict_target(tx, grant, (station, session), (true, now))? {
        return Ok(d);
    }
    let found = session_family(ctx, station, session);
    let (family, mode) = found.map_or((None, None), |(f, m)| (Some(f), m));
    if let Some(d) = mode_refusal(grant, family.as_deref(), mode.as_deref()) {
        return Ok(d);
    }
    let command = SessionCommand::Resume {
        session_id: session.clone(),
    };
    enqueue(tx, ctx, station, command, now)?;
    Ok(Decision::Accepted(Some(session.clone())))
}

/// `rename_session`: a title of 1 to 80 characters after trimming; no
/// clearing.
pub(super) fn rename(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    (station, session): (&StationId, &SessionId),
    title: &str,
    now: i64,
) -> Result<Decision> {
    if let Some(d) = strict_target(tx, grant, (station, session), (true, now))? {
        return Ok(d);
    }
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return no(
            RejectCode::InvalidCommand,
            format!("the title needs 1 to {MAX_TITLE_CHARS} characters"),
        );
    }
    let command = SessionCommand::Rename {
        session_id: session.clone(),
        title: Some(title.to_owned()),
    };
    enqueue(tx, ctx, station, command, now)?;
    Ok(Decision::Accepted(Some(session.clone())))
}

/// `set_mode`: the new mode is within the ceiling of the family and not a
/// bypass mode (ADR-008 §5.2).
pub(super) fn set_mode(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    (station, session): (&StationId, &SessionId),
    mode: &str,
    now: i64,
) -> Result<Decision> {
    if let Some(d) = strict_target(tx, grant, (station, session), (true, now))? {
        return Ok(d);
    }
    let family = session_family(ctx, station, session).map(|(f, _)| f);
    if let Some(d) = mode_refusal(grant, family.as_deref(), Some(mode)) {
        return Ok(d);
    }
    let command = SessionCommand::SetMode {
        session_id: session.clone(),
        mode: mode.to_owned(),
    };
    enqueue(tx, ctx, station, command, now)?;
    Ok(Decision::Accepted(Some(session.clone())))
}

/// The station and the path of a project.
fn project_row(tx: &Connection, id: &str) -> Result<Option<(StationId, String)>> {
    Ok(tx
        .query_row(
            "SELECT station_id, path FROM projects WHERE project_id = ?1",
            [id],
            |r| Ok((StationId(r.get(0)?), r.get(1)?)),
        )
        .optional()?)
}

/// `move_to_project`: the manual choice of the session, in the decision
/// transaction; the project and the session share a station (DL-176).
pub(super) fn project_assign(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    (station, session): (&StationId, &SessionId),
    target: &ProjectTarget,
    now: i64,
) -> Result<Decision> {
    if let Some(d) = strict_target(tx, grant, (station, session), (true, now))? {
        return Ok(d);
    }
    match target {
        ProjectTarget::Project { id } => {
            let Some((project_station, _)) = project_row(tx, id)? else {
                return no(RejectCode::InvalidCommand, "unknown project");
            };
            if &project_station != station {
                return no(
                    RejectCode::InvalidCommand,
                    "wrong_station: the session and the project are on different stations",
                );
            }
            super::project_sessions::set_project_tx(tx, station, session, Some(id), now)?;
        }
        ProjectTarget::None => {
            super::project_sessions::set_project_tx(tx, station, session, None, now)?;
        }
        ProjectTarget::Auto => {
            super::project_sessions::reset_project_tx(tx, station, session)?;
        }
    }
    ctx.flags.board.store(true, Ordering::SeqCst);
    Ok(Decision::Accepted(Some(session.clone())))
}

/// `start_in_project`: a start in the path of a project of a station of the
/// grant, under the spawn rules, the mode rules and the spawn cap.
pub(super) fn project_start(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    (project_id, prompt, instance, worktree): (&str, &str, Option<&ProviderInstanceId>, bool),
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    let Some((station, path)) = project_row(tx, project_id)? else {
        return no(RejectCode::InvalidCommand, "unknown project");
    };
    if !grant.stations.contains(&station) {
        return no(
            RejectCode::Forbidden,
            "the station of the project is not in the grant",
        );
    }
    if let Some(d) = super::coordinator::v2_refusal(grant, ctx, &station) {
        return Ok(d);
    }
    if let Some(d) = check_text(prompt) {
        return Ok(d);
    }
    let instance = match instance {
        Some(i) => i.clone(),
        None => match spawn_target(grant, ctx, None, Some(&station)) {
            Some((_, i)) => i,
            None => {
                return no(
                    RejectCode::InvalidCommand,
                    "no provider instance of the station can start a session",
                );
            }
        },
    };
    let spec = SessionSpec {
        provider_instance: instance,
        cwd: path,
        mode: None,
        model: None,
        title: None,
        coordinator: None,
    };
    let workspace = worktree.then(|| {
        Box::new(WorkspaceRequest::Worktree {
            branch: None,
            base: None,
        })
    });
    start(
        tx,
        grant,
        ctx,
        (
            &station,
            spec,
            Some(UserInput::new(prompt.trim())),
            workspace,
        ),
        command_id,
        now,
    )
}

/// The longest `question_answers` of an answer, in bytes of its JSON.
const MAX_QUESTION_ANSWERS_BYTES: usize = 16 * 1024;
/// The longest free text of an answer.
const MAX_ANSWER_TEXT_BYTES: usize = 4 * 1024;

/// What the read model knows about a request.
enum RequestState {
    Open(Box<OpenRequest>),
    Closed,
    Unknown,
}

/// The state of `request` of a session: open in the summary, else seen in
/// the timeline (answered or closed), else unknown.
fn request_state(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    request: &RequestId,
) -> Result<RequestState> {
    let summary: Option<String> = tx
        .query_row(
            "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![station.as_str(), session.0],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(text) = summary
        && let Ok(summary) = serde_json::from_str::<SessionSummary>(&text)
        && let Some(open) = summary
            .open_requests
            .into_iter()
            .find(|r| &r.request_id == request)
    {
        return Ok(RequestState::Open(Box::new(open)));
    }
    let mut stmt = tx.prepare(
        "SELECT envelope FROM timeline
         WHERE station_id = ?1 AND session_id = ?2 AND kind = 'approval.requested'",
    )?;
    let rows = stmt
        .query_map(params![station.as_str(), session.0], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for text in rows {
        let env: EventEnvelope = serde_json::from_str(&text)?;
        if let fleetdeck_proto::EventBody::Known(fleetdeck_proto::Event::ApprovalRequested {
            request_id,
            ..
        }) = env.event
            && &request_id == request
        {
            return Ok(RequestState::Closed);
        }
    }
    Ok(RequestState::Unknown)
}

fn kind_name(kind: Option<OptionKind>) -> &'static str {
    match kind {
        Some(OptionKind::AllowOnce) => "allow_once",
        Some(OptionKind::Deny) => "deny",
        _ => "none",
    }
}

type Pick = std::result::Result<String, (RejectCode, &'static str)>;

/// The option id of an answer to a question (ADR-008 section 5.3): the given
/// one, else the literal `answer` when the request has no options or has that
/// one. A question needs one of `option_id`, `question_answers`, `text`.
fn pick_for_question(request: &OpenRequest, given: (Option<&str>, bool, bool)) -> Pick {
    let invalid = |m| Err((RejectCode::InvalidCommand, m));
    let has = |id: &str| request.options.iter().any(|o| o.option_id == id);
    match given {
        (None, false, false) => invalid("a question needs option_id, question_answers or text"),
        (Some(id), _, _) if has(id) => Ok(id.to_owned()),
        (Some(_), _, _) => invalid("option_id is not an option"),
        (None, _, _) if request.options.is_empty() || has("answer") => Ok("answer".to_owned()),
        (None, _, _) => invalid("option_id is required for this question"),
    }
}

/// The option id of an answer to an approval: an option of the request with
/// the kind `allow_once` or `deny`, and no `question_answers`. `note` records
/// the kind of the option for the audit row.
fn pick_for_approval(
    request: &OpenRequest,
    (option_id, question_answers): (Option<&str>, bool),
    note: &dyn Fn(&str),
) -> Pick {
    let invalid = |m| Err((RejectCode::InvalidCommand, m));
    if question_answers {
        return invalid("an approval takes no question_answers");
    }
    let Some(id) = option_id else {
        return invalid("option_id is required");
    };
    let Some(option) = request.options.iter().find(|o| o.option_id == id) else {
        return invalid("option_id is not an option");
    };
    note(kind_name(option.kind));
    if matches!(option.kind, Some(OptionKind::AllowOnce | OptionKind::Deny)) {
        Ok(id.to_owned())
    } else {
        Err((
            RejectCode::Forbidden,
            "a coordinator answers only once, with allow once or deny",
        ))
    }
}

/// `answer_request` (ADR-008 §5): a one-time allow or a deny for an
/// approval; `question_answers` and/or `text` for a question. Counts against
/// the answer cap of the grant, in this transaction.
pub(super) fn answer(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    op: &CoordinatorOp,
    now: i64,
) -> Result<Decision> {
    let CoordinatorOp::Answer {
        station,
        session_id,
        request_id,
        option_id,
        question_answers,
        text,
    } = op
    else {
        return no(RejectCode::InvalidCommand, "not an answer");
    };
    let note = |kind: &str| {
        let mut slot = ctx
            .flags
            .note
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(format!(
            "grant={} request={request_id} kind={kind}",
            grant.grant_id
        ));
    };
    note("none");
    if let Some(d) = strict_target(tx, grant, (station, session_id), (true, now))? {
        return Ok(d);
    }
    let request = match request_state(tx, station, session_id, request_id)? {
        RequestState::Open(request) => request,
        RequestState::Closed => {
            return no(RejectCode::RequestClosed, "the request is closed already");
        }
        RequestState::Unknown => return no(RejectCode::UnknownRequest, "no such request"),
    };
    if text
        .as_deref()
        .is_some_and(|t| t.len() > MAX_ANSWER_TEXT_BYTES)
    {
        return no(RejectCode::InvalidCommand, "the text is longer than 4 KiB");
    }
    let too_big = question_answers
        .as_ref()
        .is_some_and(|v| v.to_string().len() > MAX_QUESTION_ANSWERS_BYTES);
    if too_big {
        return no(
            RejectCode::InvalidCommand,
            "question_answers is longer than 16 KiB",
        );
    }
    let question = request.request_kind == RequestKind::Question;
    let given = (option_id.as_deref(), question_answers.is_some());
    let picked = if question {
        note("question");
        pick_for_question(&request, (given.0, given.1, text.is_some()))
    } else {
        pick_for_approval(&request, given, &note)
    };
    let chosen = match picked {
        Ok(chosen) => chosen,
        Err((code, message)) => return no(code, message),
    };
    // The cap check and the count are in this write transaction; the deck
    // has one writer, so two decisions never take the last slot.
    let (cap, used): (Option<i64>, i64) = tx.query_row(
        "SELECT answer_cap, answer_used FROM tokens WHERE grant_id = ?1",
        [grant.grant_id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let cap = cap.unwrap_or(0);
    if used >= cap {
        return no(
            RejectCode::AnswerCapReached,
            format!("the grant has used its {cap} answers"),
        );
    }
    let decision = ApprovalDecision {
        option_id: chosen,
        updated_input: if question {
            question_answers.clone()
        } else {
            None
        },
        text: text.clone(),
        answered_by: Some(format!("coordinator:{}", grant.session_id)),
    };
    let command = SessionCommand::Answer {
        session_id: session_id.clone(),
        request_id: request_id.clone(),
        decision,
    };
    enqueue(tx, ctx, station, command, now)?;
    tx.execute(
        "UPDATE tokens SET answer_used = answer_used + 1 WHERE grant_id = ?1",
        [grant.grant_id.as_str()],
    )?;
    Ok(Decision::Accepted(Some(session_id.clone())))
}
