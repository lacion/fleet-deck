//! The deck relay of desktop actions (R13 §6D items 6 to 10): the interface
//! that the owner routes ([`crate::http::desktop_routes`]) and the wake
//! ([`crate::wake`]) share.
//!
//! The relay (R13 P3b):
//! - [`act`] sends one non-wall action with the dispatch authority of
//!   `link/desktop_authority.rs`. The wake uses it ([`Actor::Wake`]).
//! - [`act_body`] is the owner path. It also runs the three wall actions with
//!   the binding steps of §6D items 1 and 2: `display_begin_open`, the send,
//!   then `display_commit_binding` (open, reload) or `display_clear_binding_at`
//!   (close). A wall action holds a per-display lock (a second gets
//!   [`RelayError::Conflict`]).
//! - [`info`] reads the desktop info of a station.
//!
//! Every call checks `desktop_control` live first and again right before the
//! authority is made. A timeout means the outcome is unknown: the relay never
//! retries. After a successful answer the relay updates only the cached
//! `desktop` field of the station info. Every attempt of [`act`] and
//! [`act_body`] writes one audit row (§6D item 10); [`info`] writes none.
//!
//! The messages of [`RelayError::message`] are fixed and redacted: they hold
//! no URL, no notification text and no request content, and they never claim
//! that `forbidden` means `allow_desktop = false` (§6D item 8).

use std::sync::Arc;

use axum::http::StatusCode;
use fleetdeck_link::desktop_authority::target_monitor;
use fleetdeck_proto::StationId;
use fleetdeck_proto::desktop::{DesktopActBody, DesktopActResult, DesktopAction, DesktopInfo};
use fleetdeck_proto::version::feature;

use crate::link::DeckLink;
use crate::link::desktop_authority::{Waits, resolve, send_act, send_info};
use crate::live::LiveCell;
use crate::store::displays_binding::{BindError, check_monitor};
use crate::store::{AuditResult, DeckError, DeckStore, LinkFeature, NewAudit};

#[path = "desktop_relay_wall.rs"]
mod wall;
use wall::Wall;

/// Who asks for an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// The owner, from the board (audit action `desktop.<action>`, actor
    /// `owner`).
    Owner,
    /// The wake rule of a bound display (audit action `desktop.wake`, actor
    /// `wake`).
    Wake { display_id: String },
}

/// What the relay uses.
#[derive(Debug, Clone)]
pub struct RelayCtx {
    pub store: DeckStore,
    pub link: Arc<DeckLink>,
    /// The live settings: `desktop_control` is read here at each decision.
    pub live: LiveCell,
}

/// Why the relay refused or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayError {
    /// The station has no desktop control, or did not negotiate
    /// `desktop.v1` and `desktop_authority.v1`.
    Unsupported,
    /// The station answered `forbidden`. The cause is ambiguous: the link
    /// scope refusal uses the same code.
    Forbidden,
    /// Invalid parameters.
    Invalid,
    /// A missing target.
    NotFound,
    /// A conflict (a bound display, a superseded launch).
    Conflict,
    /// A failed or malformed answer.
    Failed,
    /// An overload.
    Overloaded,
    /// A timeout. The action may have run on the station.
    Timeout,
    /// No live link to the station.
    Unreachable,
    /// The deck setting `desktop_control` is off.
    ControlOff,
}

impl RelayError {
    /// The HTTP status and error code (§6D item 8).
    #[must_use]
    pub fn http(self) -> (StatusCode, &'static str) {
        match self {
            Self::Unsupported => (StatusCode::CONFLICT, "desktop_unsupported"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "desktop_forbidden"),
            Self::Invalid => (StatusCode::BAD_REQUEST, "invalid_desktop_action"),
            Self::NotFound => (StatusCode::NOT_FOUND, "desktop_not_found"),
            Self::Conflict => (StatusCode::CONFLICT, "desktop_conflict"),
            Self::Failed => (StatusCode::BAD_GATEWAY, "desktop_failed"),
            Self::Overloaded => (StatusCode::SERVICE_UNAVAILABLE, "desktop_overloaded"),
            Self::Timeout => (StatusCode::GATEWAY_TIMEOUT, "desktop_timeout"),
            Self::Unreachable => (StatusCode::CONFLICT, "station_unreachable"),
            Self::ControlOff => (StatusCode::CONFLICT, "desktop_control_off"),
        }
    }

