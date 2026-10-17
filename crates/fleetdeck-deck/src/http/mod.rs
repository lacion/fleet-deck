//! The board HTTP API (M2 P3-8; PLAN §9). Every route goes through the
//! [`guard`] with the scope of the [`methods::api`] table. The listener
//! accepts only the deck's board addresses (D13; checked in `DeckConfig`).
//! The deck serves HTTPS itself ([`tls`]; D13: no reverse proxy, DL-054).

pub mod access;
pub mod account_routes;
pub mod account_secrets;
pub mod activity_routes;
pub mod admission;
pub mod archive_routes;
pub mod artifact_routes;
pub mod assets;
pub mod assign_routes;
pub mod assist_routes;
pub mod auth_routes;
pub mod body_deadline;
pub mod command_routes;
pub mod desktop_routes;
pub mod display_artifact;
pub mod display_bodies;
pub mod display_changes;
pub mod display_directive_routes;
pub mod display_document;
pub mod display_routes;
pub mod display_view;
pub mod endpoints;
pub mod execution_routes;
pub mod file_routes;
pub mod fleet_answers_routes;
pub mod fleet_routes;
pub mod grant_routes;
pub mod guard;
pub mod history_routes;
pub mod methods;
pub mod native_routes;
pub mod not_built;
pub mod pairing_routes;
pub mod paste_routes;
pub mod people_routes;
pub mod plan_routes;
pub mod project_session_routes;
pub mod project_writes;
pub mod projects_routes;
pub mod read_routes;
pub mod review_anchor_cache;
pub mod review_routes;
pub mod schedule_routes;
pub mod secrets_routes;
pub mod server;
pub mod session_work_routes;
pub mod settings_routes;
pub mod share_routes;
pub mod shell_routes;
pub mod station_routes;
pub mod tailnet_routes;
pub mod tls;
pub mod tls_tailscale;
#[cfg(test)]
pub(crate) mod tls_test_pki;
pub mod workspace_routes;
pub mod worktree_routes;
pub mod ws;

use std::sync::Arc;

use axum::Router;
use axum::extract::Extension;
use axum::middleware;
use axum::routing::{delete, get, patch, post};

pub use admission::LoginLimits;
pub use auth_routes::{DeviceLinkLimiter, SessionView};
pub use guard::{ApiError, Origins};
pub use server::{BindError, BoardServer};
pub use tls::BoardTls;

use crate::link::DeckLink;
use crate::store::DeckStore;

/// The shared state of the board API.
#[derive(Debug)]
pub struct Board {
    pub store: DeckStore,
    /// The command router (P3-9a).
    pub link: Arc<DeckLink>,
    pub origins: Origins,
    pub limiter: LoginLimits,
    /// The rate limit of `POST /api/auth/device-link` (DL-146).
    pub device_links: DeviceLinkLimiter,
    /// The SHA-256 of the board certificate, for a device link.
    pub certificate_sha256: String,
    /// The board certificate is the deck's self-signed one.
    pub self_signed: bool,
    /// The live endpoints: each board address with the certificate it gets
    /// now (DL-190).
    pub endpoints: Arc<endpoints::Endpoints>,
    /// The state of the Tailscale certificate, for the Tailnet card.
    pub cert: Arc<std::sync::RwLock<crate::cert_task::CertState>>,
    /// Single-use WebSocket tickets of bearer clients.
    pub tickets: ws::Tickets,
    /// How often an open WebSocket checks its login ([`ws::RECHECK`]).
    pub ws_recheck: std::time::Duration,
    /// The longest time that one send to a display socket may take
    /// ([`ws::DISPLAY_SEND`]); a slower send ends the socket (R13 §5A).
    pub ws_display_send: std::time::Duration,
    /// The link node for the pairing methods (`None`: they answer 503).
    pub pairing: Option<Arc<crate::link::DeckNode>>,
    /// The board web client (P4-4).
    pub assets: assets::BoardAssets,
    /// The live settings of the deck, read per use (DL-172, design 1.4).
    pub live: crate::live::LiveCell,
    /// The elevation codes and the elevations of the owner (DL-172).
    pub elevations: Arc<crate::elevation::Elevations>,
    /// The settings file of the deck (`None`: the settings routes answer 503).
    pub settings: Option<settings_routes::DeckSettings>,
    /// Restarts the deck process (`None`: the restart route answers 501).
    pub restart: Option<Arc<dyn fleetdeck_core::restart::Restart>>,
    /// The local station of `fleetdeck up`; set after the pairing.
    pub local_station: Arc<std::sync::OnceLock<fleetdeck_proto::StationId>>,
    /// What the automatic tailnet start did (design 14.4); `serve` sets it
    /// once, the Tailnet card reads it.
    pub tailnet_start: Arc<std::sync::Mutex<crate::tailnet_auto::TailnetStart>>,
    /// The facts about this machine that the tailnet routes read
    /// (`tailscale_wsl::detect` in the deck; a test gives its own).
    pub host: fn() -> fleetdeck_core::tailscale_wsl::HostFacts,
}

