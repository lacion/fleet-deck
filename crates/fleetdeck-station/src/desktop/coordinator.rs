//! The target side of a coordinator desktop action (R13 P6, feature
//! `desktop_mcp.v1`; `docs/v2/design/omarchy-p6.md` §5.4, §5.6, §6.3).
//!
//! - [`DispatchCheck`] and [`LinkCheck`]: the remote check that runs right
//!   before the effect. [`LinkCheck`] sends `desktop.authority_check` to the
//!   deck on the link of the action; every answer except a clear `valid`
//!   fails closed.
//! - [`validate`]: the checks of the context and of the action kind that run
//!   right after the admission (§6.3 item 1).
//! - [`Live`], [`check_live`] and [`check_live_input`]: the live local gates
//!   of an action or a read (§2.3; R13 P7 §3.3).
//! - [`preview_host_ok`] and [`new_window_id`]: the preview of §4.4 and §4.5.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_link::control::Control;
use fleetdeck_proto::coordinator_desktop::{
    CoordinatorCapabilities, CoordinatorDesktopInfo, CoordinatorMonitor, CoordinatorWall,
};
use fleetdeck_proto::coordinator_desktop_text::{code as p6_code, deadline_ms, limits, text};
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, AuthorityReason, CoordinatorContext, DesktopAuthorityAnswer,
    DesktopAuthorityCheck, DesktopInputGatesAnswer, DesktopInputPolicyAnswer, PolicyPurpose,
    ProtectedRead,
};
use fleetdeck_proto::desktop::{DesktopAction, DesktopInfo, Urgency};
use fleetdeck_proto::desktop_authority::AUTHORITY_REFUSED;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::{Negotiated, feature};
use ring::rand::{SecureRandom, SystemRandom};

use super::capture::{Allowed, CAPTURE_QUOTA, FREE_RESERVE, capture_gate};
use super::events::PreviewOwner;
use super::input::grammar;
use super::{Fail, code};
use crate::manager::Policy;

/// The live local gates of a coordinator action, read again after the remote
/// check (§6.3 item 5).
pub type Gates = Arc<dyn Fn() -> Result<(), Fail> + Send + Sync>;

/// What a coordinator action needs of the authenticated link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extras {
    /// The authenticated deck fingerprint (64 hex).
    pub deck_fp: String,
    /// This station's live tailnet name, lower case, without the trailing
    /// dot. `None` when it is not known: a preview then opens only on the
    /// three loopback hosts.
    pub tailnet_host: Option<String>,
}

/// The remote check right before an effect (§6.3 item 4).
#[async_trait]
pub trait DispatchCheck: Send + Sync {
    /// `Ok` only when the deck confirmed the authority for `phase`.
    ///
    /// # Errors
    /// `authority_lost` when the deck withdrew the authority;
    /// `authority_unknown` for every other answer.
    async fn check(&self, phase: AuthorityPhase, read: Option<&ProtectedRead>) -> Result<(), Fail>;
}

/// The check over the deck link: `desktop.authority_check`.
pub struct LinkCheck {
    control: Control,
    operation_id: String,
    action_hash: String,
}

impl LinkCheck {
    /// For the operation `operation_id` (the `action_id`, or the `read_id`
    /// of a read) and its hash (the `action_hash`, or the `read_hash`).
    #[must_use]
    pub fn new(control: Control, operation_id: String, action_hash: String) -> Self {
        Self {
            control,
            operation_id,
            action_hash,
        }
    }
}

pub fn p6_fail(code: &'static str) -> Fail {
    Fail::new(code, text(code).unwrap_or(code))
}

pub(crate) fn lost() -> Fail {
    p6_fail(code::AUTHORITY_LOST)
}

pub(crate) fn unknown() -> Fail {
    p6_fail(code::AUTHORITY_UNKNOWN)
}