    /// The fixed, redacted text.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Unsupported => "This station's build has no desktop control.",
            Self::Forbidden => "The station refused the desktop action.",
            Self::Invalid => "The desktop action is not valid.",
            Self::NotFound => "The target of the desktop action was not found.",
            Self::Conflict => "The desktop action conflicts with the current state.",
            Self::Failed => "The desktop action failed.",
            Self::Overloaded => "The station is busy; try again later.",
            Self::Timeout => "The station did not answer in time; the action may have run.",
            Self::Unreachable => "The station is not connected.",
            Self::ControlOff => "Desktop control is off on this deck.",
        }
    }
}

/// The `desktop_control` setting, read live.
fn control_on(ctx: &RelayCtx) -> Result<(), RelayError> {
    if ctx.live.get().desktop_control {
        Ok(())
    } else {
        Err(RelayError::ControlOff)
    }
}

/// A display id as the station grammar allows it: `^[A-Za-z0-9_-]{1,64}$`.
#[must_use]
pub fn valid_display_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// True for an origin whose host is `localhost`, a loopback address or an
/// unspecified address: a wall on another computer cannot reach it.
#[must_use]
pub fn is_loopback_origin(origin: &str) -> bool {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(v6),
        None => authority.split(':').next().unwrap_or(authority),
    };
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.to_canonical().is_loopback() || ip.is_unspecified())
}

/// The pair page that a wall launch opens (§5E item 4, §5F item 3). The id is
/// a deck id and the revision a number: neither needs escaping.
fn wall_url(origin: &str, display_id: &str, revision: i64) -> String {
    format!(
        "{}/display#/display-pair?expect={display_id}&op={revision}",
        origin.trim_end_matches('/')
    )
}

