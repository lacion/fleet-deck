//! The authorization of a coordinator desktop tool (R13 P6 §6.2 steps 2 to 6,
//! decision O12-9). [`authorize`] is the only way to an [`Authorized`] value:
//! it checks, in this order, the coverage of the caller station, the grant and
//! its full binding, the deck switches, the class and the target, and the
//! features of the caller and of the target link. The same module holds the
//! deck-local pass that a send and an authority check repeat ([`local_pass`]).
//!
//! A refusal is a code of `coordinator_desktop_text::code`; a deck store error
//! is `authority_unknown` (and a warning that names no request).

use std::collections::BTreeSet;

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{SessionId, StationId};

use crate::link::DeckLink;
use crate::link::desktop_authority::{Route, resolve};
use crate::store::coordinator_desktop::{OpClass, OpTool};
use crate::store::{Grant, GrantClass, LinkFeature};

/// The nine tools of §3, `record_monitor` (R13 P5b-2 §13.1) and the three
/// input tools (R13 P7 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum Tool {
    WallShow,
    Notify,
    PreviewOpen,
    WindowClose,
    DesktopActionStatus,
    Screenshot,
    RecordMonitor,
    CaptureStatus,
    ReadCaptureImage,
    DesktopInfo,
    KeyPress,
    TypeText,
    PointerMove,
}

impl Tool {
    /// The class that the tool needs; `None` for `desktop_info`, which needs
    /// either class.
    #[must_use]
    pub(crate) fn class(self) -> Option<OpClass> {
        match self {
            Self::WallShow
            | Self::Notify
            | Self::PreviewOpen
            | Self::WindowClose
            | Self::DesktopActionStatus => Some(OpClass::Desktop),
            Self::Screenshot
            | Self::RecordMonitor
            | Self::CaptureStatus
            | Self::ReadCaptureImage => Some(OpClass::Capture),
            Self::KeyPress | Self::TypeText | Self::PointerMove => Some(OpClass::Input),
            Self::DesktopInfo => None,
        }
    }

    /// The tool of an operation row; `None` for a query.
    #[must_use]
    #[allow(dead_code)] // Used by tasks T3 and T5.
    pub(crate) fn op_tool(self) -> Option<OpTool> {
        match self {
            Self::WallShow => Some(OpTool::WallShow),
            Self::Notify => Some(OpTool::Notify),
            Self::PreviewOpen => Some(OpTool::PreviewOpen),
            Self::WindowClose => Some(OpTool::WindowClose),
            Self::Screenshot => Some(OpTool::Screenshot),
            Self::RecordMonitor => Some(OpTool::RecordMonitor),
            Self::KeyPress => Some(OpTool::KeyPress),
            Self::TypeText => Some(OpTool::TypeText),
            Self::PointerMove => Some(OpTool::PointerMove),
            Self::DesktopActionStatus
            | Self::CaptureStatus
            | Self::ReadCaptureImage
            | Self::DesktopInfo => None,
        }
    }
}

/// The checked authority of one P6 call or query. Only [`authorize`] builds
/// it, so no other code can claim an authority.
pub(crate) struct Authorized {
    grant: Grant,
    ctx: CoordinatorContext,
    target: StationId,
    route: Route,
    class: OpClass,
}

impl Authorized {
    #[must_use]
    pub(crate) fn grant(&self) -> &Grant {
        &self.grant
    }

    #[must_use]
    pub(crate) fn ctx(&self) -> &CoordinatorContext {
        &self.ctx
    }

    #[must_use]
    pub(crate) fn target(&self) -> &StationId {
        &self.target
    }

    #[must_use]
    pub(crate) fn route(&self) -> &Route {
        &self.route
    }

    /// The class that backs the call (`desktop_info`: the desktop class when
    /// the grant has it, else the capture class).
    #[must_use]
    pub(crate) fn class(&self) -> OpClass {
        self.class
    }
}

/// The grant class that an operation class needs.
pub(super) fn needs(class: OpClass) -> GrantClass {
    match class {
        OpClass::Desktop => GrantClass::Desktop,
        OpClass::Capture => GrantClass::Capture,
        OpClass::Input => GrantClass::Input,
    }
}

/// The rules of an input grant (R13 P7 §3.1): the three classes `desktop`,
/// `capture` and `input` (`not_granted`), then the explicit allowance and the
/// one supported scope (`input_not_allowed`).
fn input_grant(grant: &Grant) -> Result<(), &'static str> {
    let all = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];
    if !all.iter().all(|c| grant.classes.contains(c)) {
        return Err(code::NOT_GRANTED);
    }
    if !grant.allow_input || grant.input_scope != Some(InputScope::OwnWindows) {
        return Err(code::INPUT_NOT_ALLOWED);
    }
    Ok(())
}

