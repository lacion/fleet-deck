//! The installer repair on the station (`board-gaps.md` §3.2, §3.3, §3.4):
//! the plan store, the three reads `omarchy.install_plan`,
//! `omarchy.repair_status` and `omarchy.restore_plan`, and the actions
//! `install_replace`, `install_resume` and `restore` of `omarchy.act`.
//!
//! The full facts of a plan stay in the store of the station. The deck gets
//! a view without hashes. An action names a plan by its ID; the facts go to
//! the CLI child on stdin. A plan is used once, lasts two minutes and is
//! bound to the deck link that asked for it.

use std::ffi::OsString;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::omarchy_admin::{
    ActOutcome, LOCK_BUSY_EXIT, OmarchyActParams, OmarchyActResult, OmarchyAction, code,
};
use fleetdeck_proto::omarchy_repair::{
    InstallPlanFacts, InstallPlanInput, InstallPlanView, InstallPlanViewFile, PlanBlock,
    RepairResult, RepairStatusView, RestorePlanFacts, RestorePlanInput, RestorePlanView,
    RestorePlanViewFile, code as repair_code,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::Value;

use super::child::RunResult;
use super::{OmarchyAdmin, Permit, TEXT_BUSY, TEXT_INVALID, answer, is_empty_params, wire};

/// How long a plan lasts.
pub(super) const PLAN_LIFETIME_MS: i64 = 120_000;
/// The most plans that the station keeps.
pub(super) const MAX_PLANS: usize = 4;
/// The exit code of a coded refusal or stop of a repair command.
const REFUSED_EXIT: i32 = 4;
/// The text of `failed` for a repair read.
const TEXT_REPAIR_FAILED: &str = "The Omarchy repair command failed.";
/// The codes that a repair result can carry.
const REPAIR_CODES: [&str; 6] = [
    repair_code::PLAN_CHANGED,
    repair_code::UNSAFE_PATH,
    repair_code::BACKUP_FAILED,
    repair_code::REPAIR_INCOMPLETE,
    repair_code::RESTORE_CONFLICT,
    repair_code::BUILD_MISMATCH,
];

/// The time of the plan store, in Unix milliseconds.
pub(super) struct Clock(Box<dyn Fn() -> i64 + Send + Sync>);

impl Clock {
    pub(super) fn new(now: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self(Box::new(now))
    }

    /// The system time.
    pub(super) fn system() -> Self {
        Self::new(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        })
    }

    pub(super) fn now_ms(&self) -> i64 {
        (self.0)()
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

/// What a stored plan is for.
#[derive(Debug)]
pub(super) enum PlanKind {
    Install(InstallPlanFacts),
    Restore {
        repair_id: String,
        facts: RestorePlanFacts,
    },
}

#[derive(Debug)]
struct StoredPlan {
    id: String,
    kind: PlanKind,
    /// The deck link that asked for the plan.
    deck: Fingerprint,
    expires_at: i64,
}

/// The plans of the station: at most [`MAX_PLANS`], the oldest first.
#[derive(Debug, Default)]
pub(super) struct PlanStore {
    plans: Mutex<Vec<StoredPlan>>,
}

impl PlanStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<StoredPlan>> {
        self.plans.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores a plan that expires at `expiry(now)`. The expired plans go
    /// first; a fifth plan removes the oldest.
    pub(super) fn insert(&self, now: i64, id: String, kind: PlanKind, deck: Fingerprint) {
        let expires_at = expiry(now);
        let mut plans = self.lock();
        plans.retain(|p| p.expires_at > now);
        plans.push(StoredPlan {
            id,
            kind,
            deck,
            expires_at,
        });
        while plans.len() > MAX_PLANS {
            plans.remove(0);
        }
    }

    /// Takes the install plan `id` of the deck `deck`. An expired plan is
    /// dropped. A plan that fails another check stays.
    pub(super) fn take_install(
        &self,
        now: i64,
        id: &str,
        deck: Fingerprint,
    ) -> Option<InstallPlanFacts> {
        let mut plans = self.lock();
        let at = Self::find(&mut plans, now, id)?;
        let ok = plans[at].deck == deck && matches!(plans[at].kind, PlanKind::Install(_));
        if !ok {
            return None;
        }
        match plans.remove(at).kind {
            PlanKind::Install(facts) => Some(facts),
            PlanKind::Restore { .. } => None,
        }
    }

    /// Takes the restore plan `id` of the deck `deck` for the repair
    /// `repair_id`; the checks are those of [`Self::take_install`].
    pub(super) fn take_restore(
        &self,
        now: i64,
        id: &str,
        deck: Fingerprint,
        repair_id: &str,
    ) -> Option<RestorePlanFacts> {
        let mut plans = self.lock();
        let at = Self::find(&mut plans, now, id)?;
        let ok = plans[at].deck == deck
            && matches!(&plans[at].kind, PlanKind::Restore { repair_id: r, .. } if r == repair_id);
        if !ok {
            return None;
        }
        match plans.remove(at).kind {
            PlanKind::Restore { facts, .. } => Some(facts),
            PlanKind::Install(_) => None,
        }
    }

    /// The index of the live plan `id`; an expired plan is removed.
    fn find(plans: &mut Vec<StoredPlan>, now: i64, id: &str) -> Option<usize> {
        let at = plans.iter().position(|p| p.id == id)?;
        if plans[at].expires_at <= now {
            plans.remove(at);
            return None;
        }
        Some(at)
    }

    #[cfg(test)]
    pub(super) fn ids(&self) -> Vec<String> {
        self.lock().iter().map(|p| p.id.clone()).collect()
    }
}

/// The end of the life of a plan that is stored at `now`.
pub(super) fn expiry(now: i64) -> i64 {
    now.saturating_add(PLAN_LIFETIME_MS)
}

/// 32 lowercase hex characters.
pub(super) fn valid_hex_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// 1 to 64 characters of `[A-Za-z0-9_-]`.
fn valid_request_id(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A new plan ID: 128 random bits as 32 lowercase hex characters.
fn new_plan_id() -> Option<String> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// True for the three actions of `omarchy_repair.v1`.
pub(super) const fn is_repair_action(action: OmarchyAction) -> bool {
    matches!(
        action,
        OmarchyAction::InstallReplace | OmarchyAction::InstallResume | OmarchyAction::Restore
    )
}

/// The rules of the params of each action: the required fields, the allowed
/// fields and the shape of each value. The old actions carry none of the new
/// fields.
pub(super) fn params_valid(p: &OmarchyActParams) -> bool {
    // (plan_id, replace_recorded_edits, repair_id, asset_ids, request_id)
    let given = (
        p.plan_id.is_some(),
        p.replace_recorded_edits.is_some(),
        p.repair_id.is_some(),
        p.asset_ids.is_some(),
        p.request_id.is_some(),
    );
    let shapes = p.plan_id.as_deref().is_none_or(valid_hex_id)
        && p.repair_id.as_deref().is_none_or(valid_hex_id)
        && p.request_id.as_deref().is_none_or(valid_request_id);
    if !shapes {
        return false;
    }
    match p.action {
        OmarchyAction::InstallReplace => {
            matches!(given, (true, _, false, false, _))
        }
        OmarchyAction::InstallResume => {
            matches!(given, (false, false, true, false, _))
        }
        OmarchyAction::Restore => {
            matches!(given, (true, false, true, true, _))
                && p.asset_ids.as_ref().is_some_and(|a| !a.is_empty())
        }
        _ => given == (false, false, false, false, false),
    }
}

/// The wire error code of a plan block.
fn block_code(block: PlanBlock) -> &'static str {
    match block {
        PlanBlock::UnsafePath => repair_code::UNSAFE_PATH,
    }
}

fn install_view(id: String, expires_at: i64, facts: &InstallPlanFacts) -> InstallPlanView {
    InstallPlanView {
        plan_id: id,
        expires_at,
        files: facts
            .files
            .iter()
            .map(|f| InstallPlanViewFile {
                asset_id: f.asset_id,
                path: f.path.clone(),
                classification: f.classification,
                replaceable: f.replaceable,
            })
            .collect(),
        blocked: facts
            .files
            .iter()
            .find_map(|f| f.blocked)
            .map(|b| block_code(b).to_owned()),
    }
}

fn restore_view(id: String, expires_at: i64, facts: &RestorePlanFacts) -> RestorePlanView {
    RestorePlanView {
        plan_id: id,
        expires_at,
        repair_id: facts.repair_id.clone(),
        files: facts
            .files
            .iter()
            .map(|f| RestorePlanViewFile {
                asset_id: f.asset_id,
                path: f.path.clone(),
                action: f.action,
            })
            .collect(),
    }
}

/// The params of `omarchy.restore_plan`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestorePlanParams {
    repair_id: String,
}

/// How a repair action ended: the outcome, the code and the result.
type Ended = (ActOutcome, Option<String>, Option<RepairResult>);

/// An end `failed` with a fixed code and no result.
fn failed(fixed: &str) -> Ended {
    (ActOutcome::Failed, Some(fixed.to_owned()), None)
}

/// The stdin bytes of a repair child; `None` when they do not serialize.
fn to_stdin<T: serde::Serialize>(input: &T) -> Option<Vec<u8>> {
    match serde_json::to_vec(input) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::debug!(error = %e, "the stdin of a repair action did not serialize");
            None
        }
    }
}

