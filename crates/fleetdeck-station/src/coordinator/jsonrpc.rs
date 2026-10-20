//! The dispatch of one `tools/call` of an authenticated binding (ADR-006
//! §5.1, `docs/v2/design/mcp-p1.md` §3.3). The protocol itself (the
//! handshake, `tools/list`, errors, headers) belongs to `rmcp`
//! (`mcp_server.rs`); this module keeps what the station decides.

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_view::FleetView;
use serde_json::Value;

use fleetdeck_proto::coordinator_desktop_text::code;

use super::bindings::{Binding, BindingState};
use super::desktop_forward::{McpToolResult, p6_error, parse_request};
use super::ingress::{FleetTools, Forward};
use super::{desktop_tools, tools};

/// The tool error of a fleet-control call while the switch is off.
pub(super) const FLEET_CONTROL_OFF: &str = "not_granted: fleet control is off on this station. Ask the owner to turn on \"Allow fleet control\" in the station settings.";

/// The tool error of a call by a binding that is not active.
pub(super) const NOT_BOUND: &str =
    "not_bound: this session is still starting. Try again in a few seconds.";

/// The tool error of a call without the class that the tool needs.
pub(super) fn not_granted(name: &str) -> String {
    format!(
        "forbidden: your grant has no tool `{name}`. Call tools/list to see the tools you have."
    )
}

/// The result of one tool call, before it becomes an MCP result.
pub(super) enum CallOutcome {
    /// A tool error: `isError: true` with this text.
    Error(String),
    /// A tool success with this value.
    Value(Value),
    /// The result of a P6 desktop or capture tool.
    Desktop(McpToolResult),
}

/// Runs one `tools/call` of an authenticated binding. `fleet_control` is the
/// live station setting `allow_fleet_control`, read for this call (ADR-008
/// §3).
pub(super) async fn call(
    binding: &Binding,
    forward: &dyn Forward,
    fleet: Option<&dyn FleetTools>,
    fleet_control: bool,
    name: &str,
    args: Value,
) -> CallOutcome {
    match tools::tool(name).filter(|t| t.granted(&binding.classes)) {
        None => CallOutcome::Error(not_granted(name)),
        // A P6 desktop or capture tool (§6.1): bound, checked arguments, the
        // live switch, then one forward through the P6 path. It never
        // reaches `forward.call`, `fleet.call`, `to_op` or the outbox.
        Some(tool) if desktop_tools::is_desktop(tool.name) => {
            let out = match (binding.state, binding.caller()) {
                (BindingState::Active, Some(caller)) => match parse_request(tool.name, args) {
                    None => p6_error(code::INVALID_PARAMS, None, None),
                    Some(_) if !fleet_control => p6_error(code::NOT_GRANTED, None, None),
                    Some(request) => forward.call_desktop(caller, request).await,
                },
                _ => p6_error(code::NOT_BOUND, None, None),
            };
            CallOutcome::Desktop(out)
        }
        Some(tool) => match (binding.state, binding.caller()) {
            // A fleet-control grant (a new tool, or an ADR-006 tool under
            // such a grant) is never forwarded while the switch is off. The
            // session's own tools are not fleet control.
            (BindingState::Active, Some(_))
                if !fleet_control && binding.is_fleet_control() && !tool.is_own() =>
            {
                CallOutcome::Error(FLEET_CONTROL_OFF.to_owned())
            }
            // The fleet view is the station's own tool, but the deck answers
            // it (MCP P3 §3.1): one `fleet.view` for the binding's caller.
            (BindingState::Active, Some(caller)) if tool.name == "fleet_view" => {
                let outcome = match fleet_view_args(&args) {
                    Ok((view, cursor, station)) => forward
                        .fleet_view(caller, view, cursor, station)
                        .await
                        .and_then(super::mcp_fleet::page_value),
                    Err(e) => Err(e),
                };
                match outcome {
                    Ok(v) => CallOutcome::Value(v),
                    Err(e) => CallOutcome::Error(e),
                }
            }
            (BindingState::Active, Some(caller)) => {
                let outcome = if tool.is_own() {
                    match fleet {
                        Some(fleet) => fleet.call(caller, tool.name, args).await,
                        None => Err("unavailable: this station has no session tools".to_owned()),
                    }
                } else {
                    forward.call(caller, tool.name, args).await
                };
                match outcome {
                    Ok(v) => CallOutcome::Value(v),
                    Err(e) => CallOutcome::Error(e),
                }
            }
            _ => CallOutcome::Error(NOT_BOUND.to_owned()),
        },
    }
}

/// The most characters of the `cursor` argument of `fleet_view`.
const CURSOR_ARG_MAX: usize = 2048;
/// The most characters of the `station` argument of `fleet_view`.
const STATION_ARG_MAX: usize = 128;

/// The checked arguments of a `fleet_view` call: `view`, and the optional
/// `cursor` and `station`. No other key is accepted, so no argument can name
/// the caller.
pub(super) fn fleet_view_args(
    args: &Value,
) -> Result<(FleetView, Option<String>, Option<StationId>), String> {
    let Some(obj) = args.as_object() else {
        return Err("invalid_params: the arguments are not an object".to_owned());
    };
    if let Some(key) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "view" | "cursor" | "station"))
    {
        return Err(format!("invalid_params: unknown argument `{key}`"));
    }
    let view = match obj.get("view").and_then(Value::as_str) {
        Some("stations") => FleetView::Stations,
        Some("projects") => FleetView::Projects,
        Some("sessions") => FleetView::Sessions,
        Some("me") => FleetView::Me,
        _ => {
            return Err(
                "invalid_params: `view` is one of stations, projects, sessions, me".to_owned(),
            );
        }
    };
    let text = |key: &str, max: usize| -> Result<Option<String>, String> {
        match obj.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.chars().count() <= max => Ok(Some(s.clone())),
            Some(_) => Err(format!(
                "invalid_params: `{key}` is a text of at most {max} characters"
            )),
        }
    };
    let cursor = text("cursor", CURSOR_ARG_MAX)?;
    let station = text("station", STATION_ARG_MAX)?.map(StationId::from);
    Ok((view, cursor, station))
}