/// A deck store error: a warning without any request data, and
/// `authority_unknown`.
pub(super) fn store_error(e: &crate::store::DeckError) -> &'static str {
    tracing::warn!(error = %e, "coordinator desktop store");
    code::AUTHORITY_UNKNOWN
}

/// The deck switches (§2.3 items 1 and 4): `fleet_control`, then
/// `desktop_control`.
pub(super) fn switches(link: &DeckLink) -> Result<(), &'static str> {
    if !link.store.fleet_control() {
        return Err(code::FLEET_CONTROL_OFF);
    }
    // An empty cell counts as off.
    if !link
        .desktop_live
        .get()
        .is_some_and(|live| live.get().desktop_control)
    {
        return Err(code::DESKTOP_CONTROL_OFF);
    }
    Ok(())
}

/// The features of the caller link (§5.1); a recording also needs
/// `capture_recording_mcp.v1` (R13 P5b-2 §13.1), and an input tool the
/// capture set and `desktop_input_mcp.v1` (R13 P7 §6.1).
fn caller_features(class: OpClass, recording: bool) -> Vec<&'static str> {
    let capture = matches!(class, OpClass::Capture | OpClass::Input);
    let mut wanted = vec![
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
        feature::DESKTOP_MCP,
    ];
    if capture {
        wanted.push(feature::CAPTURE_MCP);
    }
    if recording {
        wanted.push(feature::CAPTURE_RECORDING_MCP);
    }
    if class == OpClass::Input {
        wanted.push(feature::DESKTOP_INPUT_MCP);
    }
    wanted
}

/// The features that a recording needs on the target link besides the
/// capture set (R13 P5b-2 §13.1).
const RECORDING_TARGET: [&str; 3] = [
    feature::DESKTOP_RECORDING,
    feature::ARTIFACTS_VIDEO,
    feature::CAPTURE_RECORDING_MCP,
];

/// The target link has the recording features: `station_unreachable`
/// without a link, `desktop_unsupported` when a link lacks one.
fn recording_ready(link: &DeckLink, target: &StationId) -> Result<(), &'static str> {
    for f in RECORDING_TARGET {
        match link.store.link_feature(target, f) {
            LinkFeature::Down => return Err(code::STATION_UNREACHABLE),
            LinkFeature::Without => return Err(code::DESKTOP_UNSUPPORTED),
            LinkFeature::Negotiated => {}
        }
    }
    Ok(())
}

/// The features of the target link (§5.1).
fn target_features(capture: bool) -> Vec<&'static str> {
    let mut wanted = vec![
        feature::COORDINATOR_V2,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
        feature::DESKTOP_MCP,
    ];
    if capture {
        wanted.extend([
            feature::DESKTOP_CAPTURE,
            feature::ARTIFACTS_DELETE,
            feature::CAPTURE_MCP,
        ]);
    }
    wanted
}

/// The target link has every feature of its class: `station_unreachable`
/// without a link, `desktop_unsupported` when a link lacks one. An input
/// operation needs the capture set and `desktop_input_mcp.v1` (R13 P7 §6.1).
fn target_ready(link: &DeckLink, target: &StationId, class: OpClass) -> Result<(), &'static str> {
    let mut wanted = target_features(matches!(class, OpClass::Capture | OpClass::Input));
    if class == OpClass::Input {
        wanted.push(feature::DESKTOP_INPUT_MCP);
    }
    for f in wanted {
        match link.store.link_feature(target, f) {
            LinkFeature::Down => return Err(code::STATION_UNREACHABLE),
            LinkFeature::Without => return Err(code::DESKTOP_UNSUPPORTED),
            LinkFeature::Negotiated => {}
        }
    }
    Ok(())
}

/// A callsign that the target accepts as a label: 1 to 32 characters of
/// `[a-z0-9-]` (§5.4).
fn callsign_ok(text: &str) -> bool {
    (1..=32).contains(&text.chars().count())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The label of the caller: its generated callsign, else its session ID.
async fn caller_label(
    link: &DeckLink,
    station: &StationId,
    session: &SessionId,
) -> Result<String, &'static str> {
    let summary = link
        .store
        .session_summary(station.clone(), session.clone())
        .await
        .map_err(|e| store_error(&e))?;
    Ok(summary
        .and_then(|s| s.callsign)
        .filter(|c| callsign_ok(c))
        .unwrap_or_else(|| session.as_str().to_owned()))
}

