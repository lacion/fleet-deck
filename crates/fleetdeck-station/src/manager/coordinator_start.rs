//! The coordinator part of a start effect (ADR-006 §5.4, DL-216).
//!
//! The start effect of a session whose spec carries a grant mints the
//! credential, adds a `pending` binding (generation 0) under its digest
//! and gives the adapter the arguments and the credential. The station
//! checks the handoff again here: a provider without a verified handoff,
//! a missing ingress and a resume fail the start instead of running a
//! coordinator session without its credential. A session without a grant
//! gets the session's own tools on a start and on a resume when it can
//! (DL-165). On the shim host the tools come through the MCP bridge, and
//! the launch stores the binding row before it adds the binding to memory
//! (DL-194).

use std::ffi::OsString;

use fleetdeck_proto::command::{CoordinatorSpec, GrantScope, SessionSpec};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, SessionId};
use fleetdeck_providers::coordinator::CoordinatorLaunch;
use fleetdeck_providers::host::ProcessControl as _;
use fleetdeck_providers::host::shim::host::Attached;
use fleetdeck_providers::host::shim::start::LaunchSecrets;

use crate::coordinator::{Binding, BindingState, Bindings, Handoff, ToolClass};
use crate::store::shim_bindings::{BindingRow, RowState};
use crate::store::{NewEvent, Store};

/// The reason of `coordinator.ended` at the end of the process (§5.5).
pub(super) const REASON_PROCESS_EXIT: &str = "process_exit";

/// The binding facts of a stored event batch (DL-217): the native
/// identity of `session.started` binds each pending binding of the
/// session (`coordinator.bound`, generation 1), or revokes an active
/// binding whose root identity changed (`coordinator.ended`).
pub(super) fn bound_events(
    bindings: &Bindings,
    session_id: &SessionId,
    observed: &[Event],
) -> Vec<NewEvent> {
    let mut out = Vec::new();
    for event in observed {
        let Event::SessionStarted { provider_refs, .. } = event else {
            continue;
        };
        let Some(native) = provider_refs
            .get("session_id")
            .or_else(|| provider_refs.get("thread_id"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        for b in bindings.session_native(session_id, native) {
            // The binding of a session without a grant is not a
            // coordinator binding: the deck knows nothing of it.
            if b.is_fleet_only() {
                continue;
            }
            out.push(NewEvent::from(match b.state {
                BindingState::Active => Event::CoordinatorBound {
                    binding_id: b.binding_id,
                    grant_id: b.grant_id,
                    generation: b.generation,
                    provider_instance: b.provider_instance,
                    native_id: native.to_owned(),
                },
                _ => Event::CoordinatorEnded {
                    binding_id: b.binding_id,
                    generation: b.generation,
                    reason: "revoked".to_owned(),
                },
            }));
        }
    }
    out
}

/// Revokes the bindings of a session whose process ended and returns
/// their `coordinator.ended` events (DL-217).
pub(super) fn ended_events(bindings: &Bindings, session_id: &SessionId) -> Vec<NewEvent> {
    bindings
        .end_session(session_id)
        .into_iter()
        .filter(|b| !b.is_fleet_only())
        .map(|b| {
            NewEvent::from(Event::CoordinatorEnded {
                binding_id: b.binding_id,
                generation: b.generation,
                reason: REASON_PROCESS_EXIT.to_owned(),
            })
        })
        .collect()
}

/// Prepares the handoff of one start and adds its pending binding.
///
/// # Errors
/// The reason that the start fails with.
pub(super) fn prepare(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    coordinator: &CoordinatorSpec,
    resumed: bool,
) -> Result<CoordinatorLaunch, String> {
    if resumed {
        return Err(NOT_NEW_CHILD.to_owned());
    }
    bindings.handoff(provider, None).map_err(str::to_owned)?;
    let url = bindings
        .endpoint()
        .ok_or_else(|| "the coordinator endpoint does not listen".to_owned())?;
    let classes = grant_classes(coordinator)?;
    let handoff = Handoff::prepare(provider, &url).map_err(|e| e.to_string())?;
    bindings.insert(
        handoff.digest.clone(),
        grant_binding(session_id, spec, coordinator, classes),
    );
    Ok(handoff.into_launch())
}

/// The refusal of a coordinator start that is not a new child.
const NOT_NEW_CHILD: &str = "a coordinator session starts only as a new direct child";

/// The tool classes of a grant, with the session's own tools (DL-165). The
/// station-internal `recording` class (R13 P5b-2 §13.5) is added only when
/// the spec has `allow_recording` and the `capture` class; a spec that names
/// `recording` itself is refused as an unknown class.
fn grant_classes(
    coordinator: &CoordinatorSpec,
) -> Result<std::collections::BTreeSet<ToolClass>, String> {
    let mut classes: std::collections::BTreeSet<ToolClass> = coordinator
        .classes
        .iter()
        .map(|c| ToolClass::parse(c).ok_or_else(|| format!("unknown grant class `{c}`")))
        .collect::<Result<_, _>>()?;
    // A coordinator session has the session's own tools too (DL-165).
    classes.insert(ToolClass::Own);
    if coordinator.allow_recording && classes.contains(&ToolClass::Capture) {
        classes.insert(ToolClass::Recording);
    }
    Ok(classes)
}

/// The pending binding of a coordinator grant.
fn grant_binding(
    session_id: &SessionId,
    spec: &SessionSpec,
    coordinator: &CoordinatorSpec,
    classes: std::collections::BTreeSet<ToolClass>,
) -> Binding {
    Binding {
        binding_id: format!("{session_id}:{}", coordinator.grant_id),
        grant_id: coordinator.grant_id.clone(),
        session_id: session_id.clone(),
        provider_instance: spec.provider_instance.to_string(),
        native_id: None,
        generation: 0,
        classes,
        scope: coordinator.scope,
        state: BindingState::Pending,
        control_skill: coordinator.control_skill.text.clone(),
    }
}

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
    }
}

