//! `project.secret_scan`, `project.secret_read`, `project.secret_write` and
//! `project.secret_browse` (Fleet Sync FS-3, design `docs/v2/design/fleet-sync.md` §3). The control
//! channel already checked the method, the scope and the feature. The logic
//! is in `workspace::fleet_git::secrets`; this file turns its result into the
//! link outcome. A refusal never holds a part of the params.

use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;
use crate::workspace::fleet_git::secret_call;

/// Whether `method` is one of the four secret file methods.
pub(super) fn has(method: &str) -> bool {
    matches!(
        method,
        "project.secret_scan"
            | "project.secret_read"
            | "project.secret_write"
            | "project.secret_browse"
    )
}

/// Runs one secret file method.
pub(super) async fn call(ctx: &StationLink, method: &str, params: serde_json::Value) -> Outcome {
    let policy = ctx.manager.policy();
    let options = ctx.fleet_git.provision_options();
    let data_dir = crate::resources::data_dir();
    match secret_call(
        ctx.manager.store(),
        &policy,
        ctx.manager.private_paths(),
        &options,
        data_dir.as_deref(),
        method,
        params,
    )
    .await
    {
        Ok(value) => Outcome::Ok(value),
        Err(e) => Outcome::Err(WireError::new(e.code, e.text)),
    }
}
