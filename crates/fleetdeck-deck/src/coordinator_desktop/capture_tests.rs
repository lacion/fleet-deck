//! The send of a coordinator screenshot (R13 P6 §6.2, §9): the wait and the
//! state of a send that the station did not answer with a result.

use super::*;

#[test]
fn the_deck_wait_is_35_s_cut_by_the_time_left_of_the_handler() {
    let far = Instant::now() + Duration::from_secs(40);
    assert_eq!(waits(far).short, Duration::from_millis(35_000));
    let near = Instant::now() + Duration::from_millis(200);
    assert!(waits(near).short <= Duration::from_millis(200));
    let past = Instant::now()
        .checked_sub(Duration::from_millis(5))
        .expect("an instant in the past");
    assert_eq!(waits(past).short, Duration::ZERO);
}

#[test]
fn a_deck_side_wait_or_a_lost_link_is_unknown_and_other_errors_are_failed() {
    for e in [RelayError::Timeout, RelayError::Unreachable] {
        assert_eq!(after_error(CallError::Relay(e)), Ok(OpOutcome::Unknown));
    }
    assert_eq!(
        after_error(CallError::Relay(RelayError::Failed)),
        Ok(OpOutcome::Failed("desktop_failed"))
    );
    // A wire answer is for the caller of this function to map.
    assert_eq!(
        after_error(CallError::Wire {
            code: "busy".into(),
            message: "x".into()
        }),
        Err(("busy".into(), "x".into()))
    );
}
