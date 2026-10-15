//! The tailnet routes (DL-178, tailnet design sections 2 to 4). Every route
//! is owner only (`Principal::Owner`, also with the `admin` scope).
//!
//! - `GET /api/tailnet`: what Tailscale tells about this machine, the
//!   listen lists that "Use tailnet" would save (`managed`) and the saved
//!   ones (`current`). It runs `tailscale status` at every call.
//! - `POST /api/tailnet/use`: saves those lists and `tailnet_hostname`
//!   through the normal settings writer of the deck. A danger change needs
//!   the owner's elevation, as for any other settings save. The listeners
//!   start at the next restart.
//! - `GET /api/tailnet/devices`: the peers of the tailnet with a pairing
//!   state from the deck's own store. No probe. `tailscale status` is cached
//!   for 5 s by the program path.
//!
//! Tailscale is a display source here. Nothing in an answer of Tailscale
//! authorizes anything.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use fleetdeck_core::netscope::{self, AddrClass};
use fleetdeck_core::settings::SaveError;
use fleetdeck_core::tailscale::{self, Node, Status, TailscaleCli, TailscaleError};
use fleetdeck_core::tailscale_wsl::HostFacts;
use fleetdeck_proto::settings::SettingsChange;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::Board;
use super::guard::ApiError;
use super::settings_routes::{audit, deck_view, is_elevated, owner, save_unreconciled};
use crate::cert_task::CertState;
use crate::store::Principal;
use crate::store::auth::LoginSession;
use crate::tailnet_auto::{self, TailnetStart, WSL_NAT};

/// How long an answer of `tailscale status` stays for the devices route.
const CACHE_TTL: Duration = Duration::from_secs(5);

type Cached = (Instant, Result<Status, TailscaleError>);

/// The last answer of `tailscale status`, by program path. A new
/// `tailscale_bin` is another key and replaces the old answer, so a changed
/// setting never reads an old answer.
static CACHE: Mutex<BTreeMap<PathBuf, Cached>> = Mutex::new(BTreeMap::new());

/// The answer of `cli`, from the cache when it is younger than 5 s.
async fn status_cached(cli: &TailscaleCli) -> Result<Status, TailscaleError> {
    {
        let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, answer)) = cache.get(cli.program())
            && at.elapsed() < CACHE_TTL
        {
            return answer.clone();
        }
    }
    status_fresh(cli).await
}

/// Runs `tailscale status` and keeps the answer.
async fn status_fresh(cli: &TailscaleCli) -> Result<Status, TailscaleError> {
    let answer = cli.status().await;
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    // One program at a time: a new `tailscale_bin` drops the old answer.
    cache.clear();
    cache.insert(
        cli.program().to_path_buf(),
        (Instant::now(), answer.clone()),
    );
    answer
}

/// The saved values of `deck.toml` that the routes use.
pub(super) struct Saved {
    listen: Vec<SocketAddr>,
    link_listen: Vec<SocketAddr>,
    tailnet_hostname: Option<String>,
    tailscale_bin: Option<PathBuf>,
    tailnet_auto: bool,
    auto_listen: Vec<SocketAddr>,
    auto_link_listen: Vec<SocketAddr>,
    revision: String,
}

fn unreadable() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "settings_unreadable",
        "the deck config file cannot be read or does not parse; see the deck log",
    )
}

fn addresses(values: &Map<String, Value>, key: &str) -> Option<Vec<SocketAddr>> {
    values
        .get(key)?
        .as_array()?
        .iter()
        .map(|v| v.as_str()?.parse().ok())
        .collect()
}

/// The saved settings, or `None` when this deck has no settings file.
pub(super) async fn read_saved(board: &Board) -> Result<Option<Saved>, ApiError> {
    let Some(settings) = board.settings.as_ref() else {
        return Ok(None);
    };
    let view = deck_view(settings).await.map_err(|e| {
        tracing::warn!(error = %e, "deck settings");
        unreadable()
    })?;
    let v = &view.values;
    let (Some(listen), Some(link_listen)) = (addresses(v, "listen"), addresses(v, "link_listen"))
    else {
        return Err(unreadable());
    };
    let text = |key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    Ok(Some(Saved {
        listen,
        link_listen,
        tailnet_hostname: text("tailnet_hostname"),
        tailscale_bin: text("tailscale_bin").map(PathBuf::from),
        tailnet_auto: v.get("tailnet_auto").and_then(Value::as_bool) == Some(true),
        auto_listen: addresses(v, "tailnet_auto_listen").unwrap_or_default(),
        auto_link_listen: addresses(v, "tailnet_auto_link_listen").unwrap_or_default(),
        revision: view.revision,
    }))
}