/// Maps the end of the child of a repair action. Exit 0 is `done` with the
/// result; exit 4 is `failed` with the code of the result; exit 3 is `busy`.
/// An empty or invalid stdout, a code outside the six, and every other end
/// are `failed` with a fixed code and no result.
fn map_end(ended: std::io::Result<RunResult>) -> Ended {
    match ended {
        Ok(RunResult::Exit {
            code: Some(0),
            stdout,
            ..
        }) => match serde_json::from_slice::<RepairResult>(&stdout) {
            Ok(r) => (ActOutcome::Done, None, Some(r)),
            Err(_) => failed(code::FAILED),
        },
        Ok(RunResult::Exit {
            code: Some(REFUSED_EXIT),
            stdout,
            ..
        }) => match serde_json::from_slice::<RepairResult>(&stdout) {
            Ok(r) => match r.code.clone() {
                Some(c) if REPAIR_CODES.contains(&c.as_str()) => {
                    (ActOutcome::Failed, Some(c), Some(r))
                }
                _ => failed(code::FAILED),
            },
            Err(_) => failed(code::FAILED),
        },
        Ok(RunResult::Exit { code: Some(c), .. }) if c == i32::from(LOCK_BUSY_EXIT) => {
            failed(code::BUSY)
        }
        Ok(RunResult::Exit { .. }) => failed(code::FAILED),
        Ok(RunResult::TimedOut) => failed(code::TIMEOUT),
        Ok(RunResult::TooLarge) => failed(code::OUTPUT_TOO_LARGE),
        Err(e) => {
            tracing::debug!(error = %e, "the Omarchy repair child did not start");
            failed(code::FAILED)
        }
    }
}