/// The failure of a deck answer: `Ok` for `valid: true`, `authority_lost`
/// for `valid: false` with that reason, and `authority_unknown` for every
/// other answer (an error, a malformed answer, any other reason).
pub(crate) fn answer_result(outcome: &Outcome) -> Result<(), Fail> {
    let Outcome::Ok(value) = outcome else {
        return Err(unknown());
    };
    match serde_json::from_value::<DesktopAuthorityAnswer>(value.clone()) {
        Ok(DesktopAuthorityAnswer { valid: true, .. }) => Ok(()),
        Ok(DesktopAuthorityAnswer {
            valid: false,
            reason: Some(AuthorityReason::AuthorityLost),
        }) => Err(lost()),
        _ => Err(unknown()),
    }
}

#[async_trait]
impl DispatchCheck for LinkCheck {
    async fn check(&self, phase: AuthorityPhase, read: Option<&ProtectedRead>) -> Result<(), Fail> {
        let body = DesktopAuthorityCheck {
            operation_id: self.operation_id.clone(),
            action_hash: self.action_hash.clone(),
            phase,
            read: read.cloned(),
        };
        let Ok(params) = serde_json::to_value(&body) else {
            return Err(unknown());
        };
        let limit = Duration::from_millis(deadline_ms::AUTHORITY_CHECK);
        let sent = tokio::time::timeout(
            limit,
            self.control
                .request("desktop.authority_check", None, params),
        )
        .await;
        match sent {
            Ok(Ok(outcome)) => answer_result(&outcome),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "the desktop authority check had no answer");
                Err(unknown())
            }
            Err(_) => {
                tracing::debug!("the desktop authority check timed out");
                Err(unknown())
            }
        }
    }
}

/// The refusal of an authority that is missing, invalid or not allowed here.
#[must_use]
pub fn refusal() -> Fail {
    Fail::new(code::FORBIDDEN, AUTHORITY_REFUSED)
}

/// A label of at most 64 characters from `[A-Za-z0-9_-]`.
fn label_ok(label: &str) -> bool {
    label.chars().count() <= 64
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The checks of a coordinator authority that run right after the
/// admission, before the desktop is inspected (§6.3 item 1), in this order:
/// the link features, the context, the kind of the action and the urgency of
/// a notification. An authority without a coordinator context (an owner or a
/// wake) is refused: only a coordinator sends `preview_close`.
///
/// # Errors
/// `forbidden` with the text of a refused authority, for every failure.
pub fn validate<'a>(
    ctx: Option<&'a CoordinatorContext>,
    action: &DesktopAction,
    negotiated: &Negotiated,
) -> Result<&'a CoordinatorContext, Fail> {
    let ctx = ctx.ok_or_else(refusal)?;
    if !negotiated.has(feature::DESKTOP_MCP) {
        return Err(refusal());
    }
    if matches!(action, DesktopAction::ScreenCapture { .. })
        && !negotiated.has(feature::CAPTURE_MCP)
    {
        return Err(refusal());
    }
    // R13 P7 §6.2: an input action needs `desktop_input_mcp.v1` on the link.
    // R13 P7b §4: the `any_window` variants also need `desktop_input_any.v1`
    // and the click variants `desktop_click_mcp.v1`.
    if grammar::is_input(action)
        && !(negotiated.has(feature::DESKTOP_INPUT_MCP)
            && (!grammar::is_any(action) || negotiated.has(feature::DESKTOP_INPUT_ANY))
            && (!grammar::is_click(action) || negotiated.has(feature::DESKTOP_CLICK_MCP)))
    {
        return Err(refusal());
    }
    // R13 P5b-2: a coordinator recording needs the three recording features
    // on the link, else the existing refusal.
    if matches!(action, DesktopAction::ScreenRecord { .. })
        && !(negotiated.has(feature::DESKTOP_RECORDING)
            && negotiated.has(feature::ARTIFACTS_VIDEO)
            && negotiated.has(feature::CAPTURE_RECORDING_MCP))
    {
        return Err(refusal());
    }
    let fields = [
        ctx.grant_id.as_str(),
        ctx.binding_id.as_str(),
        ctx.caller_station.as_str(),
        ctx.caller_session.as_str(),
        ctx.caller_label.as_str(),
    ];
    if fields.iter().any(|f| f.is_empty()) || !label_ok(&ctx.caller_label) {
        return Err(refusal());
    }
    match action {
        DesktopAction::Notify { urgency, .. } => {
            if *urgency == Urgency::Critical {
                return Err(refusal());
            }
        }
        DesktopAction::BrowserOpen { .. }
        | DesktopAction::PreviewClose { .. }
        | DesktopAction::ScreenCapture { .. }
        | DesktopAction::ScreenRecord { .. } => {}
        // R13 P7 §6.2: the arguments of an input action are checked again
        // with the proto validators.
        DesktopAction::InputKeyPress { .. }
        | DesktopAction::InputTypeText { .. }
        | DesktopAction::InputPointerMove { .. }
        | DesktopAction::InputAnyKeyPress { .. }
        | DesktopAction::InputAnyTypeText { .. }
        | DesktopAction::InputAnyPointerMove { .. } => grammar::check(action)?,
        // R13 P7b task S2a: the click variants pass here and are refused with
        // `desktop_unsupported` by the input protocol, after the gates.
        DesktopAction::InputPointerClick { .. } | DesktopAction::InputAnyPointerClick { .. } => {}
        _ => return Err(refusal()),
    }
    Ok(ctx)
}

