//! The save-time hint for `artifact_share` (FG-12, design §15.5, decision D8):
//! the deck refuses a new `artifact_share` that the exporting station reports
//! as exposed or as not checkable. Every use of the share still needs its own
//! authorization; with no such station, or no report, nothing is refused.

use axum::http::StatusCode;
use fleetdeck_proto::fleet_git::{
    Exposure, ExposureState, SharePathState, TaildriveReport, TaildriveState, code,
};

use super::Board;
use super::guard::ApiError;

fn label(e: Exposure) -> &'static str {
    match e {
        Exposure::ProjectHome => "a project home",
        Exposure::Worktree => "a session worktree",
        Exposure::DataDir => "the Fleet Deck data folder",
        Exposure::ClaudeHome => "the Claude home",
        Exposure::CodexHome => "the Codex home",
        Exposure::Ssh => "~/.ssh",
        Exposure::Gnupg => "~/.gnupg",
        Exposure::Aws => "~/.aws",
        Exposure::MemberDeny => "a member-hidden path",
        Exposure::Unknown => "an unknown path",
    }
}

fn refusal(code: &'static str, message: String) -> ApiError {
    let mut details = serde_json::Map::new();
    details.insert("field".to_owned(), "artifact_share".into());
    ApiError::new(StatusCode::BAD_REQUEST, code, message).with_details(details)
}

/// The refusal for `share` (`<machine>/<share>`) by one report, if any.
fn check(report: &TaildriveReport, name: &str) -> Option<ApiError> {
    let share = report.shares.iter().find(|s| s.name == name);
    if let Some(s) = share
        && (s.exposure_state == ExposureState::Exposed || !s.exposed.is_empty())
    {
        let mut what: Vec<&str> = s.exposed.iter().map(|e| label(*e)).collect();
        if what.is_empty() {
            what.push(label(Exposure::Unknown));
        }
        return Some(refusal(
            code::SHARE_EXPOSED,
            format!(
                "This share exposes {}. Pick another share.",
                what.join(", ")
            ),
        ));
    }
    let unverified = report.state != TaildriveState::Ok
        || share.is_none_or(|s| {
            s.path_state != SharePathState::Ok || s.exposure_state == ExposureState::Unknown
        });
    unverified.then(|| {
        refusal(
            code::SHARE_UNVERIFIED,
            "Fleet Deck cannot check this share now. Pick another share, or fix the warning on \
             the Stations page."
                .to_owned(),
        )
    })
}

/// Checks `share` against the newest report of the station that exports its
/// machine. Only the stations whose `taildrive.machine` equals the machine
/// (after lower-casing both) count.
///
/// # Errors
/// 400 `share_exposed` / `share_unverified` with `field = artifact_share`.
pub(super) async fn check_share(board: &Board, share: &str) -> Result<(), ApiError> {
    let Some((machine, name)) = share.split_once('/') else {
        return Ok(());
    };
    let machine = machine.to_lowercase();
    let stations = board.store.stations().await.map_err(ApiError::internal)?;
    let mut first_unverified = None;
    for st in &stations {
        let Some((info, _)) = board.store.station_info(&st.station_id) else {
            continue;
        };
        let Some(report) = info.taildrive else {
            continue;
        };
        if report.machine.as_deref().map(str::to_lowercase).as_deref() != Some(machine.as_str()) {
            continue;
        }
        match check(&report, name) {
            Some(e) if e.code == code::SHARE_EXPOSED => return Err(e),
            Some(e) => {
                first_unverified.get_or_insert(e);
            }
            None => {}
        }
    }
    first_unverified.map_or(Ok(()), Err)
}