/// A start on the shim host with its tools through the bridge (DL-194).
/// Memory holds nothing yet: the launch stores the row first, then adds
/// the binding.
pub(crate) struct BridgeStart {
    /// The bridge arguments and the token.
    pub launch: CoordinatorLaunch,
    /// The digest of the token.
    pub digest: String,
    /// The `Pending` binding (generation 0).
    pub binding: Binding,
    /// The grant expiry for the row; `None` for a fleet-only binding.
    pub expires_at: Option<i64>,
    /// The provider, for the log line after the launch.
    pub provider: ProviderKind,
}

/// The bridge form of [`prepare`]: it mints the credential and builds the
/// binding but never changes memory (DL-194 §2.4).
///
/// # Errors
/// The reason that the start fails with.
pub(super) fn prepare_bridge(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    coordinator: &CoordinatorSpec,
) -> Result<BridgeStart, String> {
    bindings.handoff(provider, None).map_err(str::to_owned)?;
    let bridge = bindings
        .bridge()
        .ok_or_else(|| "the coordinator bridge does not listen".to_owned())?;
    let classes = grant_classes(coordinator)?;
    let handoff = Handoff::prepare_bridge(provider, &bridge).map_err(|e| e.to_string())?;
    let digest = handoff.digest.clone();
    Ok(BridgeStart {
        launch: handoff.into_launch(),
        digest,
        binding: grant_binding(session_id, spec, coordinator, classes),
        expires_at: Some(coordinator.expires_at),
        provider,
    })
}

/// Stores the binding row of a launch with tools and adds the binding to
/// memory, in this order (DL-194 §2.3). Returns the launch secrets and the
/// stored digest with its provider. A row that cannot be stored gives
/// neither: the session starts without tools.
pub(super) async fn store_tools(
    store: &Store,
    bindings: &Bindings,
    shim_id: &str,
    tools: Option<BridgeStart>,
) -> (
    Option<(LaunchSecrets, ProviderKind)>,
    Option<(String, ProviderKind)>,
) {
    let Some(t) = tools else {
        return (None, None);
    };
    let row = BindingRow::from_binding(
        t.digest.clone(),
        shim_id.to_owned(),
        &t.binding,
        t.expires_at,
        store.now_ms(),
    );
    if let Err(error) = store.shim_binding_insert(row).await {
        tracing::warn!(%error, "the binding row was not stored; the session starts without tools");
        return (None, None);
    }
    bindings.insert(t.digest.clone(), t.binding);
    let secrets = LaunchSecrets {
        token: t.launch.token,
        extra_args: t.launch.args.into_iter().map(OsString::from).collect(),
    };
    (Some((secrets, t.provider)), Some((t.digest, t.provider)))
}