/// The kind of a claimed action (§7.3); `None` for a screenshot, which has
/// the capture ledger.
#[must_use]
pub fn claim_kind(action: &DesktopAction) -> Option<&'static str> {
    match action {
        DesktopAction::Notify { .. } => Some("notify"),
        DesktopAction::BrowserOpen { .. } => Some("preview_open"),
        DesktopAction::PreviewClose { .. } => Some("preview_close"),
        // R13 P7 §6.3: the input claims.
        DesktopAction::InputKeyPress { .. } => Some("input_key_press"),
        DesktopAction::InputTypeText { .. } => Some("input_type_text"),
        DesktopAction::InputPointerMove { .. } => Some("input_pointer_move"),
        // R13 P7b §6.3: the broader and click claims (store V38).
        DesktopAction::InputPointerClick { .. } => Some("input_pointer_click"),
        DesktopAction::InputAnyKeyPress { .. } => Some("input_any_key_press"),
        DesktopAction::InputAnyTypeText { .. } => Some("input_any_type_text"),
        DesktopAction::InputAnyPointerMove { .. } => Some("input_any_pointer_move"),
        DesktopAction::InputAnyPointerClick { .. } => Some("input_any_pointer_click"),
        _ => None,
    }
}

/// The live local settings that the gates read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live {
    pub fleet_control: bool,
    pub desktop: bool,
    pub capture: bool,
    /// `allow_desktop_input` (R13 P7 §3.3).
    pub desktop_input: bool,
    /// `allow_desktop_click` (R13 P7b §3.2).
    pub desktop_click: bool,
    /// `allow_desktop_input_any` (R13 P7b §3.2).
    pub desktop_input_any: bool,
    /// `capture_monitors`; empty means every monitor.
    pub monitors: Vec<String>,
}

impl Live {
    /// The values of one policy snapshot.
    #[must_use]
    pub fn of(policy: &Policy) -> Self {
        Self {
            fleet_control: policy.allow_fleet_control(),
            desktop: policy.allow_desktop(),
            capture: policy.allow_desktop_capture(),
            desktop_input: policy.allow_desktop_input(),
            desktop_click: policy.allow_desktop_click(),
            desktop_input_any: policy.allow_desktop_input_any(),
            monitors: policy.capture_monitors().to_vec(),
        }
    }
}

/// The failure of `allow_fleet_control` off.
#[must_use]
pub fn fleet_off() -> Fail {
    Fail::new(code::FORBIDDEN, "Fleet control is off on this station.")
}

/// The live gates (§2.3): `allow_fleet_control` and `allow_desktop`; for a
/// capture (`capture` is true) also `allow_desktop_capture`, and the
/// `capture_monitors` rule when `monitor` is given. A capture failure keeps
/// the P5a codes and texts.
///
/// # Errors
/// `forbidden` with the text of the first gate that is off.
pub fn check_live(live: &Live, capture: bool, monitor: Option<&str>) -> Result<(), Fail> {
    if !live.fleet_control {
        return Err(fleet_off());
    }
    if !capture {
        return if live.desktop {
            Ok(())
        } else {
            Err(Fail::forbidden())
        };
    }
    capture_gate(
        &Allowed {
            desktop: live.desktop,
            capture: live.capture,
            monitors: live.monitors.clone(),
            quota_bytes: CAPTURE_QUOTA,
            free_reserve_bytes: FREE_RESERVE,
        },
        monitor,
    )
}

