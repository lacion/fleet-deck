//! The launch of a shim with the commits of ADR-005 §3 (moved out of
//! [`super`] so that module stays under 800 lines). The final
//! authority check of the launch is MCP P4b §7.1, the late check.

use fleetdeck_proto::SessionId;
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost};
use fleetdeck_providers::host::{ProcessControl as _, ProcessSpec};

use super::super::coordinator_start::{
    BridgeStart, kill_unless_saved, log_attached, revoke_stored, store_tools,
};
use super::{IN_FLIGHT_WAIT, LaunchError, Recovered, reason, recover_shim};
use crate::coordinator::Bindings;
use crate::store::{NewShim, RunningFacts, ShimState, Store};

/// [`super::launch_shim`] for a resume or an import: `native_target`
/// is the native id that the new provider continues. The row holds it before
/// the spawn, so a crash after the spawn still names the conversation (DL-182
/// §3). The row also holds the boot id of the launch journal. `gate` (the
/// authority gate, ADR-008 §6.2) runs after the `starting` row and right
/// before the shim starts; a refusal ends the row `never_started` and is
/// [`LaunchError::Refused`].
///
/// # Errors
/// [`LaunchError`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_shim_for(
    store: &Store,
    host: &ShimHost,
    session_id: &SessionId,
    spec: &ProcessSpec,
    mode: AckMode,
    native_target: Option<&str>,
    gate: &super::super::shim_stdin::Gate<'_>,
    bindings: &Bindings,
    tools: Option<BridgeStart>,
) -> std::result::Result<Attached, LaunchError> {
    let prepared = host.prepare(spec).map_err(LaunchError::Prepare)?;
    let shim_id = prepared.record.shim_id.clone();
    store
        .shim_starting(NewShim {
            shim_id: shim_id.clone(),
            session_id: session_id.clone(),
            socket_path: prepared.record.socket_path.clone(),
            launch_boot_id: Some(prepared.launch_boot_id.clone()),
            native_target: native_target.map(str::to_owned),
        })
        .await?;
    let settle = async |error: String| -> std::result::Result<Attached, LaunchError> {
        let Some(row) = store.shim(shim_id.clone()).await? else {
            return Err(LaunchError::Settled {
                error,
                settled: reason::EXECUTION_UNKNOWN,
            });
        };
        match recover_shim(store, host, &row, mode, IN_FLIGHT_WAIT).await? {
            Recovered::Reattached(attached) => Ok(attached),
            other => Err(LaunchError::Settled {
                error,
                settled: other.name(),
            }),
        }
    };
    if let Some(refused) = gate().await {
        // No shim started: the launch journal proves it.
        store
            .shim_end(
                shim_id.clone(),
                ShimState::NeverStarted,
                reason::SHIM_NEVER_STARTED.to_owned(),
                0,
            )
            .await?;
        return Err(LaunchError::Refused(refused));
    }
    // The session lock of the start (MCP P4b PRE-PR R1) drops right after the
    // row and the binding are stored; a path that leaves earlier drops it
    // with the start.
    let mut tools = tools;
    let session_lock = tools.as_mut().and_then(|t| t.lock.take());
    let (secrets, stored) = store_tools(store, bindings, &shim_id, tools).await;
    drop(session_lock);
    // A failure after a stored row ends the row and refuses its token
    // before the launch is settled (DL-194 §2.3). The cleanup is durable
    // (the fence or the row), or the start fails even when the settlement
    // reattaches (DL-194).
    let cleanup = async || revoke_stored(store, bindings, &shim_id, stored.as_ref()).await;
    let fail = async |error: String| -> std::result::Result<Attached, LaunchError> {
        let durable = cleanup().await;
        kill_unless_saved(durable, settle(error).await).await
    };
    let secrets = match secrets {
        Some((secrets, _)) => match host.exe_digest(&spec.program.display().to_string()).await {
            Ok(digest) => Some((secrets, digest)),
            Err(e) => return fail(e.to_string()).await,
        },
        None => None,
    };
    // The final authority check (MCP P4b §7.1, the late check): after the
    // credential save and the executable hash, right before the launch; the
    // early check above is only a fast refusal.
    if let Some(refused) = gate().await {
        cleanup().await;
        store
            .shim_end(
                shim_id.clone(),
                ShimState::NeverStarted,
                reason::SHIM_NEVER_STARTED.to_owned(),
                0,
            )
            .await?;
        return Err(LaunchError::Refused(refused));
    }
    let started = host.start_with_digest(prepared, secrets).await;
    let record = match started {
        Ok(record) => record,
        Err(e) => return fail(e.to_string()).await,
    };
    if let Some((_, provider)) = &stored {
        log_attached(session_id, *provider);
    }
    let Some(identity) = record.provider_identity.clone() else {
        return fail("no provider identity".to_owned()).await;
    };
    let facts = RunningFacts {
        protocol: None,
        shim_pid: record.shim_pid,
    };
    match store
        .shim_running_with(shim_id.clone(), identity, facts)
        .await
    {
        Ok(true) => {}
        Ok(false) => return fail("the identity did not commit".to_owned()).await,
        Err(e) => {
            cleanup().await;
            return Err(e.into());
        }
    }
    let generation = match store.shim_next_generation(shim_id.clone()).await {
        Ok(Some(generation)) => generation,
        Ok(None) => return fail("the row ended".to_owned()).await,
        Err(e) => {
            cleanup().await;
            return Err(e.into());
        }
    };
    match host.attach(&record, generation, mode, false).await {
        Ok(attached) => {
            if let Err(e) = store
                .shim_picked_protocol(shim_id.clone(), attached.protocol)
                .await
            {
                if !cleanup().await {
                    // No durable cleanup: the shim must not keep the token.
                    attached.process.kill().await;
                }
                return Err(e.into());
            }
            Ok(attached)
        }
        Err(e) => fail(e.to_string()).await,
    }
}
