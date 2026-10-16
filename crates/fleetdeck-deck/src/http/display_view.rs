//! What a wall display sees of the board (R13 §5, §5A, §5B, DL-201): a
//! reduced card and a reduced station, and only inside the display's
//! ceiling. Every route and the WebSocket use these functions for a
//! `Principal::Display`; nothing else leaves the deck for a display.

use fleetdeck_proto::StationId;
use fleetdeck_proto::board::{DeckSession, StationView};
use fleetdeck_proto::event::RequestKind;
use fleetdeck_proto::session::{OpenRequest, RepoInfo, SessionSummary};

/// The last component of a path on a station (any separator).
fn last_component(root: &str) -> String {
    root.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The title of an open request on a display: a fixed label of its kind. The
/// provider's own title holds the command, the file change reason or the
/// question text, and a display never gets that (R13 §5B).
fn request_label(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Command => "Approve a command",
        RequestKind::FileChange => "Approve a file change",
        RequestKind::Tool => "Approve a tool use",
        RequestKind::Question => "Answer a question",
    }
}

fn display_request(r: &OpenRequest) -> OpenRequest {
    OpenRequest {
        request_id: r.request_id.clone(),
        node_id: None,
        request_kind: r.request_kind,
        title: request_label(r.request_kind).to_owned(),
        details: String::new(),
        input: None,
        options: Vec::new(),
        // The answer channel is a fixed enum (no text); the card keeps it.
        response: r.response,
        requested_at: r.requested_at,
    }
}

/// The card of a display: no digest, no cwd, no branch, no worktree, no
/// tools, no errors, no models, no unread count and no project.
#[must_use]
pub fn display_card(card: &DeckSession) -> DeckSession {
    let s = &card.summary;
    let mut summary = SessionSummary::new(s.session_id.clone());
    summary.provider = s.provider;
    summary.provider_instance.clone_from(&s.provider_instance);
    summary.callsign.clone_from(&s.callsign);
    summary.title.clone_from(&s.title);
    summary.title_generated = s.title_generated;
    summary.state = s.state;
    summary.activity = s.activity;
    summary.current_run.clone_from(&s.current_run);
    summary.current_turn.clone_from(&s.current_turn);
    summary.usage = s.usage.clone();
    summary.rate_limits.clone_from(&s.rate_limits);
    summary.background_agents = s.background_agents;
    summary.focus.clone_from(&s.focus);
    summary.updated_at = s.updated_at;
    summary.repo = s.repo.as_ref().map(|r| RepoInfo {
        root: last_component(&r.root),
        origin: None,
    });
    summary.open_requests = s.open_requests.iter().map(display_request).collect();
    DeckSession {
        station_id: card.station_id.clone(),
        summary,
        status: card.status,
        unread: None,
        project_id: None,
        review_rev: None,
    }
}

/// The station of a display: no key, no dial address, no info.
#[must_use]
pub fn display_station(view: &StationView) -> StationView {
    let mut v = view.clone();
    v.key = String::new();
    v.dial_url = None;
    v.info = None;
    v.info_at = None;
    v
}

/// The cards of the stations in `ceiling`, reduced.
#[must_use]
pub fn display_cards(cards: &[DeckSession], ceiling: &[StationId]) -> Vec<DeckSession> {
    cards
        .iter()
        .filter(|c| ceiling.contains(&c.station_id))
        .map(display_card)
        .collect()
}

/// The stations in `ceiling`, reduced.
#[must_use]
pub fn display_stations(views: &[StationView], ceiling: &[StationId]) -> Vec<StationView> {
    views
        .iter()
        .filter(|v| ceiling.contains(&v.station_id))
        .map(display_station)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_title_is_a_fixed_label_of_its_kind() {
        let kinds = [
            (RequestKind::Command, "Approve a command"),
            (RequestKind::FileChange, "Approve a file change"),
            (RequestKind::Tool, "Approve a tool use"),
            (RequestKind::Question, "Answer a question"),
        ];
        for (kind, label) in kinds {
            assert_eq!(request_label(kind), label);
        }
    }

    #[test]
    fn a_repo_root_shows_its_last_component_only() {
        assert_eq!(last_component("/home/me/work/repo"), "repo");
        assert_eq!(last_component("/home/me/work/repo/"), "repo");
        assert_eq!(last_component("C:\\work\\repo"), "repo");
        assert_eq!(last_component("/"), "");
    }
}