impl OmarchyAdmin {
    /// The time of the plan store, in Unix milliseconds.
    fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Runs a read child (`status` limit, no action slot). The stdout of an
    /// exit 0 is `Ok`; every other end is the wire error.
    async fn read_child(&self, tail: &[&str]) -> Result<Vec<u8>, Outcome> {
        match self.run_tail(tail, self.cfg.limits.status).await {
            Ok(RunResult::Exit {
                code: Some(0),
                stdout,
                ..
            }) => Ok(stdout),
            Ok(RunResult::Exit { code: Some(c), .. }) if c == i32::from(LOCK_BUSY_EXIT) => {
                Err(wire(code::BUSY, TEXT_BUSY))
            }
            Ok(_) => Err(wire(code::FAILED, TEXT_REPAIR_FAILED)),
            Err(e) => {
                tracing::debug!(error = %e, "the Omarchy repair read did not run");
                Err(wire(code::FAILED, TEXT_REPAIR_FAILED))
            }
        }
    }

    /// `omarchy.install_plan`: params `null` or `{}` only.
    pub async fn install_plan_call(&self, params: Value, peer: Fingerprint) -> Outcome {
        if !is_empty_params(&params) {
            return wire(code::INVALID_REQUEST, TEXT_INVALID);
        }
        let stdout = match self
            .read_child(&["omarchy", "install-plan", "--json"])
            .await
        {
            Ok(stdout) => stdout,
            Err(e) => return e,
        };
        let (Ok(facts), Some(id)) = (
            serde_json::from_slice::<InstallPlanFacts>(&stdout),
            new_plan_id(),
        ) else {
            return wire(code::FAILED, TEXT_REPAIR_FAILED);
        };
        let now = self.now_ms();
        let view = install_view(id.clone(), expiry(now), &facts);
        self.plans.insert(now, id, PlanKind::Install(facts), peer);
        answer(view)
    }

    /// `omarchy.repair_status`: params `null` or `{}` only.
    pub async fn repair_status_call(&self, params: Value) -> Outcome {
        if !is_empty_params(&params) {
            return wire(code::INVALID_REQUEST, TEXT_INVALID);
        }
        let stdout = match self
            .read_child(&["omarchy", "repair-status", "--json"])
            .await
        {
            Ok(stdout) => stdout,
            Err(e) => return e,
        };
        match serde_json::from_slice::<RepairStatusView>(&stdout) {
            Ok(view) => answer(view),
            Err(_) => wire(code::FAILED, TEXT_REPAIR_FAILED),
        }
    }

