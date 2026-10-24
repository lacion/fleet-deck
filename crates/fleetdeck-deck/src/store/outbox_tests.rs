//! The unit tests of the outbox rules.

use super::mail::member_mail_valid;
use super::*;

/// NV-17: a sender that is not a member needs no share, so no table is
/// read.
#[test]
fn a_mail_of_the_owner_or_a_token_needs_no_share() {
    let conn = Connection::open_in_memory().unwrap();
    for sender in ["owner", "token:ci"] {
        assert!(member_mail_valid(&conn, sender, "st", "s1", 0).unwrap());
    }
}

#[test]
fn the_backoff_doubles_from_five_seconds_up_to_five_minutes() {
    assert_eq!(backoff(1), 5_000);
    assert_eq!(backoff(2), 10_000);
    assert_eq!(backoff(4), 40_000);
    assert_eq!(backoff(7), MAX_BACKOFF_MS);
    assert_eq!(backoff(u32::MAX), MAX_BACKOFF_MS);
}

/// ADR-008 §5.3: a station rejection `already_answered` ends the item
/// `dead`, and the reason names the code for `command_status`.
#[test]
fn a_station_rejection_already_answered_ends_the_item_with_its_code() {
    let rejected = |code| {
        let reject = fleetdeck_proto::Reject::new(code, "no");
        let receipt = fleetdeck_proto::Receipt::rejected("c".into(), reject, 0);
        commands::DeckCommand {
            command_id: "c".into(),
            station_id: "s".into(),
            method: "approval.answer".to_owned(),
            origin: "coordinator".to_owned(),
            envelope: Value::Null,
            retry_until: 0,
            state: DeckCommandState::Done,
            attempts: 1,
            sent_at: Some(0),
            receipt: Some(receipt),
            reason: None,
            author: None,
            created_at: 0,
            updated_at: 0,
        }
    };
    for (code, reason) in [
        (
            RejectCode::AlreadyAnswered,
            "rejected: already_answered: no",
        ),
        (
            RejectCode::AnswerCapReached,
            "rejected: answer_cap_reached: no",
        ),
    ] {
        let got = next(&rejected(code));
        assert_eq!(got, Some((OutboxState::Dead, Some(reason.to_owned()))));
    }
}

/// G1: the reason of a rejected receipt carries the station text, clean and
/// cut.
#[test]
fn a_rejected_reason_carries_the_station_text_cut_and_clean() {
    let reason = |message: &str| {
        let reject = fleetdeck_proto::Reject::new(RejectCode::Forbidden, message);
        rejected_reason(Some(&reject))
    };
    assert_eq!(reason(""), "rejected: forbidden");
    assert_eq!(reason("a\nb"), "rejected: forbidden: a b");
    let long = reason(&"x".repeat(400));
    assert_eq!(long, format!("rejected: forbidden: {}…", "x".repeat(300)));
    assert_eq!(rejected_reason(None), "rejected: unknown");
}