/// What steps 1 to 4 and the caller features found.
pub(super) struct Begun {
    pub(super) grant: Grant,
    pub(super) ctx: CoordinatorContext,
    pub(super) target: StationId,
    pub(super) class: OpClass,
}

/// Steps 1 to 4 of §6.2 and the features of the caller link: everything that
/// needs no target lookup. `desktop_action_status` stops here (no target
/// lookup, §4.8).
///
/// # Errors
/// The code of the first check that fails.
pub(super) async fn begin(
    link: &DeckLink,
    (caller_station, caller_session): (&StationId, &SessionId),
    binding: &CoordinatorBinding,
    tool: Tool,
    station_arg: Option<&str>,
) -> Result<Begun, &'static str> {
    // 1. The caller station is covered by a grant in effect (the rule of
    // `decide_coordinator`).
    match link.store.station_covered_now(caller_station).await {
        Ok(true) => {}
        Ok(false) => return Err(code::NOT_GRANTED),
        Err(e) => return Err(store_error(&e)),
    }
    // 2. The grant of the caller session and its full binding.
    let now = link.store.now_ms();
    let grant = link
        .store
        .desktop_caller_grant(caller_station, caller_session, now)
        .await
        .map_err(|e| store_error(&e))?
        .ok_or(code::NOT_GRANTED)?;
    if grant.binding.as_ref() != Some(binding) {
        return Err(code::BINDING_STALE);
    }
    // 3. The deck switches.
    switches(link)?;
    // 4. The class and the target, before any target or resource lookup.
    let class = match tool.class() {
        Some(class) => {
            if class == OpClass::Input {
                input_grant(&grant)?;
            } else if !grant.classes.contains(&needs(class)) {
                return Err(code::NOT_GRANTED);
            }
            // A recording needs the explicit allowance (R13 P5b-2 §13.2).
            if tool == Tool::RecordMonitor && grant.recording_cap == 0 {
                return Err(code::RECORDING_NOT_ALLOWED);
            }
            class
        }
        None if grant.classes.contains(&GrantClass::Desktop) => OpClass::Desktop,
        None if grant.classes.contains(&GrantClass::Capture) => OpClass::Capture,
        None => return Err(code::NOT_GRANTED),
    };
    let target = station_arg.map_or_else(|| caller_station.clone(), StationId::from);
    if !grant.stations.contains(&target) {
        return Err(code::NOT_GRANTED);
    }
    // 5a. The features of the caller link.
    for f in caller_features(class, tool == Tool::RecordMonitor) {
        if link.store.link_feature(caller_station, f) != LinkFeature::Negotiated {
            return Err(code::DESKTOP_UNSUPPORTED);
        }
    }
    let ctx = CoordinatorContext {
        grant_id: grant.grant_id.clone(),
        binding_id: binding.binding_id.clone(),
        generation: binding.generation,
        caller_station: caller_station.clone(),
        caller_session: caller_session.clone(),
        caller_label: caller_label(link, caller_station, caller_session).await?,
    };
    Ok(Begun {
        grant,
        ctx,
        target,
        class,
    })
}

/// Steps 2 to 6 of §6.2 for a tool that acts on a target station.
///
/// # Errors
/// The code of the first check that fails (see [`begin`]); then
/// `station_unreachable` or `desktop_unsupported` for the target link.
pub(crate) async fn authorize(
    link: &DeckLink,
    caller_station: &StationId,
    caller_session: &SessionId,
    binding: &CoordinatorBinding,
    tool: Tool,
    station_arg: Option<&str>,
) -> Result<Authorized, &'static str> {
    let begun = begin(
        link,
        (caller_station, caller_session),
        binding,
        tool,
        station_arg,
    )
    .await?;
    // 5b. The target link and its features; no desktop epoch is
    // `station_unreachable`.
    target_ready(link, &begun.target, begun.class)?;
    if tool == Tool::RecordMonitor {
        recording_ready(link, &begun.target)?;
    }
    let route = resolve(link, &link.store, &begun.target).map_err(|_| code::STATION_UNREACHABLE)?;
    Ok(Authorized {
        grant: begun.grant,
        ctx: begun.ctx,
        target: begun.target,
        route,
        class: begun.class,
    })
}

/// What a deck-local pass compares with the store.
pub(super) struct Held<'a> {
    pub(super) grant_id: &'a str,
    pub(super) binding_id: &'a str,
    pub(super) generation: u32,
    pub(super) caller: &'a StationId,
    pub(super) target: &'a StationId,
    pub(super) class: OpClass,
}

