//! The MCP P3 builders of the station (`docs/v2/design/mcp-p3.md` §3.1–§3.3,
//! §5.2): the resource list and the resource URIs, the prompts, the
//! generated `instructions` and the guide. Every function here is pure: it
//! takes the catalog names of a binding and the station facts, and it never
//! reads the deck or the clock.

use std::collections::BTreeSet;

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_view::{FleetView, FleetViewPage};
use rmcp::ErrorData as McpError;
use rmcp::model::{
    GetPromptResult, JsonObject, Prompt, PromptArgument, PromptMessage, Resource, Role,
};
use serde_json::{Value, json};

use super::bindings::Binding;
use super::tools;

/// The URI of the guide resource.
pub(super) const GUIDE_URI: &str = "fleet://guide";

/// How long a client may keep the guide text, in milliseconds (modern era;
/// the design names the hints of the views and of the lists only, and the
/// guide is a fixed text, so it gets the 60 000 of the lists).
pub(super) const GUIDE_TTL_MS: u64 = 60_000;

/// The sentence that the guide of a binding without a grant adds (§5.2).
const GUIDE_TAIL: &str =
    "To work on another station, call fleet_view and ask your coordinator or the owner.";

/// The names of the tools in the catalog of `binding`.
pub(super) fn catalog_names(binding: &Binding) -> BTreeSet<String> {
    tools::catalog(&binding.classes)
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The platform of a station (§2.3): `omarchy` on an Omarchy desktop; else
/// `macos` on macOS; else the operating system name.
#[must_use]
pub fn station_platform(omarchy: bool, os: &str) -> String {
    if omarchy {
        "omarchy".to_owned()
    } else {
        os.to_owned()
    }
}

// ---------------------------------------------------------------------------
// The guide (§5.2).
// ---------------------------------------------------------------------------

/// The guide text of a binding: for a grant binding the control skill of
/// the deck; for a binding without a grant the session-tools text plus one
/// sentence.
pub(super) fn guide(binding: &Binding) -> String {
    if binding.is_fleet_only() {
        format!("{} {GUIDE_TAIL}", binding.control_skill)
    } else {
        binding.control_skill.clone()
    }
}

// ---------------------------------------------------------------------------
// The resources (§3.1).
// ---------------------------------------------------------------------------

/// The five resources, in their fixed order.
pub(super) fn resources() -> Vec<Resource> {
    let entry = |uri: &str, name: &str, description: &str, mime: &str| {
        Resource::new(uri, name)
            .with_description(description)
            .with_mime_type(mime)
    };
    vec![
        entry(
            GUIDE_URI,
            "guide",
            "How to work across the fleet with the tools of this session.",
            "text/markdown",
        ),
        entry(
            "fleet://stations",
            "stations",
            "The stations of the fleet: names, platforms, online state, providers.",
            "application/json",
        ),
        entry(
            "fleet://projects",
            "projects",
            "The projects of the fleet; add ?station=<id> for one station.",
            "application/json",
        ),
        entry(
            "fleet://sessions",
            "sessions",
            "The live sessions of the fleet: titles and states.",
            "application/json",
        ),
        entry(
            "fleet://me",
            "me",
            "This session and its grant.",
            "application/json",
        ),
    ]
}

/// What a resource URI names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResourceRef {
    /// `fleet://guide`.
    Guide,
    /// A view with its optional query values.
    View {
        view: FleetView,
        cursor: Option<String>,
        station: Option<StationId>,
    },
}

impl ResourceRef {
    /// The `ttlMs` hint of a read of this resource (modern era, §3.1).
    pub(super) fn ttl_ms(&self) -> u64 {
        match self {
            Self::Guide => GUIDE_TTL_MS,
            Self::View {
                view: FleetView::Stations | FleetView::Projects,
                ..
            } => 30_000,
            Self::View { .. } => 10_000,
        }
    }
}

/// The resource of `uri`, or `None` for any other URI: another scheme, an
/// unknown name, a query on the guide or on `me`, a `station` query on any
/// view but `projects`, an unknown or repeated query key, an empty value or
/// a bad percent escape.
pub(super) fn parse_uri(uri: &str) -> Option<ResourceRef> {
    let rest = uri.strip_prefix("fleet://")?;
    let (name, query) = match rest.split_once('?') {
        Some((name, query)) => (name, Some(query)),
        None => (rest, None),
    };
    let view = match name {
        "guide" => {
            return query.is_none().then_some(ResourceRef::Guide);
        }
        "stations" => FleetView::Stations,
        "projects" => FleetView::Projects,
        "sessions" => FleetView::Sessions,
        "me" => FleetView::Me,
        _ => return None,
    };
    let (mut cursor, mut station) = (None, None);
    for pair in query.into_iter().flat_map(|q| q.split('&')) {
        let (key, value) = pair.split_once('=')?;
        let value = percent_decode(value).filter(|v| !v.is_empty())?;
        match key {
            "cursor" if view != FleetView::Me && cursor.is_none() => cursor = Some(value),
            "station" if view == FleetView::Projects && station.is_none() => {
                station = Some(StationId::from(value));
            }
            _ => return None,
        }
    }
    Some(ResourceRef::View {
        view,
        cursor,
        station,
    })
}

