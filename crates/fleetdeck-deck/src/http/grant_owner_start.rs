//! The owner's coordinator start (ADR-006 §4, §7.1; DL-217), moved out of
//! `grant_routes.rs`; no behaviour change.

use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_text::limits;
use fleetdeck_proto::coordinator_spec::{GrantKind, InputScope};
use fleetdeck_proto::version::feature;

use super::Board;
use super::grant_routes::{CONTROL_SKILL, CONTROL_SKILL_VERSION, MAX_GRANT_STATIONS};
use super::guard::ApiError;
use super::settings_routes::is_elevated;
use crate::store::auth::LoginSession;
use crate::store::{AuditResult, GrantClass, LinkFeature, LinkV2, NewAudit};

/// The owner's coordinator start (ADR-006 §4, §7.1; DL-217): the board's
/// `session.start` with a `coordinator` request creates the grant on the
/// deck in this call, and the deck fills in the grant id, the expiry and
/// its control skill. Whatever grant id or skill the board sent is
/// replaced. A resend of the same `command_id` finds the same grant.
///
/// The grant's stations are the coordinator's own station and the target
/// stations of `spec.stations`; the deck takes that list out of the spec, so
/// the start command that goes to a station never carries it. A fleet-control
/// grant (the fleet scope, or the class `inspect`, `manage` or `answer`) or a
/// grant with a station other than its own needs the owner's elevation in the
/// same request, and every station of the grant must have negotiated
/// `coordinator.v2` on their current link (ADR-008 section 3). The classes
/// `desktop` and `capture` are fleet control: besides that, the caller's link
/// and every other station of the grant need the features of R13 P6 §5.1
/// (the caller's own desktop is checked when a call names it). The three desktop
/// caps are read from the spec and never go on to a station.
///
/// # Errors
/// 400 `invalid_grant` for an unknown class, no class, an unknown station, more
/// than 16 stations, an answer cap or a desktop cap out of range or without
/// its class, or a limit out of range; 400 `elevation_required`; 409
/// `unsupported`; 500 for a database error.
pub(super) async fn owner_start(
    board: &Board,
    station: &fleetdeck_proto::StationId,
    command_id: &fleetdeck_proto::CommandId,
    spec: &mut fleetdeck_proto::command::CoordinatorSpec,
    (session, elevation): (&LoginSession, Option<&str>),
) -> Result<(), ApiError> {
    let invalid = |m: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_grant", m);
    // The grant kind is set by the deck (MCP P4a §4): a board request for a
    // worker grant is refused, and the outgoing kind comes from the stored grant.
    if spec.kind != GrantKind::Coordinator {
        return Err(invalid("the grant kind is set by the deck".to_owned()));
    }
    // The board-to-deck list never goes on to a station.
    let targets = std::mem::take(&mut spec.stations);
    // The desktop caps are read here and never go on to a station either
    // (R13 P6 item 18 of decision O12-4).
    let (desktop_cap, capture_cap, image_byte_cap) = (
        spec.desktop_cap.take(),
        spec.capture_cap.take(),
        spec.image_byte_cap.take(),
    );
    // The recording opt-in and caps (R13 P5b-2 §13.2) are read here too.
    let allow_recording = std::mem::take(&mut spec.allow_recording);
    let (recording_cap, recording_second_cap) =
        (spec.recording_cap.take(), spec.recording_second_cap.take());
    // The input opt-in, scope and caps (R13 P7 §3.1, §3.2) are read here too.
    let allow_input = std::mem::take(&mut spec.allow_input);
    // R13 P7b §3.1: the click opt-in is read here too.
    let allow_click = std::mem::take(&mut spec.allow_click);
    let (input_scope, input_cap, input_character_cap) = (
        spec.input_scope.take(),
        spec.input_cap.take(),
        spec.input_character_cap.take(),
    );
    let mut classes = std::collections::BTreeSet::new();
    for name in &spec.classes {
        let class: GrantClass = serde_json::from_value(serde_json::Value::String(name.clone()))
            .map_err(|_| invalid(format!("unknown grant class `{name}`")))?;
        classes.insert(class);
    }
    if classes.is_empty() {
        return Err(invalid("a coordinator grant needs a class".to_owned()));
    }
    // MCP P0 §2.2: the spawn cap and the mode ceilings, rules 1 to 5.
    let grant_limits = board.live.get().knobs.grant;
    let (spawn_cap, max_modes) =
        super::grant_spawn::take_spawn_fields(spec, &classes, &grant_limits).map_err(invalid)?;
    for target in &targets {
        let paired = board
            .store
            .station(target.clone())
            .await
            .map_err(ApiError::internal)?;
        if paired.is_none() {
            return Err(invalid(format!("Unknown station {target}.")));
        }
    }
    let mut grant = crate::store::NewGrant::new(station.clone(), "".into());
    grant.stations.extend(targets);
    if grant.stations.len() > MAX_GRANT_STATIONS {
        return Err(invalid(format!(
            "A grant has at most {MAX_GRANT_STATIONS} stations."
        )));
    }
    let widened = grant.stations.len() > 1;
    grant.classes = classes;
    if grant.classes.contains(&GrantClass::Spawn) {
        grant.spawn_cap = spawn_cap
            .unwrap_or_else(|| crate::store::grants::grant_rules::default_spawn_cap(&grant_limits));
    }
    grant.max_modes = max_modes;
    grant.scope = spec.scope;
    grant.answer_cap = spec.answer_cap;
    grant.desktop_cap = desktop_cap;
    grant.capture_cap = capture_cap;
    grant.image_byte_cap = image_byte_cap;
    // The recording rules, for every request (a recording flag on a grant
    // without the `capture` class is refused even when the grant is not
    // fleet control): the class, the opt-in, the ranges; then the defaults.
    if allow_recording && !grant.classes.contains(&GrantClass::Capture) {
        return Err(invalid(
            "allow_recording needs the capture class".to_owned(),
        ));
    }
    if !allow_recording && (recording_cap.is_some() || recording_second_cap.is_some()) {
        return Err(invalid("recording caps need allow_recording".to_owned()));
    }
    if allow_recording {
        grant.recording_cap = recording_cap.unwrap_or(limits::RECORDING_CAP_DEFAULT);
        grant.recording_second_cap =
            recording_second_cap.unwrap_or(limits::RECORDING_SECOND_CAP_DEFAULT);
    }
    crate::store::grants::check_recording_caps(
        &grant.classes,
        grant.recording_cap,
        grant.recording_second_cap,
    )
    .map_err(|m| invalid(m.to_owned()))?;
    // The input rules, for every request, before the elevation: an explicit
    // scope or cap without the opt-in is invalid; with the opt-in the scope is
    // `own_windows` and the caps have the defaults of §9.1.
    if !allow_input && input_scope.is_some() {
        return Err(invalid("input_scope needs allow_input".to_owned()));
    }
    if !allow_input && (input_cap.is_some() || input_character_cap.is_some()) {
        return Err(invalid("input caps need allow_input".to_owned()));
    }
    if allow_click && !allow_input {
        return Err(invalid("allow_click needs allow_input".to_owned()));
    }
    if allow_input {
        grant.allow_input = true;
        grant.allow_click = allow_click;
        grant.input_scope = Some(input_scope.unwrap_or(InputScope::OwnWindows));
        grant.input_cap = input_cap.unwrap_or(limits::INPUT_CAP_DEFAULT);
        grant.input_character_cap =
            input_character_cap.unwrap_or(limits::INPUT_CHARACTER_CAP_DEFAULT);
    }
    let input_caps = (grant.input_cap, grant.input_character_cap);
    crate::store::grants::check_input_grant(
        &grant.classes,
        (grant.allow_input, grant.allow_click),
        grant.input_scope,
        input_caps,
    )
    .map_err(|m| invalid(m.to_owned()))?;
    grant.start_command_id = Some(command_id.to_string());
    if spec.idle_ttl_ms > 0 {
        grant.idle_ttl_ms = i64::try_from(spec.idle_ttl_ms).unwrap_or(i64::MAX);
    }
    if spec.expires_at > 0 {
        grant.ttl_ms = spec.expires_at.saturating_sub(board.store.now_ms());
    }
    let fleet = spec.scope == fleetdeck_proto::command::GrantScope::Fleet
        || grant.classes.iter().any(|c| c.is_fleet_control());
    if grant.classes.contains(&GrantClass::Spawn)
        && !(fleet || widened)
        && !is_elevated(board, session, elevation)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "elevation_required",
            "a grant that starts sessions needs the owner's elevation".to_owned(),
        ));
    }
    if fleet || widened {
        // The answer cap is checked first: a bad request is a bad request
        // with or without elevation.
        // An omitted cap is resolved to the limit before the check (B27).
        let answers = grant.classes.contains(&GrantClass::Answer);
        match grant.answer_cap {
            Some(_) if !answers => {
                return Err(invalid("answer_cap needs the answer class".to_owned()));
            }
            Some(cap) => crate::store::grants::grant_rules::check_answer_cap(cap, &grant_limits)
                .map_err(|m| invalid(m.to_owned()))?,
            None => {}
        }
        // The desktop caps, before the elevation, like the answer cap.
        crate::store::grants::check_desktop_caps(
            &grant.classes,
            grant.desktop_cap,
            grant.capture_cap,
            grant.image_byte_cap,
        )
        .map_err(|m| invalid(m.to_owned()))?;
        if !is_elevated(board, session, elevation) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "elevation_required",
                "a fleet-control grant needs the owner's elevation".to_owned(),
            ));
        }
        // Every station of the grant: the coordinator's own and the targets.
        for st in &grant.stations {
            if board.store.link_v2(st) != LinkV2::Negotiated {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "unsupported",
                    format!("station `{st}` has no link that negotiated coordinator.v2"),
                ));
            }
        }
        super::grant_spawn::desktop_features(
            board,
            station,
            &grant.classes,
            &grant.stations,
            (
                allow_recording,
                allow_input,
                grant.allow_click,
                grant.input_scope == Some(InputScope::AnyWindow),
            ),
        )?;
    }
    let g = board.store.create_grant(grant).await.map_err(|e| match e {
        crate::store::DeckError::Invalid(m) => invalid(m),
        e => ApiError::internal(e),
    })?;
    // Each station of the grant relays coordinator calls with the link scope
    // `coordinate` that the grant added; the link's grant cache follows
    // (DL-211).
    board
        .link
        .peers
        .refresh_changed()
        .await
        .map_err(ApiError::internal)?;
    let named = g
        .stations
        .iter()
        .map(StationId::as_str)
        .collect::<Vec<_>>()
        .join(",");
    board
        .store
        .audit_or_log(
            NewAudit::new("owner", "coordinator.grant_created", AuditResult::Ok)
                .target(g.grant_id.clone())
                .command(command_id.clone())
                .detail(super::grant_spawn::audit_detail(&g, &named)),
        )
        .await;
    // R13 P5b-2 (decision O12-22, §14 item 13): the flag goes on to the
    // caller station only when its link negotiated
    // `capture_recording_mcp.v1`; the two caps never go on. A station that
    // lacks the feature never sees the flag.
    spec.allow_recording = allow_recording
        && board
            .store
            .link_feature(station, feature::CAPTURE_RECORDING_MCP)
            == LinkFeature::Negotiated;
    // R13 P7 §3.2: the opt-in and its scope go on to the caller station only
    // when its link negotiated `desktop_input_mcp.v1`; the two caps never go
    // on. A station that lacks the feature never sees the fields.
    let input_link = allow_input
        && board
            .store
            .link_feature(station, feature::DESKTOP_INPUT_MCP)
            == LinkFeature::Negotiated;
    spec.allow_input = input_link;
    // R13 P7b §3.1: the click flag goes on only to a caller link with
    // `desktop_click_mcp.v1`, and the scope `any_window` only to one with
    // `desktop_input_any.v1` (the feature check above refused otherwise).
    spec.allow_click = g.allow_click
        && input_link
        && board
            .store
            .link_feature(station, feature::DESKTOP_CLICK_MCP)
            == LinkFeature::Negotiated;
    spec.input_scope = g.input_scope.filter(|scope| {
        input_link
            && (*scope != InputScope::AnyWindow
                || board
                    .store
                    .link_feature(station, feature::DESKTOP_INPUT_ANY)
                    == LinkFeature::Negotiated)
    });
    spec.kind = g.kind;
    spec.grant_id = g.grant_id;
    spec.idle_ttl_ms = u64::try_from(g.idle_ttl_ms).unwrap_or(0);
    spec.expires_at = g.expires_at;
    spec.classes = view_classes(&g.classes);
    spec.scope = g.scope;
    spec.answer_cap = g.answer_cap;
    spec.control_skill = fleetdeck_proto::command::ControlSkill {
        version: CONTROL_SKILL_VERSION.to_owned(),
        text: CONTROL_SKILL.to_owned(),
    };
    Ok(())
}

fn view_classes(classes: &std::collections::BTreeSet<GrantClass>) -> Vec<String> {
    classes
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}