/// The deck-local grant checks, including the idle expiry (R13 P5b-2 PRE-PR
/// r1): the switches, and the grant (in effect now, the same binding ID and
/// generation, the caller, the target and the class). Returns the grant as it
/// is now. A store error is `authority_unknown`.
///
/// # Errors
/// The code of the first check that fails.
pub(super) async fn grant_pass(link: &DeckLink, held: &Held<'_>) -> Result<Grant, &'static str> {
    switches(link)?;
    let grant = link
        .store
        .grant(held.grant_id.to_owned())
        .await
        .map_err(|e| store_error(&e))?
        .ok_or(code::NOT_GRANTED)?;
    if !crate::store::grant_scopes::in_effect(&grant, link.store.now_ms())
        || grant.station_id != *held.caller
    {
        return Err(code::NOT_GRANTED);
    }
    match &grant.binding {
        Some(b) if b.binding_id == held.binding_id && b.generation == held.generation => {}
        _ => return Err(code::BINDING_STALE),
    }
    let classes: &BTreeSet<GrantClass> = &grant.classes;
    if !classes.contains(&needs(held.class)) || !grant.stations.contains(held.target) {
        return Err(code::NOT_GRANTED);
    }
    // R13 P7 §3.1: the allowance and the scope of an input grant.
    if held.class == OpClass::Input {
        input_grant(&grant)?;
    }
    Ok(grant)
}

/// The deck-local checks that a send and an authority check repeat (§6.2
/// step 10): the switches, the grant (active, not expired, not idle-expired,
/// the same binding ID and generation, the caller, the target and the class),
/// and the features of the target link. Returns the grant as it is now. A
/// store error is `authority_unknown`.
///
/// # Errors
/// The code of the first check that fails.
pub(super) async fn local_pass(link: &DeckLink, held: &Held<'_>) -> Result<Grant, &'static str> {
    let grant = grant_pass(link, held).await?;
    target_ready(link, held.target, held.class)?;
    Ok(grant)
}

/// The deck-local authority to deliver a recording result to its caller (R13
/// P5b-2 §13.4): [`grant_pass`] for the capture class and the recording
/// allowance of the grant. It makes no check of the target link: a reference
/// needs no link to the target.
///
/// # Errors
/// The code of the first check that fails.
pub(super) async fn delivery_pass(
    link: &DeckLink,
    ctx: &CoordinatorContext,
    target: &StationId,
) -> Result<(), &'static str> {
    let held = Held {
        grant_id: &ctx.grant_id,
        binding_id: &ctx.binding_id,
        generation: ctx.generation,
        caller: &ctx.caller_station,
        target,
        class: OpClass::Capture,
    };
    let grant = grant_pass(link, &held).await?;
    if grant.recording_cap == 0 {
        return Err(code::RECORDING_NOT_ALLOWED);
    }
    Ok(())
}

/// The deck-local authority to deliver an input result to its caller (R13 P7
/// §8.3, DL-242): [`grant_pass`] for the input class, which also checks the
/// classes, the allowance and the scope of the grant. It makes no check of the
/// target link.
///
/// # Errors
/// The code of the first check that fails.
pub(super) async fn input_delivery_pass(
    link: &DeckLink,
    ctx: &CoordinatorContext,
    target: &StationId,
) -> Result<(), &'static str> {
    let held = Held {
        grant_id: &ctx.grant_id,
        binding_id: &ctx.binding_id,
        generation: ctx.generation,
        caller: &ctx.caller_station,
        target,
        class: OpClass::Input,
    };
    grant_pass(link, &held).await.map(|_| ())
}

/// The deck-local pass of a coordinator recording (R13 P5b-2 §6.3, §13.2):
/// [`local_pass`] for the capture class, the recording allowance of the grant
/// (`recording_cap > 0`) and the recording features of the target link.
/// Returns the grant as it is now.
///
/// # Errors
/// The code of the first check that fails.
pub(crate) async fn recording_pass(
    link: &DeckLink,
    ctx: &CoordinatorContext,
    target: &StationId,
) -> Result<Grant, &'static str> {
    let held = Held {
        grant_id: &ctx.grant_id,
        binding_id: &ctx.binding_id,
        generation: ctx.generation,
        caller: &ctx.caller_station,
        target,
        class: OpClass::Capture,
    };
    let grant = local_pass(link, &held).await?;
    if grant.recording_cap == 0 {
        return Err(code::RECORDING_NOT_ALLOWED);
    }
    recording_ready(link, target)?;
    Ok(grant)
}

#[cfg(test)]
#[path = "authorize_tests.rs"]
mod tests;