/// Refuses the token of a stored row and ends the row, after a launch
/// failed (DL-194 §2.3). In order: the memory gate, a fence entry, the row
/// end, and the removal of the entry when both writes worked. Returns
/// `true` when the invalidation is durable (the fence entry or the row end
/// was written) or when there is nothing to do; `false` when both writes
/// failed, so a restart could load the still-open row (DL-194).
pub(super) async fn revoke_stored(
    store: &Store,
    bindings: &Bindings,
    shim_id: &str,
    stored: Option<&(String, ProviderKind)>,
) -> bool {
    let Some((digest, _)) = stored else {
        return true;
    };
    bindings.update_digest(digest, |b| b.state = BindingState::Revoked);
    let entry = (shim_id.to_owned(), digest.clone());
    let fence = bindings.fence();
    let fence_ok = match &fence {
        Some(f) => match f.add(std::slice::from_ref(&entry), "launch") {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "the fence entry of a failed launch was not written");
                false
            }
        },
        None => false,
    };
    let row_ok = match store
        .shim_binding_set_state(digest.clone(), RowState::Ended)
        .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(%error, "the binding row of a failed launch was not ended");
            false
        }
    };
    if let (true, true, Some(f)) = (fence_ok, row_ok, &fence)
        && let Err(error) = f.remove(std::slice::from_ref(&entry))
    {
        tracing::warn!(%error, "the fence entry of a failed launch was not removed");
    }
    if !fence_ok && !row_ok {
        tracing::error!(
            shim = shim_id,
            "the invalidation of a failed launch is not durable"
        );
    }
    fence_ok || row_ok
}

/// The settlement of a failed launch when the cleanup of the credential was
/// not durable: a settlement that reattached must not continue with that
/// credential, so `kill` stops the provider and the launch fails (DL-194).
/// `durable` is the result of [`revoke_stored`].
pub(super) async fn unless_unsaved<A, K, F>(
    durable: bool,
    settled: Result<A, super::shims::LaunchError>,
    kill: K,
) -> Result<A, super::shims::LaunchError>
where
    K: FnOnce(A) -> F,
    F: std::future::Future<Output = ()>,
{
    match settled {
        Ok(attached) if !durable => {
            kill(attached).await;
            Err(super::shims::LaunchError::Settled {
                error: "the session credentials could not be saved".to_owned(),
                settled: super::shims::reason::EXECUTION_UNKNOWN,
            })
        }
        other => other,
    }
}

/// [`unless_unsaved`] for an attached shim: it kills the provider.
pub(super) async fn kill_unless_saved(
    durable: bool,
    settled: Result<Attached, super::shims::LaunchError>,
) -> Result<Attached, super::shims::LaunchError> {
    unless_unsaved(durable, settled, |attached| async move {
        attached.process.kill().await;
    })
    .await
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
    Ok(handoff.into_launch())
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
        launch: handoff.into_launch(),
        digest,
        binding: fleet_binding(session_id, spec),
        expires_at: None,
        provider,
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

/// Chooses the tools of one start (DL-165, DL-194). `shim_host` is true
/// when the session runs under a shim; `resumed` is true for a resume.
///
/// # Errors
/// The reason that a coordinator start fails with. A session without a
/// grant never fails here: it gets [`Tools::None`] and a log line.
pub(super) fn tools_for(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    shim_host: bool,
    resumed: bool,
) -> Result<Tools, String> {
    // The bridge mode needs Linux and a probed version list in which every
    // version passed the live bridge smoke (§9.10).
    let versions = bindings.versions(provider);
    let bridge_ok = cfg!(target_os = "linux")
        && !versions.is_empty()
        && versions
            .iter()
            .all(|v| fleetdeck_providers::coordinator::bridge_handoff(provider, Some(v)).is_ok());
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
    if resumed {
        return Err(NOT_NEW_CHILD.to_owned());
    }
    if !shim_host {
        return prepare(bindings, provider, session_id, spec, coordinator, false)
            .map(Tools::Direct);
    }
    if !bridge_ok {
        return Err(
            "a coordinator session on the shim host needs a provider version whose bridge is verified"
                .to_owned(),
        );
    }
    prepare_bridge(bindings, provider, session_id, spec, coordinator).map(Tools::Bridge)
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
const FLEET_GRANT: &str = "fleet";

/// The MCP `instructions` of a session without a coordinator grant.
const FLEET_SKILL: &str = "Fleet Deck session tools. `publish_artifact` keeps a file of your \
workspace (an HTML page, Markdown, an image, SVG or PDF) as a versioned artifact that the owner \
opens in Fleet Deck. To show a page or a document to the owner, call publish_artifact. When you \
cannot write files (for example in dontAsk mode), pass the page as `content` with a `name` such as \
report.html. To update a page, publish again with its artifact_id. `set_focus` sets the one line (at most 120 characters) on your card. \
`ask_coordinator` asks the coordinator of your task a question. The tools need no credential.";

#[cfg(test)]
#[path = "coordinator_start_recording_tests.rs"]
mod recording_tests;