/// The board API router.
pub fn router(board: Arc<Board>) -> Router {
    // The open OAuth transactions (DL-173): in memory, one set for this
    // router and its clones.
    let oauth = Arc::new(crate::oauth::Transactions::default());
    let mut r = Router::new();
    for api in methods::command_methods() {
        r = r.route(&methods::route_of(api), post(command_routes::command));
    }
    r.route("/api/auth/login", post(auth_routes::password))
        .route("/api/auth/elevate", post(auth_routes::elevate))
        .route(
            "/api/settings/deck",
            get(settings_routes::deck_get).post(settings_routes::deck_set),
        )
        .route("/api/settings/restart", post(settings_routes::restart))
        .route("/api/tailnet", get(tailnet_routes::get))
        .route("/api/tailnet/use", post(tailnet_routes::use_tailnet))
        .route("/api/tailnet/devices", get(tailnet_routes::devices))
        .route(
            "/api/pairing/window",
            get(pairing_routes::window_get).post(pairing_routes::window_set),
        )
        .route("/api/pairing/requests", get(pairing_routes::requests))
        .route(
            "/api/pairing/requests/{request_id}/approve",
            post(pairing_routes::approve),
        )
        .route(
            "/api/pairing/requests/{request_id}/deny",
            post(pairing_routes::deny),
        )
        .route(
            "/api/stations/{station}/settings",
            get(settings_routes::station_get).post(settings_routes::station_set),
        )
        .route(
            "/api/secrets/stations/{station}/settings",
            post(secrets_routes::station_settings).layer(axum::extract::DefaultBodyLimit::max(
                secrets_routes::MAX_BODY,
            )),
        )
        .route("/api/auth/link", post(auth_routes::link))
        .route("/api/auth/join", post(auth_routes::join))
        .route("/api/auth/device-link", post(auth_routes::device_link))
        .route("/api/auth/logout", post(auth_routes::logout))
        .route("/api/auth/session", get(auth_routes::session))
        .route(
            "/api/stations",
            get(read_routes::stations).post(station_routes::add),
        )
        .route("/api/stations/invite", post(station_routes::invite))
        .route(
            "/api/stations/{station}/remove",
            post(station_routes::remove),
        )
        .route(
            "/api/stations/{station}/workspaces",
            get(workspace_routes::list),
        )
        .route(
            "/api/stations/{station}/workspace/preflight",
            post(workspace_routes::preflight),
        )
        .route(
            "/api/stations/{station}/worktrees",
            get(worktree_routes::list),
        )
        .route(
            "/api/stations/{station}/worktrees/trash",
            post(worktree_routes::trash),
        )
        .route(
            "/api/stations/{station}/worktrees/restore",
            post(worktree_routes::restore),
        )
        .route(
            "/api/stations/{station}/worktrees/purge",
            post(worktree_routes::purge),
        )
        .route(
            "/api/stations/{station}/worktrees/release",
            post(worktree_routes::release),
        )
        .route(
            "/api/stations/{station}/shells",
            get(shell_routes::list).post(shell_routes::command),
        )
        .route(
            "/api/stations/{station}/shells/{shell}/attach",
            get(shell_routes::attach),
        )
        .route(
            "/api/stations/{station}/paste",
            post(paste_routes::paste)
                .layer(axum::extract::DefaultBodyLimit::max(paste_routes::MAX_BODY)),
        )
        .route("/api/stations/{station}/files/list", get(file_routes::list))
        .route("/api/stations/{station}/files/read", get(file_routes::read))
        .route(
            "/api/stations/{station}/files/search",
            get(file_routes::search),
        )
        .route(
            "/api/stations/{station}/sessions/{session}/turns/{turn}/diff",
            get(file_routes::diff),
        )
        .route(
            "/api/stations/{station}/sessions/{session}/diff",
            get(file_routes::session_diff),
        )
        .route(
            "/api/archive",
            get(archive_routes::list).post(archive_routes::change),
        )
        .route("/api/assist/{kind}", post(assist_routes::generate))
        .route(
            "/api/stations/{station}/sessions/{session}/digest",
            post(assist_routes::digest),
        )
        .route("/api/conflicts", get(plan_routes::conflicts))
        .route(
            "/api/schedules",
            get(schedule_routes::list).post(schedule_routes::create),
        )
        .route(
            "/api/schedules/{schedule_id}",
            axum::routing::put(schedule_routes::update).delete(schedule_routes::delete),
        )
        .route(
            "/api/schedules/{schedule_id}/update",
            post(schedule_routes::update),
        )
        .route(
            "/api/schedules/{schedule_id}/delete",
            post(schedule_routes::delete),
        )
        .route(
            "/api/schedules/{schedule_id}/run",
            post(schedule_routes::run),
        )
        .route(
            "/api/schedules/{schedule_id}/runs",
            get(schedule_routes::runs),
        )
        .route("/api/plans", get(plan_routes::list))
        .route("/api/plans/mark", post(plan_routes::mark))
        .route("/api/sessions", get(read_routes::sessions))
        .route(
            "/api/sessions/{station}/{session}/read",
            post(activity_routes::mark_read),
        )
        .route("/api/activity", get(activity_routes::activity))
        .route("/api/fleet/extras", get(read_routes::fleet_extras))
        .route("/api/assign", post(assign_routes::assign))
        .route(
            "/api/mail",
            get(fleet_routes::mails).post(fleet_routes::send_mail),
        )
        .route("/api/mail/group", post(fleet_routes::send_group_mail))
        .route(
            "/api/tasks",
            get(fleet_routes::tasks).post(fleet_routes::create_task),
        )
        .route(
            "/api/tasks/{task_id}/complete",
            post(fleet_routes::complete_task),
        )
        .route("/api/tasks/{task_id}/wait", get(fleet_routes::wait_task))
        .route("/api/tasks/wait", get(fleet_routes::wait_any_task))
        .route("/api/coordinator/grants", get(grant_routes::list))
        .route(
            "/api/coordinator/grants/{grant_id}/revoke",
            post(grant_routes::revoke),
        )
        .route(
            "/api/coordinator/grants/{grant_id}/renew",
            post(grant_routes::renew),
        )
        .route("/api/artifacts", get(artifact_routes::list))
        .route("/api/artifacts/publish", post(artifact_routes::publish))
        .route("/api/artifacts/{station}/{id}", get(artifact_routes::get))
        .route(
            "/api/artifacts/{station}/{id}/shares",
            post(artifact_routes::share),
        )
        .route(
            "/api/artifacts/{station}/{id}/shares/{token}",
            delete(artifact_routes::revoke),
        )
        .route(
            "/api/artifacts/{station}/{id}/view",
            post(artifact_routes::view),
        )
        .route(
            "/api/artifacts/{station}/{id}/{version}/raw/{*path}",
            get(artifact_routes::raw),
        )
        .route(
            "/api/projects",
            get(projects_routes::list).post(projects_routes::create),
        )
        .route(
            "/api/projects/{project}",
            axum::routing::put(projects_routes::update).delete(projects_routes::delete),
        )
        .route(
            "/api/projects/{project}/preflight",
            post(projects_routes::preflight),
        )
        .route(
            "/api/projects/{project}/issues",
            get(projects_routes::issues),
        )
        .route(
            "/api/projects/{project}/issues/{key}",
            get(projects_routes::issue),
        )
        .route(
            "/api/projects/{project}/issues/{key}/comments",
            post(project_writes::issue_comment),
        )
        .route(
            "/api/projects/{project}/issues/{key}/transition",
            post(project_writes::issue_transition),
        )
        .route(
            "/api/projects/{project}/assets",
            get(projects_routes::asset),
        )
        .route(
            "/api/projects/{project}/prs",
            get(projects_routes::prs).post(project_writes::pr_create),
        )
        .route(
            "/api/projects/{project}/prs/{number}",
            get(projects_routes::pr),
        )
        .route(
            "/api/projects/{project}/prs/{number}/checks",
            get(projects_routes::pr_checks),
        )
        .route(
            "/api/projects/{project}/prs/{number}/comments",
            post(project_writes::pr_comment),
        )
        .route(
            "/api/projects/{project}/prs/{number}/reviews",
            post(project_writes::pr_review),
        )
        .route(
            "/api/projects/{project}/sessions/assign",
            post(project_session_routes::assign),
        )
        .route(
            "/api/sessions/{station}/{session}/project/clear",
            post(project_session_routes::clear),
        )
        .route(
            "/api/sessions/{station}/{session}/project/auto",
            post(project_session_routes::auto),
        )
        .route(
            "/api/projects/{project}/sessions",
            post(project_writes::sessions),
        )
        // The accounts of a station (DL-173). The static `forge` segment wins
        // over `{account_id}`.
        .route(
            "/api/stations/{station}/accounts",
            get(account_routes::list),
        )
        .route(
            "/api/stations/{station}/accounts/forge/login",
            post(account_routes::forge_login),
        )
        .route(
            "/api/stations/{station}/accounts/forge/login/{login_id}",
            get(account_routes::forge_login_status),
        )
        .route(
            "/api/stations/{station}/accounts/{account_id}/logout",
            post(account_routes::logout),
        )
        .route(
            "/api/stations/{station}/accounts/{account_id}/containers",
            get(account_routes::containers),
        )
        // The credential relay (DL-173, design section 0): 16 KiB bodies.
        .route(
            "/api/secrets/stations/{station}/accounts/apps",
            post(account_secrets::app_set).layer(axum::extract::DefaultBodyLimit::max(
                account_secrets::MAX_BODY,
            )),
        )
        .route(
            "/api/secrets/stations/{station}/accounts/token",
            post(account_secrets::token_set).layer(axum::extract::DefaultBodyLimit::max(
                account_secrets::MAX_BODY,
            )),
        )
        .route(
            "/api/accounts/oauth/start",
            post(account_routes::oauth_start).layer(Extension(Arc::clone(&oauth))),
        )
        .route(
            "/api/accounts/oauth/finish",
            post(account_routes::oauth_finish).layer(Extension(Arc::clone(&oauth))),
        )
        .route("/api/commands", get(read_routes::commands))
        .route(
            "/api/commands/{command_id}",
            get(command_routes::get_command),
        )
        .route("/api/audit", get(read_routes::audit))
        .route("/api/ws", get(ws::upgrade))
        .route("/api/ws/ticket", post(ws::ticket))
        .route(
            "/api/shares",
            get(share_routes::list_shares).post(share_routes::create_share),
        )
        .route(
            "/api/shares/{share_id}/revoke",
            post(share_routes::revoke_share),
        )
        .route(
            "/api/shares/{share_id}/history",
            post(share_routes::set_history),
        )
        .route(
            "/api/sessions/{station}/{session}/history",
            get(history_routes::history),
        )
        .route(
            "/api/sessions/{station}/{session}/subagents/{tool_use_id}",
            get(session_work_routes::subagent),
        )
        .route(
            "/api/sessions/{station}/{session}/branches",
            get(session_work_routes::branches),
        )
        .route(
            "/api/sessions/{station}/{session}/branches/{head}/diff",
            get(session_work_routes::branch_diff),
        )
        .route(
            "/api/stations/{station}/native-sessions",
            get(native_routes::list),
        )
        .route(
            "/api/stations/{station}/native-sessions/preview",
            post(native_routes::preview),
        )
        .route(
            "/api/spaces",
            get(share_routes::list_spaces).post(share_routes::create_space),
        )
        .route(
            "/api/spaces/{space_id}/sessions",
            post(share_routes::add_space_session),
        )
        .route(
            "/api/sessions/{station}/{session}/chat",
            get(people_routes::chat).post(people_routes::post_chat),
        )
        .route(
            "/api/sessions/{station}/{session}/drive/request",
            post(people_routes::drive_request),
        )
        .route(
            "/api/sessions/{station}/{session}/drive/grant",
            post(people_routes::drive_grant),
        )
        .route(
            "/api/sessions/{station}/{session}/drive/release",
            post(people_routes::drive_release),
        )
        .route(
            "/api/sessions/{station}/{session}/review",
            get(review_routes::list),
        )
        .route(
            "/api/sessions/{station}/{session}/review/comments",
            post(review_routes::add),
        )
        .route(
            "/api/sessions/{station}/{session}/review/comments/{comment}",
            patch(review_routes::edit).delete(review_routes::delete),
        )
        .route(
            "/api/sessions/{station}/{session}/review/send",
            post(review_routes::send),
        )
        .route(
            "/api/sessions/{station}/{session}/review/reviews/{review}/send-again",
            post(review_routes::send_again),
        )
        .route(
            "/api/stations/{station}/sessions/{session}/turn-diff",
            get(review_routes::turn_diff),
        )
        .route(
            "/api/sessions/{station}/{session}/execution",
            get(execution_routes::check),
        )
        .route(
            "/api/sessions/{station}/{session}/fleet-answers",
            get(fleet_answers_routes::list),
        )
        .merge(display_routes::routes())
        .merge(display_directive_routes::routes())
        .merge(display_changes::routes())
        .merge(display_artifact::routes())
        .merge(desktop_routes::routes())
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&board),
            guard::guard,
        ))
        // The public share route is outside the guard: the token is its
        // credential (DL-165).
        // The document of a wall display: the board page with a stricter
        // policy (R13 §5E item 3, §5F item 3). `/display/` redirects.
        .route("/display", get(display_document::document))
        .route("/display/", get(display_document::redirect))
        .route("/a/{token}", get(artifact_routes::public_slash))
        .route("/a/{token}/", get(artifact_routes::public_main))
        .route("/a/{token}/{*path}", get(artifact_routes::public))
        // The OAuth return of Jira and Linear (DL-173, design section 2.3).
        // It is outside the guard on purpose: the provider redirect is a
        // cross-site navigation, so the SameSite=Strict login cookie is not
        // sent and no CSRF header can be added. The one-use `state` of an
        // open transaction is its only credential, and the answer is a 303
        // to a page that checks the cookie (`oauth_finish`).
        .route(
            "/oauth/callback",
            get(crate::oauth::callback).layer(Extension(oauth)),
        )
        .fallback(board_files)
        // A body that is not received in time ends its connection (M1).
        .layer(middleware::from_fn(body_deadline::body_deadline))
        .with_state(board)
}

/// Every path outside the API table: the board web client (P4-4).
async fn board_files(
    axum::extract::State(board): axum::extract::State<Arc<Board>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> axum::response::Response {
    assets::serve(&board.assets, &method, &uri).await
}