pub(super) fn cli_of(saved: Option<&Saved>) -> TailscaleCli {
    TailscaleCli::locate(saved.and_then(|s| s.tailscale_bin.as_deref()))
}

fn texts<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

#[derive(Serialize)]
struct SelfView {
    dns_name: String,
    ips: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os: Option<String>,
}

#[derive(Serialize)]
struct Lists {
    listen: Vec<String>,
    link_listen: Vec<String>,
}

#[derive(Serialize)]
struct Managed {
    listen: Vec<String>,
    link_listen: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tailnet_hostname: Option<String>,
}

/// One automatic address that was not used at start.
#[derive(Serialize)]
struct SkippedView {
    address: String,
    reason: String,
}

/// The state of "Use Tailscale automatically" (design 14.4).
#[derive(Serialize)]
struct AutoView {
    /// The saved setting `tailnet_auto`.
    enabled: bool,
    /// Why the mode did not apply at start (shown only when it was on).
    #[serde(skip_serializing_if = "Option::is_none")]
    start_reason: Option<String>,
    /// The automatic addresses that were not used at start.
    skipped: Vec<SkippedView>,
    /// The usable tailnet addresses now differ from the automatic entries.
    changed: bool,
}

/// The state of the trusted certificate from Tailscale (DL-190). It never
/// holds text of `tailscale`: only a reason code.
#[derive(Clone, Serialize)]
struct CertificateView {
    enabled: bool,
    /// `off`, `valid`, `renewing`, `failed`, `expired` or `name_mismatch`.
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// The end of the certificate, Unix ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    not_after: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<&'static str>,
    /// The owner's own certificate files (`board.crt`) are in use.
    owner_files: bool,
}

impl CertificateView {
    fn of(state: &CertState, owner_files: bool) -> Self {
        Self {
            owner_files,
            enabled: state.enabled,
            state: state.state.as_str(),
            name: state.name.clone(),
            not_after: state.not_after_ms,
            reason_code: state.reason_code,
        }
    }
}

/// The answer of `GET /api/tailnet`.
#[derive(Serialize)]
pub struct TailnetView {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// How the program was chosen: `configured`, `path`, `macos_app` or
    /// `windows` (design 15.1).
    #[serde(skip_serializing_if = "Option::is_none")]
    program: Option<&'static str>,
    #[serde(rename = "self", skip_serializing_if = "Option::is_none")]
    this: Option<SelfView>,
    userspace: bool,
    managed: Managed,
    current: Lists,
    auto: AutoView,
    certificate: CertificateView,
}

