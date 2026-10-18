//! The live policy query of a lease (R13 P5b-1 task K1, design
//! `omarchy-p5b.md` §6.2, §6.3): `desktop.capture_policy_now` to the target.
//! Anything false or uncertain is a refusal: fail closed.

use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::{CapturePolicyAnswer, CapturePolicyNowParams};

use crate::store::desktop_recordings::RecordingRow;

/// The whole query (§6.3: the lease has 5 s, and this is a part of it).
pub(super) const POLICY_WAIT: Duration = Duration::from_secs(3);

/// True when the target allows the recording to go on: the answer is valid,
/// every setting and the lock state allow it, the monitor is allowed and
/// present, and the `desktop_epoch` is the `link_epoch` of the row. An error
/// answer, a timeout and a malformed answer are `false`.
pub(super) async fn allowed(control: &Control, row: &RecordingRow) -> bool {
    let params = serde_json::json!(CapturePolicyNowParams {
        capture_id: row.capture.capture_id.clone(),
        action_hash: row.capture.action_hash.clone(),
    });
    let outcome = tokio::time::timeout(
        POLICY_WAIT,
        control.request("desktop.capture_policy_now", None, params),
    )
    .await;
    let Ok(Ok(Outcome::Ok(value))) = outcome else {
        return false;
    };
    // A parse error is not logged: the answer could hold desktop data.
    let Ok(answer) = serde_json::from_value::<CapturePolicyAnswer>(value) else {
        return false;
    };
    answer.valid
        && answer.allow_desktop
        && answer.allow_desktop_capture
        && answer.monitor_allowed
        && answer.monitor_present
        && answer.unlocked
        && row.link_epoch.as_deref() == Some(answer.desktop_epoch.as_str())
}
