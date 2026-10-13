//! `assign auto` routing (M3 6-3, PLAN §7.3, DL-204): the pure choice of
//! the target session, and of the best station for a new session when no
//! session fits. The HTTP route (`http::assign_routes`) reads the read
//! model, asks the stations whether the repository exists, and sends the
//! task.
//!
//! The session ranking is v0's: idle first, then queued, then busy; then
//! the least buried (fewest undelivered mails); then the most recent
//! activity. The provider instance with the most room (R4.4, DL-161)
//! comes after the burial and before the recency. The station terms of v2: the link is up (the card is not
//! `unreachable`), the session works in the repository, and it runs the
//! provider.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use fleetdeck_proto::board::{DeckSession, StationView};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CardStatus, SessionId};

/// What the task needs from its target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Needs<'a> {
    pub repo: Option<&'a str>,
    pub provider: Option<ProviderKind>,
}

/// The rank of a card status; `None` for a card that takes no task
/// (needs you, ended, unreachable, lost).
#[must_use]
pub fn status_rank(status: CardStatus) -> Option<u8> {
    match status {
        CardStatus::Idle => Some(0),
        CardStatus::Queued => Some(1),
        CardStatus::Working | CardStatus::Verifying => Some(2),
        CardStatus::NeedsYou | CardStatus::Ended | CardStatus::Unreachable | CardStatus::Lost => {
            None
        }
    }
}

/// True when `cwd` is the repository `repo`, a directory in it, or one of
/// its Fleet Deck worktrees (`<repo>--fd-<callsign>`, DL-200).
#[must_use]
pub fn in_repo(cwd: &str, repo: &str) -> bool {
    let repo = repo.trim_end_matches('/');
    if repo.is_empty() {
        return false;
    }
    cwd == repo
        || cwd
            .strip_prefix(repo)
            .is_some_and(|rest| rest.starts_with('/') || rest.starts_with("--fd-"))
}

/// The used percent that stands for an instance whose limits are unknown.
pub const UNKNOWN_USED_PERCENT: f64 = 50.0;

/// How tight the provider instance of a session is: the highest
/// `used_percent` of its latest windows, or [`UNKNOWN_USED_PERCENT`] when
/// no session of the same station and instance reported one. A window of a
/// scope comes from the most recently updated session that reported it
/// (the same rule as the board's meters).
#[must_use]
pub fn used_percent_of(sessions: &[DeckSession], of: &DeckSession) -> f64 {
    let same = |s: &&DeckSession| {
        s.station_id == of.station_id && s.summary.provider_instance == of.summary.provider_instance
    };
    let mut latest: BTreeMap<&str, (i64, Option<f64>)> = BTreeMap::new();
    for s in sessions.iter().filter(same) {
        for limit in &s.summary.rate_limits {
            let seen = latest.get(limit.scope.as_str());
            if seen.is_none_or(|(at, _)| *at < s.summary.updated_at) {
                latest.insert(&limit.scope, (s.summary.updated_at, limit.used_percent));
            }
        }
    }
    latest
        .values()
        .filter_map(|(_, used)| *used)
        .reduce(f64::max)
        .unwrap_or(UNKNOWN_USED_PERCENT)
}

/// The best session for the task, or `None`.
#[must_use]
pub fn pick_session<'a>(
    sessions: &'a [DeckSession],
    needs: &Needs<'_>,
    buried: &BTreeMap<SessionId, u32>,
) -> Option<&'a DeckSession> {
    sessions
        .iter()
        .filter_map(|s| {
            let rank = status_rank(s.status)?;
            if let Some(p) = needs.provider
                && s.summary.provider != Some(p)
            {
                return None;
            }
            if let Some(repo) = needs.repo {
                let cwd = s.summary.cwd.as_deref()?;
                if !in_repo(cwd, repo) {
                    return None;
                }
            }
            let mail = buried.get(&s.summary.session_id).copied().unwrap_or(0);
            Some(((rank, mail), s))
        })
        .min_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| {
                    used_percent_of(sessions, a.1).total_cmp(&used_percent_of(sessions, b.1))
                })
                .then_with(|| Reverse(a.1.summary.updated_at).cmp(&Reverse(b.1.summary.updated_at)))
                .then_with(|| a.1.summary.session_id.cmp(&b.1.summary.session_id))
        })
        .map(|(_, s)| s)
}

/// The stations that can start a new session for the task, best first:
/// the link is up, the station knows its capacity and is under
/// `max_sessions`, and a provider instance of the needed kind (or of any
/// kind) is signed in. Most free places first, then by name. The caller
/// still checks that the repository exists there.
#[must_use]
pub fn spawn_candidates<'a>(
    stations: &'a [StationView],
    needs: &Needs<'_>,
) -> Vec<&'a StationView> {
    let mut out: Vec<(u64, &StationView)> = stations
        .iter()
        .filter(|st| st.reachable)
        .filter_map(|st| {
            let info = st.info.as_ref()?;
            let free = match info.max_sessions {
                Some(max) => u64::from(max).checked_sub(info.live_sessions)?,
                None => u64::MAX,
            };
            if free == 0 {
                return None;
            }
            let has_provider = info.providers.iter().any(|p| {
                p.auth == "signed_in" && needs.provider.is_none_or(|kind| p.provider == kind)
            });
            has_provider.then_some((free, st))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    out.into_iter().map(|(_, st)| st).collect()
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