    /// `omarchy.restore_plan`: params `{repair_id}`.
    pub async fn restore_plan_call(&self, params: Value, peer: Fingerprint) -> Outcome {
        let Ok(params) = serde_json::from_value::<RestorePlanParams>(params) else {
            return wire(code::INVALID_REQUEST, TEXT_INVALID);
        };
        if !valid_hex_id(&params.repair_id) {
            return wire(code::INVALID_REQUEST, TEXT_INVALID);
        }
        let stdout = match self
            .read_child(&[
                "omarchy",
                "restore-plan",
                "--repair",
                &params.repair_id,
                "--json",
            ])
            .await
        {
            Ok(stdout) => stdout,
            Err(e) => return e,
        };
        let (Ok(facts), Some(id)) = (
            serde_json::from_slice::<RestorePlanFacts>(&stdout),
            new_plan_id(),
        ) else {
            return wire(code::FAILED, TEXT_REPAIR_FAILED);
        };
        if facts.repair_id != params.repair_id {
            return wire(code::FAILED, TEXT_REPAIR_FAILED);
        }
        let now = self.now_ms();
        let view = restore_view(id.clone(), expiry(now), &facts);
        let kind = PlanKind::Restore {
            repair_id: params.repair_id,
            facts,
        };
        self.plans.insert(now, id, kind, peer);
        answer(view)
    }

    /// The answer `plan_changed`: no child ran.
    async fn plan_changed(&self, permit: Permit) -> Outcome {
        let status = self.build_status(None).await;
        drop(permit);
        answer(OmarchyActResult {
            outcome: ActOutcome::Failed,
            code: Some(repair_code::PLAN_CHANGED.to_owned()),
            message: None,
            status,
            repair: None,
        })
    }

    /// One of the three repair actions. The caller has checked the params,
    /// the elevation, the action slot (`permit`) and the presence of Omarchy.
    pub(super) async fn repair_act(
        &self,
        params: &OmarchyActParams,
        peer: Fingerprint,
        permit: Permit,
    ) -> Outcome {
        let plan_id = params.plan_id.as_deref().unwrap_or_default();
        let repair_id = params.repair_id.as_deref().unwrap_or_default();
        let now = self.now_ms();
        let (tail, stdin): (Vec<&str>, Option<Vec<u8>>) = match params.action {
            OmarchyAction::InstallReplace => {
                let Some(facts) = self.plans.take_install(now, plan_id, peer) else {
                    return self.plan_changed(permit).await;
                };
                let input = InstallPlanInput {
                    facts,
                    replace_recorded_edits: params
                        .replace_recorded_edits
                        .clone()
                        .unwrap_or_default(),
                };
                let Some(bytes) = to_stdin(&input) else {
                    return self.finish(permit, failed(code::FAILED)).await;
                };
                (
                    vec![
                        "omarchy",
                        "install",
                        "--yes",
                        "--replace-conflicts",
                        "--plan-stdin",
                    ],
                    Some(bytes),
                )
            }
            OmarchyAction::InstallResume => {
                (vec!["omarchy", "install", "--resume", repair_id], None)
            }
            OmarchyAction::Restore => {
                let Some(facts) = self.plans.take_restore(now, plan_id, peer, repair_id) else {
                    return self.plan_changed(permit).await;
                };
                let input = RestorePlanInput {
                    facts,
                    asset_ids: params.asset_ids.clone().unwrap_or_default(),
                };
                let Some(bytes) = to_stdin(&input) else {
                    return self.finish(permit, failed(code::FAILED)).await;
                };
                (vec!["omarchy", "restore", "--plan-stdin"], Some(bytes))
            }
            _ => return wire(code::INVALID_REQUEST, TEXT_INVALID),
        };
        let argv: Vec<OsString> = tail.iter().map(OsString::from).collect();
        let ended = self
            .run_argv_stdin(&argv, self.cfg.limits.install, stdin)
            .await;
        self.finish(permit, map_end(ended)).await
    }

    /// Builds the answer of a repair action and frees the action slot.
    async fn finish(&self, permit: Permit, (outcome, failed_code, repair): Ended) -> Outcome {
        let status = self.build_status(None).await;
        drop(permit);
        answer(OmarchyActResult {
            outcome,
            code: failed_code,
            message: None,
            status,
            repair,
        })
    }
}