/// `GET /api/tailnet`.
///
/// # Errors
/// 403 `owner_only`; 500 `settings_unreadable`.
pub async fn get(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<TailnetView>, ApiError> {
    owner(
        &board,
        &principal,
        "tailnet.get",
        "",
        "reads the tailnet status",
    )
    .await?;
    let saved = read_saved(&board).await?;
    let facts = (board.host)();
    let start = board
        .tailnet_start
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let certificate = CertificateView::of(
        &board.cert.read().unwrap_or_else(PoisonError::into_inner),
        board.endpoints.owner(),
    );
    let cli = cli_of(saved.as_ref());
    let status = status_fresh(&cli).await;
    // A program was chosen unless the lookup found nothing to run.
    let program =
        (!matches!(status, Err(TailscaleError::NotInstalled))).then(|| cli.kind().as_str());
    Ok(Json(view_of(
        saved.as_ref(),
        status,
        program,
        &facts,
        &start,
        certificate,
    )))
}

fn view_of(
    saved: Option<&Saved>,
    status: Result<Status, TailscaleError>,
    program: Option<&'static str>,
    facts: &HostFacts,
    start: &TailnetStart,
    certificate: CertificateView,
) -> TailnetView {
    let (listen, link_listen) = saved.map_or((&[][..], &[][..]), |s| {
        (s.listen.as_slice(), s.link_listen.as_slice())
    });
    let hostname = saved.and_then(|s| s.tailnet_hostname.clone());
    let current = Lists {
        listen: texts(listen),
        link_listen: texts(link_listen),
    };
    let enabled = saved.is_some_and(|s| s.tailnet_auto);
    let mut auto = AutoView {
        enabled,
        start_reason: None,
        skipped: Vec::new(),
        changed: false,
    };
    if start.enabled {
        auto.start_reason = start.start_reason.clone();
        auto.skipped = start
            .skipped
            .iter()
            .map(|(address, reason)| SkippedView {
                address: address.to_string(),
                reason: reason.clone(),
            })
            .collect();
    }
    let unchanged = |reason: Option<String>, auto: AutoView| TailnetView {
        available: reason.is_none(),
        reason: reason.clone(),
        program,
        this: None,
        userspace: false,
        managed: Managed {
            listen: texts(listen),
            link_listen: texts(link_listen),
            tailnet_hostname: hostname.clone(),
        },
        current: Lists {
            listen: texts(listen),
            link_listen: texts(link_listen),
        },
        auto,
        certificate: certificate.clone(),
    };
    let status = match status {
        Ok(s) => s,
        Err(e) => return unchanged(Some(e.reason()), auto),
    };
    let plan = tailscale::plan_use(&status, listen, link_listen, tailscale::is_usable);
    let this = SelfView {
        dns_name: status.this.dns_name.clone(),
        ips: texts(&status.this.ips),
        login: status.this.login.clone(),
        os: status.this.os.clone(),
    };
    let managed = if plan.userspace {
        Managed {
            listen: texts(listen),
            link_listen: texts(link_listen),
            tailnet_hostname: hostname,
        }
    } else {
        Managed {
            listen: texts(&plan.listen),
            link_listen: texts(&plan.link_listen),
            tailnet_hostname: plan.tailnet_hostname.or(hostname),
        }
    };
    if let Some(saved) = saved {
        auto.changed = tailnet_auto::changed(
            saved.tailnet_auto,
            plan.userspace,
            &plan.usable_ips,
            saved.auto_listen.iter().chain(&saved.auto_link_listen),
        );
    }
    TailnetView {
        available: true,
        // On WSL with NAT networking the Windows addresses are not here.
        reason: (plan.userspace && facts.wsl).then(|| WSL_NAT.to_owned()),
        program,
        this: Some(this),
        userspace: plan.userspace,
        managed,
        current,
        auto,
        certificate,
    }
}

/// The body of `POST /api/tailnet/use`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseBody {
    #[serde(default)]
    elevation: Option<String>,
}

/// The answer of `POST /api/tailnet/use`.
#[derive(Serialize)]
pub struct UseAnswer {
    saved: bool,
    restart_needed: bool,
    listen: Vec<String>,
    link_listen: Vec<String>,
    tailnet_hostname: Option<String>,
}

fn body_error(_: &JsonRejection) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        "the request body is not valid",
    )
}

/// `POST /api/tailnet/use { elevation? }`.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required` (with `keys`), `invalid`,
/// `cannot_apply`; 403 `owner_only`; 409 `tailnet_unavailable`,
/// `tailnet_userspace`, `settings_changed`, `settings_changed_on_disk`; 500
/// `settings_write_failed`, `settings_unreadable`; 503 `settings_unavailable`.
pub async fn use_tailnet(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<UseBody>, JsonRejection>,
) -> Result<Json<UseAnswer>, ApiError> {
    let session = owner(
        &board,
        &principal,
        USE_ACTION,
        "",
        "puts the tailnet addresses in the deck settings",
    )
    .await?;
    let Json(body) = body.map_err(|e| body_error(&e))?;
    use_with(&board, &session, body.elevation, tailscale::is_usable)
        .await
        .map(Json)
}

/// The audit action of "Use tailnet".
const USE_ACTION: &str = "tailnet.use";

/// "Use tailnet" for the owner `session`: `is_usable` tells whether a
/// tailnet address of this machine is assigned to a local interface (the
/// route passes [`tailscale::is_usable`]). The audit row has the
/// keys that really changed, never a value.
///
/// # Errors
/// As [`use_tailnet`], after the owner check.
pub async fn use_with(
    board: &Arc<Board>,
    session: &LoginSession,
    elevation: Option<String>,
    is_usable: impl Fn(IpAddr) -> bool,
) -> Result<UseAnswer, ApiError> {
    let mut keys = Vec::new();
    let result = apply_use(board, session, elevation, &mut keys, is_usable).await;
    audit(board, USE_ACTION, "", &keys, &result).await;
    result
}

