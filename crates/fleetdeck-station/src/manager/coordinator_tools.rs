//! The session tools of a start (DL-165, DL-194): the fleet-only handoff
//! and the choice of the tools of one start, moved out of
//! `coordinator_start.rs`; no behaviour change.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{GrantKind, GrantScope, SessionSpec};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::coordinator::CoordinatorLaunch;

use super::coordinator_start::{
    BridgeStart, NOT_NEW_CHILD, RecordStart, prepare, prepare_bridge, with_skill,
};
use crate::coordinator::{Binding, BindingState, Bindings, Handoff, ToolClass};

/// The pending binding of a session without a grant (DL-165).
fn fleet_binding(session_id: &SessionId, spec: &SessionSpec) -> Binding {
    Binding {
        binding_id: format!("{session_id}:{FLEET_GRANT}"),
        grant_id: String::new(),
        session_id: session_id.clone(),
        provider_instance: spec.provider_instance.to_string(),
        native_id: None,
        generation: 0,
        classes: std::iter::once(ToolClass::Own).collect(),
        scope: GrantScope::Lineage,
        state: BindingState::Pending,
        control_skill: FLEET_SKILL.to_owned(),
        kind: GrantKind::Coordinator,
    }
}

/// Why a session has no session tools (DL-165). The text goes to the
/// station log; it never holds a credential.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NoTools {
    /// A shim-hosted session has no handoff path.
    Durable,
    /// The provider version has no verified handoff (the probed versions).
    Unverified(String),
    /// The ingress does not listen.
    NoEndpoint,
    /// The provider (or its adapter) refused the handoff.
    Refused,
}

impl std::fmt::Display for NoTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Durable => f.write_str("durable host"),
            Self::Unverified(versions) => write!(
                f,
                "provider version not verified for the handoff ({versions})"
            ),
            Self::NoEndpoint => f.write_str("no tool endpoint"),
            Self::Refused => f.write_str("the provider refused the handoff"),
        }
    }
}

/// The fleet-only handoff of a session that has no coordinator grant
/// (DL-165): the session gets the loopback MCP endpoint and a credential
/// for its own tools (`publish_artifact`, `set_focus`, `ask_coordinator`).
/// A start and a resume get it alike (a resume replaces the old binding of
/// the same id).
///
/// # Errors
/// [`NoTools`] when the session cannot have it: a shim-hosted start has no
/// handoff path, the provider version has no verified handoff, the
/// endpoint does not listen, or the provider refuses. The session then
/// starts without the tools; that is not an error of the start.
pub(super) fn prepare_fleet(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    durable: bool,
) -> Result<CoordinatorLaunch, NoTools> {
    if durable {
        return Err(NoTools::Durable);
    }
    if bindings.handoff(provider, None).is_err() {
        return Err(unverified(bindings, provider));
    }
    let url = bindings.endpoint().ok_or(NoTools::NoEndpoint)?;
    let handoff = Handoff::prepare(provider, &url).map_err(|_| NoTools::Refused)?;
    bindings.insert(handoff.digest.clone(), fleet_binding(session_id, spec));
    Ok(with_skill(
        bindings,
        provider,
        session_id,
        handoff.into_launch(),
    ))
}

/// [`NoTools::Unverified`] with the probed versions of `provider`.
fn unverified(bindings: &Bindings, provider: ProviderKind) -> NoTools {
    let versions = bindings.versions(provider);
    NoTools::Unverified(if versions.is_empty() {
        "unknown".to_owned()
    } else {
        versions.join(", ")
    })
}

/// The bridge form of [`prepare_fleet`] (DL-194): no memory change.
///
/// # Errors
/// [`NoTools`] when the session cannot have the tools.
pub(super) fn prepare_bridge_fleet(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
) -> Result<BridgeStart, NoTools> {
    if bindings.handoff(provider, None).is_err() {
        return Err(unverified(bindings, provider));
    }
    let bridge = bindings.bridge().ok_or(NoTools::NoEndpoint)?;
    let handoff = Handoff::prepare_bridge(provider, &bridge).map_err(|_| NoTools::Refused)?;
    let digest = handoff.digest.clone();
    Ok(BridgeStart {
        launch: with_skill(bindings, provider, session_id, handoff.into_launch()),
        digest,
        binding: fleet_binding(session_id, spec),
        expires_at: None,
        provider,
        grant_id: None,
        start_command_id: None,
    })
}

