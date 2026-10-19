//! `maintenance.open` (`board-gaps.md` §2.9, Admission and Launch): one
//! durable admission at a time, the spec as the launch marker, then
//! `systemd-run`. A launch that fails or is lost is never repeated: the
//! recovery of the states decides.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::desktop_tools::{MaintenanceInfo, MaintenanceOpen, MaintenanceState};
use fleetdeck_proto::desktop_tools::{code, is_hex_id, size_ok};
use fleetdeck_proto::link::Outcome;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

use super::files::{RunStateKind, Spec, read_state, write_spec};
use super::records::{info_of, list_specs, now_ms, state_of};
use super::service::{MaintenanceService, host_env};

/// The step of the wait for the state `running`.
const SETTLE_STEP: Duration = Duration::from_millis(100);

pub(super) const TEXT_INVALID: &str = "The request is not valid.";
pub(super) const TEXT_INTERNAL: &str = "The answer could not be built.";

pub(super) fn wire(code: &str, text: &str) -> Outcome {
    Outcome::Err(fleetdeck_proto::link::WireError::new(code, text))
}

/// A refusal with the fixed text of `name`.
pub(super) fn refuse(name: &str) -> Outcome {
    wire(name, code::text(name).unwrap_or(TEXT_INVALID))
}

pub(super) fn invalid() -> Outcome {
    wire("invalid_request", TEXT_INVALID)
}

pub(super) fn internal() -> Outcome {
    wire("internal", TEXT_INTERNAL)
}

pub(super) fn answer(value: impl Serialize) -> Outcome {
    match serde_json::to_value(value) {
        Ok(v) => Outcome::Ok(v),
        Err(_) => internal(),
    }
}

/// 128 random bits as 32 lowercase hex characters.
fn random_hex() -> Option<String> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The exact argv of the launch.
pub(super) fn launch_argv(program: &Path, dir: &Path, id: &str) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["/usr/bin/systemd-run", "--user", "--collect", "--quiet"]
        .iter()
        .map(OsString::from)
        .collect();
    argv.push(format!("--unit=fleetdeck-maintenance-{id}").into());
    argv.extend(
        [
            "--service-type=exec",
            "--property=Restart=no",
            "--expand-environment=no",
            "--",
        ]
        .iter()
        .map(OsString::from),
    );
    argv.push(program.into());
    argv.extend(
        ["maintenance-run", "--id", id, "--dir"]
            .iter()
            .map(OsString::from),
    );
    argv.push(dir.into());
    argv
}

impl MaintenanceService {
    /// Waits for the state `running` or `exited` of the run `id`, for at
    /// most `self.settle`.
    async fn settle(&self, id: &str) {
        let deadline = Instant::now() + self.settle;
        loop {
            let ready = matches!(
                read_state(&self.dir, id),
                Ok(Some(s)) if matches!(s.state, RunStateKind::Running | RunStateKind::Exited)
            );
            if ready || Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(SETTLE_STEP).await;
        }
    }

    async fn info(&self, spec: &Spec) -> MaintenanceInfo {
        info_of(self.host.as_ref(), &self.dir, spec).await
    }

    /// `maintenance.open`.
    pub(super) async fn open(
        &self,
        params: Value,
        peer: Fingerprint,
        allow_shell: bool,
    ) -> Outcome {
        let Ok(req) = serde_json::from_value::<MaintenanceOpen>(params) else {
            return invalid();
        };
        if !is_hex_id(&req.request_id) || !is_hex_id(&req.plan_id) || !size_ok(req.cols, req.rows) {
            return invalid();
        }
        let _admission = self.admission.lock().await;
        let specs = match list_specs(&self.dir) {
            Ok(specs) => specs,
            Err(e) => {
                tracing::warn!(error = %e, "the maintenance records cannot be read");
                return internal();
            }
        };
        if let Some(spec) = specs.iter().find(|s| s.request_id == req.request_id) {
            if spec.plan_id != req.plan_id {
                return refuse(code::REQUEST_ID_CONFLICT);
            }
            return answer(self.info(spec).await);
        }
        for spec in &specs {
            let (state, _, _) = state_of(self.host.as_ref(), &self.dir, spec).await;
            if matches!(
                state,
                MaintenanceState::Admitted | MaintenanceState::Running
            ) {
                return refuse(code::MAINTENANCE_BUSY);
            }
        }
        if !allow_shell {
            return refuse(code::SHELL_DISABLED);
        }
        let Some(plan) = self.plans.plan(&req.plan_id, &peer, now_ms()) else {
            return refuse(code::PLAN_CHANGED);
        };
        if let Err(refusal) = self.plans.recheck(&plan).await {
            return refuse(refusal);
        }
        let (Some(id), Some(station_token), Some(runner_token)) =
            (random_hex(), random_hex(), random_hex())
        else {
            tracing::warn!("the system random generator failed");
            return internal();
        };
        let spec = Spec {
            v: 1,
            maintenance_id: id.clone(),
            request_id: req.request_id,
            deck: peer.to_string(),
            plan_id: req.plan_id.clone(),
            tools: plan.tools,
            transaction: plan.transaction,
            conf_sha256: plan.conf_sha256,
            cols: req.cols,
            rows: req.rows,
            station_token,
            runner_token,
            created_at: now_ms(),
        };
        if let Err(e) = write_spec(&self.dir, &spec) {
            tracing::warn!(error = %e, "the maintenance spec was not written");
            return internal();
        }
        self.plans.remove(&req.plan_id);
        let argv = launch_argv(&self.program, &self.dir, &id);
        match self.host.launch(argv, host_env()).await {
            Ok(0) => {}
            Ok(code) => tracing::warn!(code, "the maintenance launch exited with an error"),
            Err(e) => tracing::warn!(error = %e, "the maintenance launch failed"),
        }
        self.settle(&id).await;
        answer(self.info(&spec).await)
    }
}
