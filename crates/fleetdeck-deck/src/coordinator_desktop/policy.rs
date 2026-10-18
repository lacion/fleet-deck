//! The live policy round trips of the coordinator desktop tools (R13 P6 §5.5,
//! §5.6, §9): the caller station (`coordinator.policy_now` with the expected
//! binding) and the target station (`desktop.policy_now`). Both fail closed,
//! and both wait at most the time that is left of the total deadline of the
//! handler: no time left is `authority_unknown`.

use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::coordinator_desktop_wire::{
    DesktopPolicyAnswer, DesktopPolicyNow, PolicyPurpose,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::{CoordinatorBinding, PolicyNow};
use fleetdeck_proto::{SessionId, StationId};

use crate::link::DeckLink;
use crate::link::desktop_authority::Route;

/// The wait of one round trip: its own limit, cut by what is left of
/// `deadline`. `None` when no time is left.
fn wait(limit_ms: u64, deadline: Instant) -> Option<Duration> {
    let left = deadline.checked_duration_since(Instant::now())?;
    let wait = left.min(Duration::from_millis(limit_ms));
    (!wait.is_zero()).then_some(wait)
}

/// The live policy of the caller station for `binding` (§5.6): the station
/// has fleet control on, the session is bound, and the binding is the
/// expected one. A timeout, a lost link and an answer without `binding_matches`
/// are `authority_unknown`; a false value is `not_granted`.
///
/// # Errors
/// `not_granted` or `authority_unknown`.
pub(crate) async fn caller_policy(
    link: &DeckLink,
    caller_station: &StationId,
    caller_session: &SessionId,
    binding: &CoordinatorBinding,
    deadline: Instant,
) -> Result<(), &'static str> {
    let Some(timeout) = wait(deadline_ms::CALLER_POLICY, deadline) else {
        return Err(code::AUTHORITY_UNKNOWN);
    };
    let query = PolicyNow {
        caller_session: caller_session.clone(),
        binding: Some(binding.clone()),
    };
    let Some(answer) = link.policy_now_with(caller_station, query, timeout).await else {
        return Err(code::AUTHORITY_UNKNOWN);
    };
    let Some(matches) = answer.binding_matches else {
        return Err(code::AUTHORITY_UNKNOWN);
    };
    if answer.fleet_control && answer.bound && matches {
        Ok(())
    } else {
        Err(code::NOT_GRANTED)
    }
}

/// The live policy of the target station (§5.5): fleet control and desktop
/// are on, and for a capture purpose also the capture setting and the monitor
/// rule. An uncertain answer is `authority_unknown`; a false value is
/// `not_granted`. (`_link` keeps the signature of the call layer; the route
/// holds the control channel.)
///
/// # Errors
/// `not_granted` or `authority_unknown`.
pub(crate) async fn target_policy(
    _link: &DeckLink,
    route: &Route,
    purpose: PolicyPurpose,
    monitor: Option<&str>,
    deadline: Instant,
) -> Result<(), &'static str> {
    let Some(timeout) = wait(deadline_ms::TARGET_POLICY, deadline) else {
        return Err(code::AUTHORITY_UNKNOWN);
    };
    let params = serde_json::to_value(DesktopPolicyNow {
        purpose,
        monitor: monitor.map(str::to_owned),
    })
    .map_err(|_| code::AUTHORITY_UNKNOWN)?;
    let outcome = tokio::time::timeout(
        timeout,
        route.control().request("desktop.policy_now", None, params),
    )
    .await;
    let value = match outcome {
        Ok(Ok(Outcome::Ok(value))) => value,
        // A refusal, a lost link and a timeout are all uncertain.
        _ => return Err(code::AUTHORITY_UNKNOWN),
    };
    let answer: DesktopPolicyAnswer =
        serde_json::from_value(value).map_err(|_| code::AUTHORITY_UNKNOWN)?;
    let capture_ok = purpose == PolicyPurpose::Desktop
        || (answer.allow_desktop_capture && answer.monitor_allowed);
    if answer.allow_fleet_control && answer.allow_desktop && capture_ok {
        Ok(())
    } else {
        Err(code::NOT_GRANTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_is_cut_by_the_deadline_and_no_time_left_is_none() {
        let far = Instant::now() + Duration::from_secs(60);
        assert_eq!(wait(3_000, far), Some(Duration::from_millis(3_000)));
        let near = Instant::now() + Duration::from_millis(200);
        let w = wait(3_000, near).expect("time is left");
        assert!(w <= Duration::from_millis(200), "{w:?}");
        let past = Instant::now()
            .checked_sub(Duration::from_millis(5))
            .expect("an instant in the past");
        assert_eq!(wait(3_000, past), None);
    }
}
