//! The fleet operations of `coordinator.call` that need the whole session
//! list: group mail (R2.5) and a worker's question to its coordinator
//! (R2.3). They run inside the decision transaction of
//! [`super::coordinator`], like the other ops of [`super::coordinator_ops`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::command::{SessionCommand, UserInput};
use fleetdeck_proto::mutation::{CoordinatorDelivery, CoordinatorOp};
use fleetdeck_proto::orchestration::{
    MailDelivery, MailGroup, TaskCompletion, TaskState, mail_frame, question_frame,
};
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::Result;
use super::coordinator_ops::{Context, Decision, check_text, session_known};
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
