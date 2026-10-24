//! What a wall display sees of the board (R13 §5, §5A, §5B, DL-201): a
//! reduced card and a reduced station, and only inside the effective scope
//! of the display (its ceiling narrowed by its directive, R13 §5D item 17).
//! Every route and the WebSocket use these functions for a
//! `Principal::Display`; nothing else leaves the deck for a display.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::http::StatusCode;
use fleetdeck_proto::board::{DeckSession, StationView};
use fleetdeck_proto::display_event::{DisplayDisk, DisplayResources, DisplaySessionLoad};
use fleetdeck_proto::event::RequestKind;
use fleetdeck_proto::resources::StationResources;
use fleetdeck_proto::session::{OpenRequest, RepoInfo, SessionSummary};
use fleetdeck_proto::{SessionId, StationId};

use super::Board;
use super::guard::ApiError;
use crate::coordinator_desktop::effective_scope;
use crate::store::DisplayLogin;
use crate::store::display_directives::EffectiveScope;

/// The last component of a path on a station (any separator).
pub(crate) fn last_component(root: &str) -> String {
    root.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The title of an open request on a display: a fixed label of its kind. The
/// provider's own title holds the command, the file change reason or the
/// question text, and a display never gets that (R13 §5B).
pub(crate) fn request_label(kind: RequestKind) -> &'static str {
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

/// The card of a display, built from an explicit allowlist (R13 §5D item
/// 11): provider and instance, callsign, title, state and activity, the run
/// and turn ids, usage and rate limits, the background agent count, the running
/// agent count, the focus line, the update time, the repository basename, the batch
/// reference when it is valid, and the open requests as fixed labels. It has
/// no digest, cwd, branch, worktree, tools, errors, models, base, review
/// revision, unread count or project.
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
    // A count (R13 §5D): the agent count of the card. Not the workflow task
    // ids, not the background workflows, not the partial flag.
    summary.running_agents = s.running_agents;
    summary.focus.clone_from(&s.focus);
    summary.batch = s.batch.clone().filter(|b| b.problem().is_none());
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

/// True when the scope allows a session of `station` with the repository
/// root `root`: the station is in the scope, and a repository filter equals
/// the displayed basename. A session without a repository fails a
/// repository filter.
#[must_use]
pub fn in_scope(scope: &EffectiveScope, station: &StationId, root: Option<&str>) -> bool {
    scope.stations.contains(station)
        && scope
            .repo
            .as_ref()
            .is_none_or(|r| root.is_some_and(|root| last_component(root) == *r))
}

/// The reduced resources of a station: the host numbers and the load of the
/// visible sessions only (R13 §5D item 10). No disk path or label, no
/// process count.
fn display_resources(r: &StationResources, visible: &BTreeSet<&SessionId>) -> DisplayResources {
    DisplayResources {
        at: r.at,
        disks: r
            .disks
            .iter()
            .map(|d| DisplayDisk {
                total_bytes: d.total_bytes,
                free_bytes: d.free_bytes,
            })
            .collect(),
        memory: r.memory.clone(),
        cpu: r.cpu.clone(),
        sessions: r
            .sessions
            .iter()
            .filter(|l| visible.contains(&l.session_id))
            .map(|l| DisplaySessionLoad {
                session_id: l.session_id.clone(),
                cpu_pct: l.cpu_pct,
                rss_bytes: l.rss_bytes,
            })
            .collect(),
    }
}

/// The station of a display: no key, no dial address, no info, and the
/// reduced resources instead of the info.
#[must_use]
pub fn display_station(view: &StationView, visible: &BTreeSet<&SessionId>) -> StationView {
    let mut v = view.clone();
    v.display_resources = view
        .info
        .as_ref()
        .and_then(|i| i.resources.as_ref())
        .map(|r| display_resources(r, visible));
    v.key = String::new();
    v.dial_url = None;
    v.info = None;
    v.info_at = None;
    v
}

/// The cards inside `scope`, reduced.
#[must_use]
pub fn display_cards(cards: &[DeckSession], scope: &EffectiveScope) -> Vec<DeckSession> {
    cards
        .iter()
        .filter(|c| {
            in_scope(
                scope,
                &c.station_id,
                c.summary.repo.as_ref().map(|r| r.root.as_str()),
            )
        })
        .map(display_card)
        .collect()
}

/// The stations inside `scope`, reduced. `cards` are the cards that the
/// display may see (the visible sessions of each station).
#[must_use]
pub fn display_stations(
    views: &[StationView],
    scope: &EffectiveScope,
    cards: &[DeckSession],
) -> Vec<StationView> {
    views
        .iter()
        .filter(|v| scope.stations.contains(&v.station_id))
        .map(|v| {
            let visible: BTreeSet<&SessionId> = cards
                .iter()
                .filter(|c| c.station_id == v.station_id)
                .map(|c| &c.summary.session_id)
                .collect();
            display_station(v, &visible)
        })
        .collect()
}

/// The effective scope of a display now. The login session is checked first
/// (not expired, not idle, not logged out, the same `auth_generation`): a
/// route and every chunk of a stream call this, so a session that ended
/// stops the next answer. A display that is revoked after its login was read
/// gets `401 login_required`.
///
/// # Errors
/// 401 `login_required`, or `internal` for a database error.
pub async fn scope_of(board: &Arc<Board>, d: &DisplayLogin) -> Result<EffectiveScope, ApiError> {
    let ended = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "login_required",
            "the display login ended",
        )
    };
    if !board
        .store
        .display_session_alive(d.session_hash.clone())
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ended());
    }
    effective_scope(&board.link, &d.display_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(ended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleetdeck_proto::resources::{CpuLoad, DiskSpace, MemoryUse, SessionLoad};
    use fleetdeck_proto::session::BatchRef;

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

    fn scope(stations: &[&str], repo: Option<&str>) -> EffectiveScope {
        EffectiveScope {
            stations: stations.iter().map(|s| StationId::new(*s)).collect(),
            repo: repo.map(str::to_owned),
        }
    }

    #[test]
    fn the_scope_checks_the_station_and_the_repository_basename() {
        let a = StationId::new("a");
        let b = StationId::new("b");
        let open = scope(&["a"], None);
        assert!(in_scope(&open, &a, None));
        assert!(!in_scope(&open, &b, None));
        let narrow = scope(&["a"], Some("repo"));
        assert!(in_scope(&narrow, &a, Some("/srv/x/repo")));
        assert!(!in_scope(&narrow, &a, Some("/srv/x/other")));
        // A session without a repository fails a repository filter.
        assert!(!in_scope(&narrow, &a, None));
    }

    #[test]
    fn the_resources_hold_no_path_label_or_process_count_and_only_visible_sessions() {
        let r = StationResources {
            at: 7,
            disks: vec![DiskSpace {
                label: "SENTINEL-LABEL".to_owned(),
                path: "/SENTINEL/path".to_owned(),
                total_bytes: 10,
                free_bytes: 4,
            }],
            memory: Some(MemoryUse {
                total_bytes: 8,
                available_bytes: 3,
            }),
            cpu: Some(CpuLoad {
                cores: 4,
                load1_x100: 1,
                load5_x100: 2,
            }),
            sessions: vec![
                SessionLoad {
                    session_id: SessionId::new("seen"),
                    cpu_pct: Some(5),
                    rss_bytes: Some(6),
                    procs: 9,
                },
                SessionLoad {
                    session_id: SessionId::new("hidden"),
                    cpu_pct: Some(50),
                    rss_bytes: None,
                    procs: 1,
                },
            ],
        };
        let seen = SessionId::new("seen");
        let visible = BTreeSet::from([&seen]);
        let text = serde_json::to_string(&display_resources(&r, &visible)).unwrap();
        assert!(!text.contains("SENTINEL"), "{text}");
        assert!(!text.contains("procs"), "{text}");
        assert!(!text.contains("hidden"), "{text}");
        assert!(text.contains("\"total_bytes\":10"), "{text}");
        assert!(text.contains("\"seen\""), "{text}");
    }

    #[test]
    fn the_card_keeps_a_valid_batch_and_drops_a_broken_one() {
        let mut card = DeckSession {
            station_id: StationId::new("a"),
            summary: SessionSummary::new(SessionId::new("s")),
            status: fleetdeck_proto::session::CardStatus::Idle,
            unread: Some(3),
            project_id: Some("p".to_owned()),
            review_rev: Some(9),
        };
        card.summary.batch = Some(BatchRef {
            batch_id: "b1".to_owned(),
            index: 2,
            size: 4,
        });
        card.summary.base = Some(fleetdeck_proto::session::SessionBase {
            git_ref: Some("SENTINEL".to_owned()),
            oid: "abc".to_owned(),
            inferred: false,
        });
        let shown = display_card(&card);
        assert_eq!(shown.summary.batch, card.summary.batch);
        assert!(shown.summary.base.is_none());
        assert!(shown.review_rev.is_none() && shown.unread.is_none());
        card.summary.batch = Some(BatchRef {
            batch_id: "b/1".to_owned(),
            index: 2,
            size: 4,
        });
        assert!(display_card(&card).summary.batch.is_none());
    }

    #[test]
    fn the_display_card_copies_running_agents_only() {
        let mut card = DeckSession {
            station_id: StationId::new("a"),
            summary: SessionSummary::new(SessionId::new("s")),
            status: fleetdeck_proto::session::CardStatus::Idle,
            unread: None,
            project_id: None,
            review_rev: None,
        };
        card.summary.running_agents = 7;
        card.summary.agents_partial = true;
        card.summary.workflows_overflow = true;
        card.summary.background_workflows = 2;
        card.summary.running_workflows = vec![fleetdeck_proto::session::RunningWorkflow {
            task_id: "wf-secret".to_owned(),
            running_agents: 7,
            agents_total: 9,
            untracked: false,
        }];
        let shown = display_card(&card).summary;
        assert_eq!(shown.running_agents, 7);
        assert!(shown.running_workflows.is_empty());
        assert_eq!(shown.background_workflows, 0);
        assert!(!shown.agents_partial);
        assert!(!shown.workflows_overflow);
    }
}