async fn apply_use(
    board: &Arc<Board>,
    session: &LoginSession,
    elevation: Option<String>,
    keys: &mut Vec<String>,
    is_usable: impl Fn(IpAddr) -> bool,
) -> Result<UseAnswer, ApiError> {
    let Some(settings) = board.settings.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "settings_unavailable",
            "this deck has no settings file set up",
        ));
    };
    let saved = read_saved(board).await?.ok_or_else(unreadable)?;
    let status = status_fresh(&cli_of(Some(&saved))).await.map_err(|e| {
        ApiError::new(
            StatusCode::CONFLICT,
            "tailnet_unavailable",
            format!("Tailscale is not available: {}", e.reason()),
        )
    })?;
    let plan = tailscale::plan_use(&status, &saved.listen, &saved.link_listen, is_usable);
    if plan.userspace {
        let text = if (board.host)().wsl {
            WSL_NAT
        } else {
            "Tailscale runs in userspace networking: set the listen addresses by hand"
        };
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "tailnet_userspace",
            text,
        ));
    }
    let mut changes = Map::new();
    if plan.listen != saved.listen {
        changes.insert("listen".to_owned(), json!(texts(&plan.listen)));
    }
    if plan.link_listen != saved.link_listen {
        changes.insert("link_listen".to_owned(), json!(texts(&plan.link_listen)));
    }
    // The entries that "Use tailnet" put in each list are the ones that the
    // automatic mode may replace later (design 14.4): every entry of the
    // tailnet range, as the plan replaces every one.
    let added = |list: &[SocketAddr]| -> Vec<SocketAddr> {
        list.iter()
            .copied()
            .filter(|a| netscope::classify(a.ip()) == AddrClass::Tailnet)
            .collect()
    };
    for (key, list, saved_list) in [
        (
            "tailnet_auto_listen",
            added(&plan.listen),
            &saved.auto_listen,
        ),
        (
            "tailnet_auto_link_listen",
            added(&plan.link_listen),
            &saved.auto_link_listen,
        ),
    ] {
        if list != *saved_list {
            changes.insert(key.to_owned(), json!(texts(&list)));
        }
    }
    if let Some(name) = plan
        .tailnet_hostname
        .as_ref()
        .filter(|n| saved.tailnet_hostname.as_ref() != Some(*n))
    {
        changes.insert("tailnet_hostname".to_owned(), json!(name));
    }
    if changes.is_empty() {
        return Ok(UseAnswer {
            saved: true,
            restart_needed: false,
            listen: texts(&saved.listen),
            link_listen: texts(&saved.link_listen),
            tailnet_hostname: saved.tailnet_hostname,
        });
    }
    // The audit row has the keys that change (also when the save fails).
    *keys = changes.keys().cloned().collect();
    keys.sort();
    let elevated = is_elevated(board, session, elevation.as_deref());
    let change = SettingsChange {
        changes,
        revision: saved.revision,
        elevation,
    };
    // This save writes the provenance lists itself.
    let done = save_unreconciled(settings, &board.store, &board.live, change, elevated)
        .await
        .map_err(|e| save_error(&e))?;
    let list = |key: &str| -> Vec<String> {
        done.values
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(UseAnswer {
        saved: true,
        restart_needed: !done.restart_needed.is_empty(),
        listen: list("listen"),
        link_listen: list("link_listen"),
        tailnet_hostname: done
            .values
            .get("tailnet_hostname")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// The error of a failed save: the status and the fixed code of the wire.
fn save_error(e: &SaveError) -> ApiError {
    let (status, code) = match e.code() {
        "settings_changed" => (StatusCode::CONFLICT, "settings_changed"),
        "settings_changed_on_disk" => (StatusCode::CONFLICT, "settings_changed_on_disk"),
        "invalid" => (StatusCode::BAD_REQUEST, "invalid"),
        "cannot_apply" => (StatusCode::BAD_REQUEST, "cannot_apply"),
        "elevation_required" => (StatusCode::BAD_REQUEST, "elevation_required"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "settings_write_failed"),
    };
    let err = ApiError::new(status, code, e.to_string());
    match e {
        SaveError::ElevationRequired { keys } => {
            let mut details = Map::new();
            details.insert("keys".to_owned(), json!(keys));
            err.with_details(details)
        }
        _ => err,
    }
}

/// One device of `GET /api/tailnet/devices`.
#[derive(Serialize)]
struct DeviceView {
    host_name: String,
    dns_name: String,
    ips: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    login: Option<String>,
    tags: Vec<String>,
    online: bool,
    /// `paired`, `request waiting` or `none`.
    state: &'static str,
    /// The command for the station owner while the pairing window is open
    /// (tailnet design 5.2); `null` otherwise, and for a paired device.
    command: Option<String>,
}

/// The answer of `GET /api/tailnet/devices`.
#[derive(Serialize)]
pub struct DevicesView {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    devices: Vec<DeviceView>,
}

/// The host of a stored station address (`ip:port`, `[ip]:port`,
/// `name:port`), as an address or a lower-case name.
enum Host {
    Ip(IpAddr),
    Name(String),
}

fn host_of(dial_url: &str) -> Host {
    if let Ok(addr) = dial_url.parse::<SocketAddr>() {
        return Host::Ip(addr.ip());
    }
    let host = dial_url.rsplit_once(':').map_or(dial_url, |(h, _)| h);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(ip) => Host::Ip(ip),
        Err(_) => Host::Name(host.trim_end_matches('.').to_ascii_lowercase()),
    }
}

/// A device is `paired` when a paired station is dialed at one of its
/// addresses or at its DNS name, or when a live station link comes from one
/// of its addresses (a station that dials the deck has no stored address).
/// Every other device is `none`.
fn state_of(device: &Node, hosts: &[Host], live: &BTreeSet<IpAddr>) -> &'static str {
    let dns = device.dns_name.to_ascii_lowercase();
    let paired = hosts.iter().any(|h| match h {
        Host::Ip(ip) => device.ips.contains(ip),
        Host::Name(name) => !dns.is_empty() && *name == dns,
    }) || device
        .ips
        .iter()
        .any(|ip| live.contains(&ip.to_canonical()));
    if paired { "paired" } else { "none" }
}

/// `GET /api/tailnet/devices`.
///
/// # Errors
/// 403 `owner_only`; 500 `settings_unreadable`, `internal`.
pub async fn devices(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<DevicesView>, ApiError> {
    owner(
        &board,
        &principal,
        "tailnet.devices",
        "",
        "lists the devices of the tailnet",
    )
    .await?;
    let saved = read_saved(&board).await?;
    let status = match status_cached(&cli_of(saved.as_ref())).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(Json(DevicesView {
                available: false,
                reason: Some(e.reason()),
                devices: Vec::new(),
            }));
        }
    };
    let hosts: Vec<Host> = board
        .store
        .stations()
        .await
        .map_err(ApiError::internal)?
        .iter()
        .filter_map(|s| s.dial_url.as_deref())
        .map(host_of)
        .collect();
    let live = board.store.link_peer_ips();
    let hub = board.pairing.as_ref().map(|n| n.pair_requests());
    let command = match hub {
        Some(hub) if hub.is_open() => join_command(&status.this, hub),
        _ => None,
    };
    let mut peers = status.peers;
    peers.sort_by(|a, b| (&a.host_name, &a.dns_name).cmp(&(&b.host_name, &b.dns_name)));
    let mut devices = Vec::new();
    for d in &peers {
        let mut state = state_of(d, &hosts, &live);
        if state != "paired"
            && let Some(hub) = hub
        {
            for ip in &d.ips {
                if hub.has_request_from(*ip).await {
                    state = "request waiting";
                    break;
                }
            }
        }
        devices.push(DeviceView {
            host_name: d.host_name.clone(),
            dns_name: d.dns_name.clone(),
            ips: texts(&d.ips),
            os: d.os.clone(),
            login: d.login.clone(),
            tags: d.tags.clone(),
            online: d.online,
            state,
            command: if state == "paired" {
                None
            } else {
                command.clone()
            },
        });
    }
    Ok(Json(DevicesView {
        available: true,
        reason: None,
        devices,
    }))
}

/// The exact station command of tailnet design 5.2 and DL-178:
/// `fleetdeck station join --tailnet <host>:<port> --deck-fingerprint <64 hex>`.
/// The host is the DNS name of this deck, or its first tailnet address when
/// it has no DNS name (an IPv6 address in brackets); the port is the one of
/// the first running tailnet link listener. `None` without such a listener.
fn join_command(this: &Node, hub: &fleetdeck_link::pair_hub::PairRequests) -> Option<String> {
    let port = hub.tailnet_addrs().first()?.port();
    let host = if this.dns_name.is_empty() {
        match this.ips.first()? {
            IpAddr::V6(v6) => format!("[{v6}]"),
            IpAddr::V4(v4) => v4.to_string(),
        }
    } else {
        this.dns_name.clone()
    };
    Some(format!(
        "fleetdeck station join --tailnet {host}:{port} --deck-fingerprint {}",
        hub.deck_key().fingerprint()
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_certificate_view_says_when_the_owner_files_are_used() {
        let state = CertState::off();
        for owner in [true, false] {
            let json = serde_json::to_value(CertificateView::of(&state, owner)).unwrap();
            assert_eq!(json["owner_files"], owner);
        }
    }
}