/// The text of a percent-encoded query value; `None` for a bad escape or
/// bytes that are not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = text.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The answer of a view as the compact JSON `{"items", "next_cursor"}`
/// (§3.1). A deck answer that is not a page is an `internal` error.
///
/// # Errors
/// The `internal:` text for an answer that does not parse as a page.
pub(super) fn page_value(answer: Value) -> Result<Value, String> {
    let page: FleetViewPage = serde_json::from_value(answer)
        .map_err(|_| "internal: the deck answered a fleet view that is not a page".to_owned())?;
    Ok(json!({"items": page.items, "next_cursor": page.next_cursor}))
}

/// The text of a resource read that failed: `{"error": "<text>"}`.
pub(super) fn error_text(text: &str) -> String {
    json!({"error": text}).to_string()
}

// ---------------------------------------------------------------------------
// The prompts (§3.2).
// ---------------------------------------------------------------------------

/// Why a prompt request fails.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PromptError {
    /// Not listed for this binding: `unknown prompt`.
    Unknown,
    /// A required argument is missing or an argument is not a string.
    Invalid(String),
}

impl From<PromptError> for McpError {
    fn from(error: PromptError) -> Self {
        match error {
            PromptError::Unknown => McpError::invalid_params("unknown prompt", None),
            PromptError::Invalid(text) => McpError::invalid_params(text, None),
        }
    }
}

const REPORT: &str = "Write a short report of what you did and what is open. Publish it with publish_artifact (name report.md), then set your card line with set_focus.";

/// Whether the catalog has every name of `names`.
fn has_all(catalog: &BTreeSet<String>, names: &[&str]) -> bool {
    names.iter().all(|n| catalog.contains(*n))
}

/// The prompts of a catalog, in the order of §3.2: a prompt is listed only
/// when the catalog has every tool that its text names.
pub(super) fn prompts(catalog: &BTreeSet<String>) -> Vec<Prompt> {
    let mut out = Vec::new();
    for name in ["report", "validate_on_platform", "delegate"] {
        if available(name, catalog) {
            out.push(describe(name));
        }
    }
    out
}

fn available(name: &str, catalog: &BTreeSet<String>) -> bool {
    match name {
        "report" => has_all(catalog, &["publish_artifact", "set_focus"]),
        "validate_on_platform" => {
            has_all(catalog, &["fleet_view", "assign", "wait_tasks"])
                && (catalog.contains("start_in_project") || catalog.contains("spawn"))
        }
        "delegate" => has_all(catalog, &["fleet_view", "assign", "wait_tasks"]),
        _ => false,
    }
}

fn describe(name: &str) -> Prompt {
    let argument = |name: &str, description: &str, required: bool| {
        PromptArgument::new(name)
            .with_description(description)
            .with_required(required)
    };
    match name {
        "report" => Prompt::new(
            "report",
            Some("Report what you did and what is open."),
            None,
        ),
        "validate_on_platform" => Prompt::new(
            "validate_on_platform",
            Some("Validate the current work on a station of another platform."),
            Some(vec![argument(
                "platform",
                "The platform to validate on, for example macos.",
                true,
            )]),
        ),
        _ => Prompt::new(
            "delegate",
            Some("Give a task to another session of the fleet."),
            Some(vec![
                argument("task", "The task to give.", true),
                argument("station", "The name of a station to prefer.", false),
            ]),
        ),
    }
}

/// The value of a string argument; an empty text counts as missing.
fn text_arg(args: &JsonObject, key: &str, required: bool) -> Result<Option<String>, PromptError> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        None | Some(Value::Null) | Some(Value::String(_)) if required => Err(PromptError::Invalid(
            format!("the argument `{key}` is required"),
        )),
        None | Some(Value::Null) | Some(Value::String(_)) => Ok(None),
        Some(_) => Err(PromptError::Invalid(format!(
            "the argument `{key}` is a string"
        ))),
    }
}