/// The serde tag of an action, for the audit row.
fn tag_of<T: serde::Serialize>(action: &T) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|v| v.get("action").and_then(|t| t.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn is_wall(action: &DesktopAction) -> bool {
    matches!(
        action,
        DesktopAction::WallOpen { .. }
            | DesktopAction::WallClose { .. }
            | DesktopAction::WallReload { .. }
    )
}

fn audit_result(result: &Result<DesktopActResult, RelayError>) -> AuditResult {
    match result {
        Ok(_) => AuditResult::Ok,
        Err(
            RelayError::ControlOff
            | RelayError::Unsupported
            | RelayError::Forbidden
            | RelayError::Invalid
            | RelayError::NotFound
            | RelayError::Conflict,
        ) => AuditResult::Refused,
        Err(
            RelayError::Failed
            | RelayError::Overloaded
            | RelayError::Timeout
            | RelayError::Unreachable,
        ) => AuditResult::Failed,
    }
}

/// One audit row for one attempt (§6D item 10). The detail holds codes and
/// deck identifiers only: never a URL, a text or a session id.
async fn audit(
    ctx: &RelayCtx,
    station: &StationId,
    name: &str,
    actor: &Actor,
    monitor: Option<&str>,
    result: &Result<DesktopActResult, RelayError>,
) {
    let code = result.as_ref().err().map(|e| e.http().1);
    let status = audit_result(result);
    let row = match actor {
        Actor::Owner => {
            let row =
                NewAudit::new("owner", format!("desktop.{name}"), status).target(station.as_str());
            match code {
                Some(c) => row.detail(c),
                None => row,
            }
        }
        Actor::Wake { display_id } => {
            let display = if valid_display_id(display_id) {
                display_id.as_str()
            } else {
                "-"
            };
            let monitor = monitor.filter(|m| check_monitor(m).is_ok()).unwrap_or("-");
            NewAudit::new("wake", "desktop.wake", status)
                .target(station.as_str())
                .detail(format!(
                    "display={display} monitor={monitor} code={}",
                    code.unwrap_or("ok")
                ))
        }
    };
    ctx.store.audit_or_log(row).await;
}

fn store_error(e: &DeckError) -> RelayError {
    tracing::warn!(error = %e, "desktop relay store");
    RelayError::Failed
}

fn bind_error(e: BindError) -> RelayError {
    match e {
        BindError::Unknown => RelayError::NotFound,
        BindError::Bound | BindError::Superseded => RelayError::Conflict,
        BindError::Invalid(_) => RelayError::Invalid,
        BindError::Store(e) => store_error(&e),
    }
}

/// The checks that only the wake needs: it may send `screen.power on` for the
/// exact bound monitor of its display (§6.6, §6D item 4). Returns the
/// `quiet_hours` of the display for the authority.
async fn wake_check(
    ctx: &RelayCtx,
    station: &StationId,
    action: &DesktopAction,
    display_id: &str,
) -> Result<Option<String>, RelayError> {
    let DesktopAction::ScreenPower {
        monitor: Some(monitor),
        on: true,
    } = action
    else {
        return Err(RelayError::Invalid);
    };
    let display = ctx
        .store
        .display_get(display_id.to_owned())
        .await
        .map_err(|e| store_error(&e))?
        .ok_or(RelayError::NotFound)?;
    let bound = display
        .binding
        .as_ref()
        .is_some_and(|b| &b.station_id == station && b.monitor == *monitor);
    if bound {
        Ok(display.quiet_hours)
    } else {
        Err(RelayError::Conflict)
    }
}

/// One send, with no audit row: the link, the live switch, the authority, the
/// answer, and the cache update.
async fn run_act(
    ctx: &RelayCtx,
    station: &StationId,
    action: &DesktopAction,
    quiet_hours: Option<String>,
) -> Result<DesktopActResult, RelayError> {
    let route = resolve(&ctx.link, &ctx.store, station)?;
    // The deck stops issuing authorities at once when the switch turns off.
    control_on(ctx)?;
    let mut result = send_act(
        &ctx.store,
        &route,
        action,
        quiet_hours,
        Waits::default(),
        None,
    )
    .await?;
    crate::desktop_capture::stamp_features(&ctx.store, station, &mut result.info);
    ctx.store
        .update_station_desktop(station, result.info.clone());
    Ok(result)
}

/// Runs one non-wall action on `station`. The owner path of the board is
/// [`act_body`]; a wall action here is `Invalid`.
///
/// # Errors
/// [`RelayError`]. A [`RelayError::Timeout`] means that the action may have
/// run.
pub async fn act(
    ctx: &RelayCtx,
    station: StationId,
    action: DesktopAction,
    actor: Actor,
) -> Result<DesktopActResult, RelayError> {
    let name = tag_of(&action);
    let monitor = target_monitor(&action).map(str::to_owned);
    let result = async {
        control_on(ctx)?;
        if is_wall(&action) {
            return Err(RelayError::Invalid);
        }
        let quiet_hours = match &actor {
            Actor::Owner => None,
            Actor::Wake { display_id } => wake_check(ctx, &station, &action, display_id).await?,
        };
        run_act(ctx, &station, &action, quiet_hours).await
    }
    .await;
    audit(ctx, &station, &name, &actor, monitor.as_deref(), &result).await;
    result
}

/// Runs one action of the owner's HTTP body, with the binding steps of the
/// wall actions. `origin` is the board address for the pair page of a wall
/// open or reload.
///
/// # Errors
/// [`RelayError`]. A [`RelayError::Timeout`] means that the action may have
/// run; the binding is then unchanged.
pub async fn act_body(
    ctx: &RelayCtx,
    station: StationId,
    body: DesktopActBody,
    origin: Option<&str>,
) -> Result<DesktopActResult, RelayError> {
    let name = tag_of(&body);
    let result = run_body(ctx, &station, body, origin).await;
    audit(ctx, &station, &name, &Actor::Owner, None, &result).await;
    result
}

async fn run_body(
    ctx: &RelayCtx,
    station: &StationId,
    body: DesktopActBody,
    origin: Option<&str>,
) -> Result<DesktopActResult, RelayError> {
    control_on(ctx)?;
    match body {
        DesktopActBody::WallOpen {
            display_id,
            monitor,
            workspace,
        } => {
            let wall = Wall::start(ctx, station, &display_id, origin)?;
            wall.open(&monitor, workspace).await
        }
        DesktopActBody::WallReload { display_id } => {
            Wall::start(ctx, station, &display_id, origin)?
                .reload()
                .await
        }
        DesktopActBody::WallClose { display_id } => {
            Wall::start(ctx, station, &display_id, None)?.close().await
        }
        other => {
            let action = plain_action(ctx, station, other).await?;
            run_act(ctx, station, &action, None).await
        }
    }
}

fn monitor_ok(monitor: &str) -> Result<(), RelayError> {
    check_monitor(monitor).map_err(|_| RelayError::Invalid)
}

/// The action of a body that is not a wall action, with the checks of the
/// deck: monitor names, and a `browser_open` session of the target station.
async fn plain_action(
    ctx: &RelayCtx,
    station: &StationId,
    body: DesktopActBody,
) -> Result<DesktopAction, RelayError> {
    Ok(match body {
        DesktopActBody::ScreenPower { monitor, on } => {
            if let Some(m) = &monitor {
                monitor_ok(m)?;
            }
            DesktopAction::ScreenPower { monitor, on }
        }
        DesktopActBody::ScreenLock => DesktopAction::ScreenLock,
        DesktopActBody::ScreenDnd { on } => DesktopAction::ScreenDnd { on },
        DesktopActBody::ScreenIdleInhibit { on } => DesktopAction::ScreenIdleInhibit { on },
        DesktopActBody::WorkspaceShow { monitor, workspace } => {
            monitor_ok(&monitor)?;
            DesktopAction::WorkspaceShow { monitor, workspace }
        }
        DesktopActBody::Notify {
            title,
            body,
            urgency,
        } => DesktopAction::Notify {
            title,
            body,
            urgency,
        },
        DesktopActBody::BrowserOpen {
            url,
            monitor,
            session,
        } => {
            if let Some(m) = &monitor {
                monitor_ok(m)?;
            }
            if let Some(id) = &session {
                let known = ctx
                    .store
                    .session_summary(station.clone(), id.clone())
                    .await
                    .map_err(|e| store_error(&e))?;
                if known.is_none() {
                    return Err(RelayError::Invalid);
                }
            }
            DesktopAction::BrowserOpen {
                url,
                monitor,
                session,
            }
        }
        DesktopActBody::WindowClose { address } => DesktopAction::WindowClose { address },
        // R13 P6 follow-up: the owner closes a preview that an agent opened.
        // It needs `desktop_mcp.v1` on every live link of the station.
        DesktopActBody::PreviewClose { window_id } => {
            match ctx.store.link_feature(station, feature::DESKTOP_MCP) {
                LinkFeature::Down => return Err(RelayError::Unreachable),
                LinkFeature::Without => return Err(RelayError::Unsupported),
                LinkFeature::Negotiated => {}
            }
            DesktopAction::PreviewClose { window_id }
        }
        // R13 P5b task F: the owner recording has its own start (task K1).
        DesktopActBody::ScreenRecord { .. } => return Err(RelayError::Unsupported),
        DesktopActBody::ScreenCapture { .. }
        | DesktopActBody::WallOpen { .. }
        | DesktopActBody::WallClose { .. }
        | DesktopActBody::WallReload { .. } => return Err(RelayError::Invalid),
    })
}

/// Reads the desktop info of `station`; `fresh` bypasses the station cache
/// of 10 seconds (the wake rule asks for it). It needs `desktop_control` and
/// writes no audit row.
///
/// # Errors
/// [`RelayError`].
pub async fn info(
    ctx: &RelayCtx,
    station: StationId,
    fresh: bool,
) -> Result<DesktopInfo, RelayError> {
    control_on(ctx)?;
    let route = resolve(&ctx.link, &ctx.store, &station)?;
    control_on(ctx)?;
    let mut info = send_info(&route, fresh, Waits::default()).await?;
    crate::desktop_capture::stamp_features(&ctx.store, &station, &mut info);
    ctx.store.update_station_desktop(&station, info.clone());
    Ok(info)
}

#[cfg(test)]
#[path = "desktop_relay_tests.rs"]
mod tests;