/// The failure of `allow_desktop_input` off (R13 P7 §3.3 item 10).
#[must_use]
pub fn input_off() -> Fail {
    Fail::new(code::FORBIDDEN, "Desktop input is off on this station.")
}

/// The live gates of an input action (R13 P7 §3.3 items 8 to 11), in this
/// order: `allow_fleet_control`, `allow_desktop`, `allow_desktop_input`, for
/// the `InputAny*` variants `allow_desktop_input_any` (R13 P7b §3.2), for the
/// click variants `allow_desktop_click`, then `allow_desktop_capture`. The
/// monitor rule of item 14 needs the preview and is checked by the input
/// protocol.
///
/// # Errors
/// `forbidden` with the text of the first P7 gate that is off;
/// `input_any_disabled` or `input_click_disabled` for a P7b gate.
pub fn check_live_input(live: &Live, action: &DesktopAction) -> Result<(), Fail> {
    check_live_gates(live, grammar::is_any(action), grammar::is_click(action))
}

/// The live gates of the `window_list` read: those of an `InputAny*` action
/// without the click gate (R13 P7b §6.1).
///
/// # Errors
/// As [`check_live_input`].
pub fn check_live_window_list(live: &Live) -> Result<(), Fail> {
    check_live_gates(live, true, false)
}

/// The gates of [`check_live_input`] for an action that is (`any`) one of the
/// `InputAny*` variants and (`click`) one of the click variants.
///
/// # Errors
/// As [`check_live_input`].
pub fn check_live_gates(live: &Live, any: bool, click: bool) -> Result<(), Fail> {
    if !live.fleet_control {
        return Err(fleet_off());
    }
    if !live.desktop {
        return Err(Fail::forbidden());
    }
    if !live.desktop_input {
        return Err(input_off());
    }
    if any && !live.desktop_input_any {
        return Err(p6_fail(p6_code::INPUT_ANY_DISABLED));
    }
    if click && !live.desktop_click {
        return Err(p6_fail(p6_code::INPUT_CLICK_DISABLED));
    }
    capture_gate(
        &Allowed {
            desktop: live.desktop,
            capture: live.capture,
            monitors: Vec::new(),
            quota_bytes: CAPTURE_QUOTA,
            free_reserve_bytes: FREE_RESERVE,
        },
        None,
    )
}

/// The answer of `desktop.policy_now` for the input purpose (R13 P7 §6.4).
/// A null monitor is unresolved: the preview geometry belongs to the target,
/// so the answer allows it and the target enforces the real monitor coverage
/// before and during the input. A named monitor follows the capture monitor
/// rule.
#[must_use]
pub fn input_policy_answer(live: &Live, monitor: Option<&str>) -> DesktopInputPolicyAnswer {
    DesktopInputPolicyAnswer {
        allow_fleet_control: live.fleet_control,
        allow_desktop: live.desktop,
        allow_desktop_capture: live.capture,
        allow_desktop_input: live.desktop_input,
        monitor_allowed: match monitor {
            None => true,
            Some(m) => live.monitors.is_empty() || live.monitors.iter().any(|x| x == m),
        },
    }
}

/// The answer of `desktop.policy_now` for the purposes `input_click`,
/// `input_any` and `input_any_click` (R13 P7b §9.2): the five fields of
/// [`input_policy_answer`] and the station gates that the purpose needs.
#[must_use]
pub fn input_gates_answer(
    live: &Live,
    monitor: Option<&str>,
    purpose: PolicyPurpose,
) -> DesktopInputGatesAnswer {
    let five = input_policy_answer(live, monitor);
    let click = matches!(
        purpose,
        PolicyPurpose::InputClick | PolicyPurpose::InputAnyClick
    );
    let any = matches!(
        purpose,
        PolicyPurpose::InputAny | PolicyPurpose::InputAnyClick
    );
    DesktopInputGatesAnswer {
        allow_fleet_control: five.allow_fleet_control,
        allow_desktop: five.allow_desktop,
        allow_desktop_capture: five.allow_desktop_capture,
        allow_desktop_input: five.allow_desktop_input,
        monitor_allowed: five.monitor_allowed,
        allow_desktop_click: click.then_some(live.desktop_click),
        allow_desktop_input_any: any.then_some(live.desktop_input_any),
    }
}

