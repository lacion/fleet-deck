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

use fleetdeck_proto::command::{CoordinatorSpec, SessionSpec};
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, Event, SessionId};
use fleetdeck_providers::coordinator::CoordinatorLaunch;
use fleetdeck_providers::host::ProcessControl as _;
use fleetdeck_providers::host::shim::host::Attached;
use fleetdeck_providers::host::shim::start::LaunchSecrets;

use crate::coordinator::{Binding, BindingState, Bindings, Handoff, ToolClass};
use crate::store::shim_bindings::{BindingRow, RowState};
use crate::store::{NewEvent, Store};

pub(super) use super::coordinator_tools::{
    NoTools, Tools, bridge_verified, log_attached, log_no_tools, tools_generic,
};
#[cfg(test)]
pub(super) use super::coordinator_tools::{prepare_bridge_fleet, prepare_fleet, tools_for};

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
    let classes = grant_classes(coordinator, DeckFeatures::of(bindings))?;
    let handoff = Handoff::prepare(provider, &url).map_err(|e| e.to_string())?;
    bindings.insert(
        handoff.digest.clone(),
        grant_binding(session_id, spec, coordinator, classes),
    );
    Ok(with_skill(
        bindings,
        provider,
        session_id,
        handoff.into_launch(),
    ))
}

/// `launch` with the fleet skill tree of the station (MCP P3 §4.2), when the
/// tree is published and the probed version of every instance of the
/// provider family is at or above the verified minimum for the skill (the
/// rule of the handoff gate). Otherwise the launch keeps `skill_dir: None`
/// and the station logs one line for this start.
pub(super) fn with_skill(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    mut launch: CoordinatorLaunch,
) -> CoordinatorLaunch {
    let Some(dir) = bindings.skill_dir() else {
        tracing::info!(session = %session_id, provider = ?provider, "no fleet skill: the tree is not published");
        return launch;
    };
    let versions = bindings.versions(provider);
    let verified = !versions.is_empty()
        && versions
            .iter()
            .all(|v| fleetdeck_providers::coordinator::skill_injection_verified(provider, Some(v)));
    if verified {
        launch.skill_dir = Some(dir);
    } else {
        let probed = if versions.is_empty() {
            "unknown".to_owned()
        } else {
            versions.join(", ")
        };
        tracing::info!(
            session = %session_id,
            provider = ?provider,
            "no fleet skill: provider version not verified for the skill ({probed})"
        );
    }
    launch
}

/// The refusal of a coordinator start that is not a new child.
pub(super) const NOT_NEW_CHILD: &str = "a coordinator session starts only as a new direct child";

/// The input features that the current deck link negotiated (R13 P7 §3.2,
/// R13 P7b §4). No deck link gives `false` for each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DeckFeatures {
    /// `desktop_input_mcp.v1`.
    input_mcp: bool,
    /// `desktop_click_mcp.v1`.
    click_mcp: bool,
    /// `desktop_input_any.v1`.
    input_any: bool,
}

impl DeckFeatures {
    pub(super) fn of(bindings: &Bindings) -> Self {
        bindings.deck().map_or_else(Self::default, |deck| Self {
            input_mcp: deck.has_feature(feature::DESKTOP_INPUT_MCP),
            click_mcp: deck.has_feature(feature::DESKTOP_CLICK_MCP),
            input_any: deck.has_feature(feature::DESKTOP_INPUT_ANY),
        })
    }
}

