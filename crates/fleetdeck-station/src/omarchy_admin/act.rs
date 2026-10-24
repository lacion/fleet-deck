//! `omarchy.act` (design `omarchy-admin.md` §2.2, §3.1, §3.3): the checks,
//! the fixed child of each action and the detached provisioning retry.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::omarchy_admin::{
    ActOutcome, LOCK_BUSY_EXIT, OmarchyActParams, OmarchyAction, code,
};
use serde_json::Value;

use super::child::{RunResult, clean_message};
use super::provision::{self, StateRead};
use super::repair;
use super::{
    Holder, OmarchyAdmin, Permit, TEXT_BUSY, TEXT_ELEVATION, TEXT_INVALID, TEXT_MISSING,
    TEXT_NOT_PROVISIONED, result, wire,
};

impl OmarchyAdmin {
    /// `omarchy.act`. `peer` is the deck link of the request (the plans of
    /// the repair actions are bound to it).
    pub async fn act_call(self: &Arc<Self>, params: Value, peer: Fingerprint) -> Outcome {
        let Ok(params) = serde_json::from_value::<OmarchyActParams>(params) else {
            return wire(code::INVALID_REQUEST, TEXT_INVALID);
        };
        if !repair::params_valid(&params) {
            return wire(code::INVALID_REQUEST, TEXT_INVALID);
        }
        if !params.elevated {
            return wire(code::ELEVATION_REQUIRED, TEXT_ELEVATION);
        }
        let action = params.action;
        let Some(permit) = self.try_hold(Holder::Act(action)) else {
            return wire(code::BUSY, TEXT_BUSY);
        };
        if action == OmarchyAction::ProvisionRetry {
            return self.retry(permit).await;
        }
        if crate::desktop::detect::omarchy_root(&self.cfg.omarchy_env).is_none() {
            return wire(code::OMARCHY_MISSING, TEXT_MISSING);
        }
        if matches!(
            action,
            OmarchyAction::ScreensaverOn
                | OmarchyAction::ScreensaverOff
                | OmarchyAction::ScreensaverRebaseline
                | OmarchyAction::Uninstall
        ) {
            let status = self.build_status(None).await;
            if status.install.as_ref().is_some_and(|i| i.outdated) {
                return result(
                    ActOutcome::Failed,
                    Some(code::INTEGRATION_OUTDATED),
                    None,
                    status,
                );
            }
        }
        if repair::is_repair_action(action) {
            return self.repair_act(&params, peer, permit).await;
        }
        let (outcome, failed_code, message) = self.run_action(action).await;
        let status = self.build_status(None).await;
        drop(permit);
        result(outcome, failed_code, message, status)
    }

    /// Runs the fixed child of `action` and maps its end.
    async fn run_action(
        &self,
        action: OmarchyAction,
    ) -> (ActOutcome, Option<&'static str>, Option<String>) {
        let (tail, limit, refused): (&[&str], Duration, &'static str) = match action {
            OmarchyAction::Install => (
                &["omarchy", "install", "--yes"],
                self.cfg.limits.install,
                code::INSTALL_REFUSED,
            ),
            OmarchyAction::Uninstall => (
                &["omarchy", "uninstall"],
                self.cfg.limits.uninstall,
                code::UNINSTALL_INCOMPLETE,
            ),
            OmarchyAction::ScreensaverOn => (
                &["omarchy", "screensaver", "on"],
                self.cfg.limits.screensaver,
                code::SCREENSAVER_REFUSED,
            ),
            OmarchyAction::ScreensaverOff => (
                &["omarchy", "screensaver", "off"],
                self.cfg.limits.screensaver,
                code::SCREENSAVER_REFUSED,
            ),
            OmarchyAction::ScreensaverRebaseline => (
                &["omarchy", "screensaver", "rebaseline"],
                self.cfg.limits.screensaver,
                code::SCREENSAVER_REFUSED,
            ),
            // The caller handles the retry and the repair actions before it
            // reaches this function.
            OmarchyAction::ProvisionRetry
            | OmarchyAction::InstallReplace
            | OmarchyAction::InstallResume
            | OmarchyAction::Restore => return (ActOutcome::Failed, Some(code::FAILED), None),
        };
        match self.run_tail(tail, limit).await {
            // A done action keeps the warnings of the command (for example a
            // plugin step that failed), so that the card can show them.
            Ok(RunResult::Exit {
                code: Some(0),
                stderr,
                ..
            }) => (ActOutcome::Done, None, clean_message(&stderr)),
            Ok(RunResult::Exit {
                code: Some(1),
                stderr,
                ..
            }) => (ActOutcome::Failed, Some(refused), clean_message(&stderr)),
            // The Omarchy lock stayed busy for the whole wait of the command.
            Ok(RunResult::Exit { code: Some(c), .. }) if c == i32::from(LOCK_BUSY_EXIT) => {
                (ActOutcome::Failed, Some(code::BUSY), None)
            }
            Ok(RunResult::Exit { .. }) => (ActOutcome::Failed, Some(code::FAILED), None),
            Ok(RunResult::TimedOut) => (ActOutcome::Failed, Some(code::TIMEOUT), None),
            Ok(RunResult::TooLarge) => (ActOutcome::Failed, Some(code::OUTPUT_TOO_LARGE), None),
            Err(e) => {
                tracing::debug!(error = %e, ?action, "the Omarchy child did not start");
                (ActOutcome::Failed, Some(code::FAILED), None)
            }
        }
    }

    /// `provision_retry`: allowed by the state file; the child runs in a
    /// task that holds the action slot until it ends.
    async fn retry(self: &Arc<Self>, permit: Permit) -> Outcome {
        let dir = &self.cfg.station_dir;
        let StateRead::Found { phase, error } = provision::read_state(dir) else {
            return wire(code::NOT_PROVISIONED, TEXT_NOT_PROVISIONED);
        };
        if !matches!(phase.as_str(), "paired" | "link" | "integration")
            || (error.is_none() && provision::bootstrap_running(dir))
        {
            return wire(code::NOT_PROVISIONED, TEXT_NOT_PROVISIONED);
        }
        let Some(file) = self.cfg.provision_file() else {
            let status = self.build_status(None).await;
            return result(ActOutcome::Failed, Some(code::FAILED), None, status);
        };
        let mut tail: Vec<OsString> = vec![
            "provision-bootstrap".into(),
            "--file".into(),
            file.into_os_string(),
        ];
        if error.is_some() {
            tail.push("--retry".into());
        }
        self.set_last_retry("running");
        let status = self.build_status(Some(OmarchyAction::ProvisionRetry)).await;
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let ended = this.run_argv(&tail, this.cfg.limits.retry).await;
            let last = match ended {
                Ok(RunResult::Exit { code: Some(0), .. }) => {
                    let complete = matches!(
                        provision::read_state(&this.cfg.station_dir),
                        StateRead::Found { ref phase, .. } if phase == "complete"
                    );
                    if complete { "complete" } else { "failed" }
                }
                Ok(RunResult::Exit { code: Some(2), .. }) => "terminal",
                Ok(RunResult::TimedOut) => "timeout",
                Ok(_) | Err(_) => "failed",
            };
            this.set_last_retry(last);
            drop(permit);
        });
        result(ActOutcome::Started, None, None, status)
    }
}