/// The answer of `prompts/get`.
///
/// # Errors
/// [`PromptError::Unknown`] for a prompt that the catalog does not allow,
/// and [`PromptError::Invalid`] for a missing or non-string argument.
pub(super) fn prompt(
    name: &str,
    args: &JsonObject,
    catalog: &BTreeSet<String>,
) -> Result<GetPromptResult, PromptError> {
    if !available(name, catalog) {
        return Err(PromptError::Unknown);
    }
    let text = match name {
        "report" => REPORT.to_owned(),
        "validate_on_platform" => {
            let platform = text_arg(args, "platform", true)?.unwrap_or_default();
            let starter = match (
                catalog.contains("start_in_project"),
                catalog.contains("spawn"),
            ) {
                (true, true) => "start_in_project (or spawn when there is no project)",
                (true, false) => "start_in_project",
                _ => "spawn",
            };
            format!(
                "Validate the current work on {platform}. Call fleet_view with view stations and pick an online station with platform {platform}. Start one session there with {starter}, assign it the validation task with assign, wait with wait_tasks, then read its report and tell me the result. If no such station is online, say so and stop."
            )
        }
        _ => {
            let task = text_arg(args, "task", true)?.unwrap_or_default();
            let station = text_arg(args, "station", false)?
                .map(|s| format!(" on station {s}"))
                .unwrap_or_default();
            let auto = if catalog.contains("assign_auto") {
                " (or use assign_auto)"
            } else {
                ""
            };
            format!(
                "Give this task to another session: {task}. Call fleet_view with view sessions and pick an idle session{station}, then assign the task with assign{auto}. Wait with wait_tasks and report the result."
            )
        }
    };
    Ok(GetPromptResult::new(vec![PromptMessage::new_text(
        Role::User,
        text,
    )]))
}

// ---------------------------------------------------------------------------
// The instructions (§3.3).
// ---------------------------------------------------------------------------

/// The `instructions` of a binding: the lines of §3.3 whose condition holds,
/// in order. `station` is the name and the platform of this station.
pub(super) fn instructions(binding: &Binding, station: Option<&(String, String)>) -> String {
    let catalog = catalog_names(binding);
    let has = |name: &str| catalog.contains(name);
    let mut lines: Vec<String> = Vec::new();
    lines.push(match station {
        Some((name, platform)) => {
            format!("You are a Fleet Deck session on station {name} ({platform}).")
        }
        None => "You are a Fleet Deck session.".to_owned(),
    });
    lines.push(
        "Call fleet_view to see the stations and their platforms (view stations), the live sessions (view sessions), and yourself and your grant (view me)."
            .to_owned(),
    );
    lines.push(verbs(&has));
    lines.push(
        "Publish reports with publish_artifact and keep your card line current with set_focus."
            .to_owned(),
    );
    lines.push(
        "Never ask for, print or pass on a credential. A refused call (forbidden, expired, revoked) is final: do not retry it in a loop; tell the owner what you need. Use only the tools in your tool list."
            .to_owned(),
    );
    if has("command_status") {
        lines.push(
            "A call that answers pending is not finished: check it with command_status.".to_owned(),
        );
    }
    if !binding.is_fleet_only() {
        lines.push(
            "Read fleet://guide (or call fleet_view and read the guide resource) before you coordinate other sessions."
                .to_owned(),
        );
    }
    lines.join(" ")
}

/// Line 3 of §3.3: the verbs that the catalog allows.
fn verbs(has: &dyn Fn(&str) -> bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    match (has("spawn"), has("start_in_project")) {
        (true, true) => parts.push("start sessions with spawn or start_in_project".to_owned()),
        (true, false) => parts.push("start sessions with spawn".to_owned()),
        (false, true) => parts.push("start sessions with start_in_project".to_owned()),
        (false, false) => {}
    }
    match (has("assign"), has("wait_tasks")) {
        (true, true) => parts.push("give work with assign and wait with wait_tasks".to_owned()),
        (true, false) => parts.push("give work with assign".to_owned()),
        (false, true) => parts.push("wait for tasks with wait_tasks".to_owned()),
        (false, false) => {}
    }
    if has("stop") {
        parts.push("stop the sessions that you started with stop".to_owned());
    }
    if parts.is_empty() {
        "To get help from another station, ask your coordinator with ask_coordinator or tell the owner."
            .to_owned()
    } else {
        format!("You may {}.", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uri_parser_takes_the_five_resources_and_their_queries_only() {
        assert_eq!(parse_uri("fleet://guide"), Some(ResourceRef::Guide));
        assert_eq!(
            parse_uri("fleet://projects?station=st-1&cursor=abc"),
            Some(ResourceRef::View {
                view: FleetView::Projects,
                cursor: Some("abc".to_owned()),
                station: Some("st-1".into()),
            })
        );
        assert_eq!(
            parse_uri("fleet://projects?station=a%20b").map(|r| match r {
                ResourceRef::View { station, .. } => station.map(|s| s.to_string()),
                ResourceRef::Guide => None,
            }),
            Some(Some("a b".to_owned()))
        );
        for bad in [
            "fleet://",
            "fleet://other",
            "file://stations",
            "fleet://guide?cursor=x",
            "fleet://me?cursor=x",
            "fleet://stations?station=x",
            "fleet://stations?cursor=",
            "fleet://stations?cursor=a&cursor=b",
            "fleet://stations?x=1",
            "fleet://stations?cursor",
            "fleet://stations?cursor=%zz",
            "fleet://stations/",
        ] {
            assert_eq!(parse_uri(bad), None, "{bad}");
        }
    }
}
