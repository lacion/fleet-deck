//! The fleet operations of `coordinator.call` that need the whole session
//! list: group mail (R2.5) and a worker's question to its coordinator
//! (R2.3). They run inside the decision transaction of
//! [`super::coordinator`], like the other ops of [`super::coordinator_ops`].

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::command::{SessionCommand, UserInput};
use fleetdeck_proto::mutation::CoordinatorDelivery;
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

/// `ask_coordinator`: a `question` completion of the caller's newest open
/// task, and the framed question as mail to the coordinator session that
/// created the task. A caller without an open task is rejected. A task
/// that the owner made, or whose coordinator session is gone, keeps the
/// question for the board and sends no mail.
pub(super) fn ask(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    question: &str,
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    if let Some(d) = check_text(question) {
        return Ok(d);
    }
    let row: Option<(String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT task_id, coordinator_station, coordinator_session FROM tasks
             WHERE station_id = ?1 AND session_id = ?2 AND state IN ('open', 'question')
             ORDER BY created_at DESC, task_id DESC LIMIT 1",
            params![grant.station_id.as_str(), grant.session_id.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((task_id, station, session)) = row else {
        return no(RejectCode::InvalidState, "the caller has no open task");
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
        .find(|s| s.station_id == grant.station_id && s.summary.session_id == grant.session_id)
        .and_then(|s| s.summary.callsign.clone())
        .unwrap_or_else(|| grant.session_id.0.clone());
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