/// The tools that one start gets (DL-194).
#[allow(
    clippy::large_enum_variant,
    reason = "one value per start; the shape is fixed by the brief"
)]
pub(super) enum Tools {
    /// No tools; the session starts without them.
    None,
    /// The loopback endpoint of the direct host; memory holds the binding.
    Direct(CoordinatorLaunch),
    /// The bridge of the shim host; the launch stores the binding.
    Bridge(BridgeStart),
}

/// `tools_for_start` without the skip, for the tests (DL-165, DL-194).
///
/// # Errors
/// The reason that a coordinator start fails with. A session without a
/// grant never fails here: it gets [`Tools::None`] and a log line.
#[cfg(test)]
pub(super) fn tools_for(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    shim_host: bool,
    resumed: bool,
) -> Result<Tools, String> {
    let start = super::worker_grant::tools_for_start;
    start(
        bindings, provider, session_id, spec, shim_host, resumed, None, None,
    )
    .map(|(t, _)| t)
}

/// True when the bridge mode is available for `provider`: Linux and a probed
/// version list in which every version passed the live bridge smoke (§9.10).
pub(super) fn bridge_verified(bindings: &Bindings, provider: ProviderKind) -> bool {
    let versions = bindings.versions(provider);
    cfg!(target_os = "linux")
        && !versions.is_empty()
        && versions
            .iter()
            .all(|v| fleetdeck_providers::coordinator::bridge_handoff(provider, Some(v)).is_ok())
}

/// [`tools_for`] without the worker rules: the owner grant and the
/// fleet-only forms. A start that `record` prepared (MCP P4b §5) is not
/// refused as a resume.
///
/// # Errors
/// The reason that an owner grant start fails with.
pub(super) fn tools_generic(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    shim_host: bool,
    resumed: bool,
    record: Option<&RecordStart>,
) -> Result<Tools, String> {
    let bridge_ok = bridge_verified(bindings, provider);
    let Some(coordinator) = spec.coordinator.as_deref() else {
        let chosen = if !shim_host {
            prepare_fleet(bindings, provider, session_id, spec, false).map(Tools::Direct)
        } else if bridge_ok {
            prepare_bridge_fleet(bindings, provider, session_id, spec).map(Tools::Bridge)
        } else {
            Err(NoTools::Durable)
        };
        return Ok(chosen.unwrap_or_else(|why| {
            log_no_tools(session_id, provider, &why);
            Tools::None
        }));
    };
    if resumed && record.is_none() {
        return Err(NOT_NEW_CHILD.to_owned());
    }
    if !shim_host {
        return prepare(
            bindings,
            provider,
            session_id,
            spec,
            coordinator,
            false,
            record,
        )
        .map(Tools::Direct);
    }
    if !bridge_ok {
        return Err(
            "a coordinator session on the shim host needs a provider version whose bridge is verified"
                .to_owned(),
        );
    }
    prepare_bridge(bindings, provider, session_id, spec, coordinator, record).map(Tools::Bridge)
}

/// The info line of a session that got its tools (a start or a resume).
pub(super) fn log_attached(session_id: &SessionId, provider: ProviderKind) {
    tracing::info!(session = %session_id, provider = ?provider, "session tools attached");
}

/// The info line of a session that has no tools, with the reason.
pub(super) fn log_no_tools(session_id: &SessionId, provider: ProviderKind, why: &NoTools) {
    tracing::info!(session = %session_id, provider = ?provider, "no session tools: {why}");
}

/// The name part of a fleet-only binding id.
pub(super) const FLEET_GRANT: &str = "fleet";

/// The MCP `instructions` of a session without a coordinator grant.
pub(super) const FLEET_SKILL: &str = "Fleet Deck session tools. `publish_artifact` keeps a file of your \
workspace (an HTML page, Markdown, an image, SVG or PDF) as a versioned artifact that the owner \
opens in Fleet Deck. To show a page or a document to the owner, call publish_artifact. When you \
cannot write files (for example in dontAsk mode), pass the page as `content` with a `name` such as \
report.html. To update a page, publish again with its artifact_id. `set_focus` sets the one line (at most 120 characters) on your card. \
`ask_coordinator` asks the coordinator of your task a question. The tools need no credential.";
