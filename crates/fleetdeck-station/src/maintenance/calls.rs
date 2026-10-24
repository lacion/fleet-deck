//! `maintenance.list`, `maintenance.resize` and `maintenance.attach`
//! (`board-gaps.md` §2.9).

use fleetdeck_link::data::Data;
use fleetdeck_proto::desktop_tools::{
    MaintenanceAttach, MaintenanceList, MaintenanceListQuery, MaintenanceResize, MaintenanceState,
    STREAM_PURPOSE, code, is_hex_id, size_ok,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::shell::ShellAttached;
use serde_json::Value;

use super::files::read_spec;
use super::open::{answer, internal, invalid, refuse, wire};
use super::records::{KEEP, info_of, list_specs, remove_files, state_of};
use super::relay::RelayCmd;
use super::runner_client::connect;
use super::service::{MaintenanceService, service};

/// The text of `unavailable`: no service is installed.
pub(super) const TEXT_UNAVAILABLE: &str =
    "Desktop tool installation is not available on this station.";

/// How the attach call issues a stream token for a purpose. `Err` is the
/// text of the failure.
pub(super) type Issue<'a> = &'a dyn Fn(&str) -> Result<String, String>;

impl MaintenanceService {
    /// `maintenance.list`: the newest 16 runs; older finished runs are
    /// removed with their files.
    pub(super) async fn list(&self, params: Value) -> Outcome {
        if !params.is_null() && serde_json::from_value::<MaintenanceListQuery>(params).is_err() {
            return invalid();
        }
        let specs = match list_specs(&self.dir) {
            Ok(specs) => specs,
            Err(e) => {
                tracing::warn!(error = %e, "the maintenance records cannot be read");
                return internal();
            }
        };
        let mut runs = Vec::new();
        for (at, spec) in specs.iter().enumerate() {
            if at < KEEP {
                runs.push(info_of(self.host.as_ref(), &self.dir, spec).await);
                continue;
            }
            let (state, _, _) = state_of(self.host.as_ref(), &self.dir, spec).await;
            if matches!(state, MaintenanceState::Exited | MaintenanceState::Lost) {
                remove_files(&self.dir, &spec.maintenance_id);
            }
        }
        answer(MaintenanceList { runs })
    }

    /// `maintenance.resize`.
    pub(super) async fn resize(&self, params: Value) -> Outcome {
        let Ok(req) = serde_json::from_value::<MaintenanceResize>(params) else {
            return invalid();
        };
        if !is_hex_id(&req.maintenance_id) || !size_ok(req.cols, req.rows) {
            return invalid();
        }
        let Ok(spec) = read_spec(&self.dir, &req.maintenance_id) else {
            return refuse(code::UNKNOWN_MAINTENANCE);
        };
        let (state, _, _) = state_of(self.host.as_ref(), &self.dir, &spec).await;
        if state != MaintenanceState::Running {
            return refuse(code::MAINTENANCE_ENDED);
        }
        // A new client replaces the current one of the runner, so a live
        // relay sends the resize itself.
        if let Some(relay) = self.relay_sender(&req.maintenance_id) {
            let cmd = RelayCmd::Resize {
                cols: req.cols,
                rows: req.rows,
            };
            if relay.send(cmd).await.is_ok() {
                return Outcome::Ok(serde_json::json!({}));
            }
        }
        let sent = match connect(&self.dir, &spec).await {
            Ok(mut client) => client.resize(req.cols, req.rows).await,
            Err(e) => Err(e),
        };
        match sent {
            Ok(()) => Outcome::Ok(serde_json::json!({})),
            Err(e) => {
                tracing::debug!(error = %e, "a maintenance resize did not reach the runner");
                refuse(code::MAINTENANCE_ENDED)
            }
        }
    }
}

/// `maintenance.attach` for `service`: a stream token with the purpose
/// `maintenance:<id>`. `issue` is `None` when the link has no data channel.
pub(super) fn attach_with(
    service: Option<&MaintenanceService>,
    issue: Option<Issue<'_>>,
    params: Value,
) -> Outcome {
    let Ok(req) = serde_json::from_value::<MaintenanceAttach>(params) else {
        return invalid();
    };
    if !is_hex_id(&req.maintenance_id) {
        return invalid();
    }
    let Some(service) = service else {
        return wire("unavailable", TEXT_UNAVAILABLE);
    };
    if read_spec(&service.dir, &req.maintenance_id).is_err() {
        return refuse(code::UNKNOWN_MAINTENANCE);
    }
    let Some(issue) = issue else {
        return wire("unsupported", "this link has no data channel");
    };
    match issue(&format!("{STREAM_PURPOSE}{}", req.maintenance_id)) {
        Ok(stream_token) => answer(ShellAttached { stream_token }),
        Err(e) => {
            tracing::warn!(error = %e, "a maintenance stream token was not issued");
            internal()
        }
    }
}

/// `maintenance.attach` on the installed service. `data` is the data channel
/// of the link, when it has one.
pub fn attach_call(data: Option<&Data>, params: Value) -> Outcome {
    let installed = service();
    let issuer = data.map(|d| {
        move |purpose: &str| {
            d.issue_token(purpose)
                .map(|token| token.as_str().to_owned())
                .map_err(|e| e.to_string())
        }
    });
    attach_with(
        installed.as_deref(),
        issuer.as_ref().map(|f| f as Issue<'_>),
        params,
    )
}