/// The tool classes of a grant, with the session's own tools (DL-165). The
/// station-internal `recording` class (R13 P5b-2 §13.5) is added only when
/// the spec has `allow_recording` and the `capture` class; a spec that names
/// `recording` itself is refused as an unknown class. The station-internal
/// `input_enabled` class (R13 P7 §3.2) is added only when the spec has
/// `allow_input`, the classes `desktop`, `capture` and `input`,
/// `features.input_mcp`, and either the scope `own_windows`, or the scope
/// `any_window` with `features.input_any`; `any_window` is never reduced to
/// `own_windows` (R13 P7b §4). `click_enabled` (R13 P7b §3.1) needs
/// `input_enabled`, `allow_click` and `features.click_mcp`. `input_any`
/// needs `input_enabled` and the scope `any_window`. A spec that names an
/// internal class is refused too.
pub(super) fn grant_classes(
    coordinator: &CoordinatorSpec,
    features: DeckFeatures,
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
    let scope_ok = match coordinator.input_scope {
        Some(InputScope::OwnWindows) => true,
        Some(InputScope::AnyWindow) => features.input_any,
        None => false,
    };
    if features.input_mcp
        && coordinator.allow_input
        && scope_ok
        && [ToolClass::Desktop, ToolClass::Capture, ToolClass::Input]
            .iter()
            .all(|c| classes.contains(c))
    {
        classes.insert(ToolClass::InputEnabled);
        if coordinator.allow_click && features.click_mcp {
            classes.insert(ToolClass::ClickEnabled);
        }
        if coordinator.input_scope == Some(InputScope::AnyWindow) {
            classes.insert(ToolClass::InputAny);
        }
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
        kind: coordinator.kind,
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
    /// The worker grant (MCP P4a §6.2) when the binding is one: a failed row
    /// save then appends its `shim_row_failed` outcome. `None` otherwise.
    pub grant_id: Option<String>,
    /// The start command of that worker grant.
    pub start_command_id: Option<CommandId>,
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
    let classes = grant_classes(coordinator, DeckFeatures::of(bindings))?;
    let handoff = Handoff::prepare_bridge(provider, &bridge).map_err(|e| e.to_string())?;
    let digest = handoff.digest.clone();
    Ok(BridgeStart {
        launch: with_skill(bindings, provider, session_id, handoff.into_launch()),
        digest,
        binding: grant_binding(session_id, spec, coordinator, classes),
        expires_at: Some(coordinator.expires_at),
        provider,
        grant_id: None,
        start_command_id: None,
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
        if let Some(grant_id) = t.grant_id {
            let skip = super::worker_grant::WorkerSkip {
                grant_id,
                start_command_id: t.start_command_id,
                reason: super::worker_grant::SHIM_ROW_FAILED,
            };
            super::worker_grant::record(store, &t.binding.session_id, Some(&skip)).await;
        }
        return (None, None);
    }
    bindings.insert(t.digest.clone(), t.binding);
    // A shim-hosted Claude gets the plugin directory and the skill rule
    // through `claude_args`; Codex gets its skill root in the handshake.
    let args = if t.provider == ProviderKind::Claude {
        t.launch.claude_args()
    } else {
        t.launch.args
    };
    let secrets = LaunchSecrets {
        token: t.launch.token,
        extra_args: args.into_iter().map(OsString::from).collect(),
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

#[cfg(test)]
#[path = "coordinator_start_recording_tests.rs"]
mod recording_tests;

#[cfg(test)]
#[path = "coordinator_start_input_tests.rs"]
mod input_tests;

/// MCP P3 test 16 (station part): a provider version that is not verified
/// for the fleet skill gets `skill_dir: None` on its launch
/// (`docs/v2/design/mcp-p3.md` §4.2, §6).
#[cfg(test)]
mod skill_tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use fleetdeck_proto::command::SessionSpec;
    use fleetdeck_proto::event::ProviderKind;

    use super::{prepare_bridge_fleet, prepare_fleet};
    use crate::coordinator::{Bindings, BridgeEndpoint};

    const URL: &str = "http://127.0.0.1:41234/mcp";

    fn spec() -> SessionSpec {
        SessionSpec {
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        }
    }

    fn tree() -> PathBuf {
        PathBuf::from("/station/share/fleet-skill/abc")
    }

    /// Bindings with the probed `versions` of `provider` and, when `published`,
    /// the skill tree.
    fn bindings(provider: ProviderKind, versions: &[&str], published: bool) -> Bindings {
        let bindings = Bindings::default();
        bindings.set_endpoint(URL.to_owned());
        bindings.set_bridge(BridgeEndpoint {
            program: "/station/bin/current".into(),
            socket: "/run/mcp.s".into(),
        });
        for version in versions {
            bindings.record_version(provider, (*version).to_owned());
        }
        if published {
            bindings.set_skill_dir(Some(tree()));
        }
        bindings
    }

    /// The skill tree of the direct launch and of the bridge launch.
    fn skill(bindings: &Bindings, provider: ProviderKind) -> (Option<PathBuf>, Option<PathBuf>) {
        let session = "s-1".into();
        let direct = prepare_fleet(bindings, provider, &session, &spec(), false)
            .unwrap()
            .skill_dir;
        let bridge = prepare_bridge_fleet(bindings, provider, &session, &spec())
            .unwrap()
            .launch
            .skill_dir;
        (direct, bridge)
    }

    #[test]
    fn an_unverified_provider_version_gets_no_injection() {
        let some = (Some(tree()), Some(tree()));
        let none = (None, None);
        // Claude: the handoff passes from 2.1.296, the skill from 2.1.302.
        let claude = ProviderKind::Claude;
        assert_eq!(skill(&bindings(claude, &["2.1.302"], true), claude), some);
        assert_eq!(skill(&bindings(claude, &["2.2.0"], true), claude), some);
        assert_eq!(skill(&bindings(claude, &["2.1.301"], true), claude), none);
        // Every probed instance of the family must pass.
        assert_eq!(
            skill(&bindings(claude, &["2.1.302", "2.1.301"], true), claude),
            none
        );
        assert_eq!(
            skill(&bindings(claude, &["2.1.302", "2.1.400"], true), claude),
            some
        );
        // No published tree: no injection, also for a verified version.
        assert_eq!(skill(&bindings(claude, &["2.1.302"], false), claude), none);
        // Codex: the skill from 0.163.0.
        let codex = ProviderKind::Codex;
        assert_eq!(skill(&bindings(codex, &["0.163.0"], true), codex), some);
        assert_eq!(skill(&bindings(codex, &["0.162.1"], true), codex), none);
        // Without a skill the Claude arguments are the plain handoff ones; with
        // one they carry the plugin directory and the skill rule.
        let session = "s-1".into();
        let plain = prepare_fleet(
            &bindings(claude, &["2.1.301"], true),
            claude,
            &session,
            &spec(),
            false,
        )
        .unwrap();
        assert_eq!(plain.claude_args(), plain.args);
        let with = prepare_fleet(
            &bindings(claude, &["2.1.302"], true),
            claude,
            &session,
            &spec(),
            false,
        )
        .unwrap();
        let args = with.claude_args();
        assert!(args.iter().any(|a| a == "--plugin-dir"));
        assert!(args.iter().any(|a| a.contains("Skill(fleetdeck:fleet)")));
    }
}