/// The `CoordinatorDesktopInfo` of a fresh desktop info (§4.1). The deck
/// fills `station`.
///
/// # Errors
/// `desktop_result_too_large` above the monitor, wall or byte limit.
pub fn map_info(info: &DesktopInfo, live: &Live) -> Result<CoordinatorDesktopInfo, Fail> {
    let too_large = || p6_fail(p6_code::DESKTOP_RESULT_TOO_LARGE);
    let max = |n: u32| usize::try_from(n).unwrap_or(usize::MAX);
    if info.monitors.len() > max(limits::INFO_MAX_MONITORS) {
        return Err(too_large());
    }
    let monitors = info
        .monitors
        .iter()
        .map(|m| CoordinatorMonitor {
            name: m.name.clone(),
            width: m.width,
            height: m.height,
            scale_pct: m.scale_pct,
            focused: m.focused,
            dpms_on: m.dpms_on,
            capture_allowed: live.monitors.is_empty() || live.monitors.contains(&m.name),
        })
        .collect();
    let walls: Vec<CoordinatorWall> = info
        .walls
        .iter()
        .filter_map(|w| {
            w.monitor.as_ref().map(|monitor| CoordinatorWall {
                display_id: w.display_id.clone(),
                monitor: monitor.clone(),
            })
        })
        .collect();
    if walls.len() > max(limits::INFO_MAX_WALLS) {
        return Err(too_large());
    }
    let out = CoordinatorDesktopInfo {
        station: String::new(),
        kind: info.kind,
        monitors,
        locked: info.locked,
        capabilities: CoordinatorCapabilities {
            notify: live.desktop && info.tools.notify_send,
            preview: live.desktop && info.tools.chromium,
            screenshot: live.capture && info.tools.grim,
        },
        walls,
    };
    let size = serde_json::to_vec(&out).map_or(usize::MAX, |bytes| bytes.len());
    if u64::try_from(size).unwrap_or(u64::MAX) > limits::INFO_MAX_BYTES {
        return Err(too_large());
    }
    Ok(out)
}

/// Whether the host of `url` is one that a coordinator preview may open: it
/// must equal `localhost`, `127.0.0.1`, `[::1]` or `tailnet_host`, compared
/// without case. `url` has the grammar of `args::url`.
#[must_use]
pub fn preview_host_ok(url: &str, tailnet_host: Option<&str>) -> bool {
    let Some(start) = url.find("://").map(|i| i + 3) else {
        return false;
    };
    let after = &url[start..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    let host = if authority.starts_with('[') {
        authority.find(']').map_or(authority, |i| &authority[..=i])
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    let host = host.to_ascii_lowercase();
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
        || tailnet_host.is_some_and(|t| !t.is_empty() && t == host)
}

/// The tailnet name for the host list: lower case, without the trailing dot.
#[must_use]
pub fn tailnet_host_of(dns_name: &str) -> Option<String> {
    let name = dns_name.trim_end_matches('.').to_ascii_lowercase();
    (!name.is_empty()).then_some(name)
}

/// A window handle: 32 lowercase hex characters from 16 random bytes.
///
/// # Errors
/// `failed` when the system random source fails.
pub fn new_window_id() -> Result<String, Fail> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Fail::new(code::FAILED, "the system random source failed"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The owner of a preview that `ctx` opens on the link of `deck_fp`.
#[must_use]
pub fn preview_owner(deck_fp: &str, ctx: &CoordinatorContext) -> PreviewOwner {
    PreviewOwner {
        deck_fp: deck_fp.to_owned(),
        grant_id: ctx.grant_id.clone(),
        caller_station: ctx.caller_station.to_string(),
        caller_session: ctx.caller_session.to_string(),
    }
}

#[cfg(test)]
#[path = "coordinator_recording_tests.rs"]
mod recording_tests;
