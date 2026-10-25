//! `station.desktop_tools_plan` (`board-gaps.md` §2.4, revision 4): the
//! install plan for the tools that are missing, with the root-owned
//! configuration check and the dry run. The terminal command is not run here.

use std::path::Path;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::desktop_tools::{DesktopToolsPlan, code};
use ring::rand::{SecureRandom, SystemRandom};

use super::etc_check::{EtcRoot, conf_sha256};
use super::facts::{PACMAN, context};
use super::mapping::{TOOLS, target};
use super::probe::Exec;
use super::store::{PLAN_LIFETIME_MS, PlanStore, StoredPlan};
use super::transaction::{DryRun, dry_run};
use crate::desktop::detect::Found;
use crate::desktop::run::resolve;

/// The tool IDs of a plan by state.
#[derive(Default)]
struct Split {
    installable: Vec<String>,
    unavailable: Vec<String>,
    already_present: Vec<String>,
    /// The `<repo>/<name>` target of each installable tool.
    targets: Vec<String>,
}

fn split(env: &ChildEnv) -> Split {
    let mut out = Split::default();
    for tool in &TOOLS {
        let id = tool.id.to_owned();
        if resolve(env, tool.exe).is_some() {
            out.already_present.push(id);
        } else if let Some(target) = target(tool) {
            out.installable.push(id);
            out.targets.push(target);
        } else {
            out.unavailable.push(id);
        }
    }
    out
}

fn answer(split: Split) -> DesktopToolsPlan {
    DesktopToolsPlan {
        plan_id: None,
        expires_at: None,
        installable: split.installable,
        unavailable: split.unavailable,
        already_present: split.already_present,
        command_display: None,
        system_upgrade: false,
        blocked: None,
        refused: Vec::new(),
        transaction: Vec::new(),
    }
}

fn blocked(split: Split, refused: Vec<String>) -> DesktopToolsPlan {
    DesktopToolsPlan {
        blocked: Some(code::TRANSACTION_REFUSED.to_owned()),
        refused,
        ..answer(split)
    }
}

/// 128 random bits as 32 lowercase hex characters.
fn new_plan_id() -> Option<String> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Builds the plan of the deck `peer` at `now_ms` and stores it when it is
/// valid. `None` when the plan ID cannot be made.
pub async fn build(
    exec: &dyn Exec,
    root: &EtcRoot,
    store: &PlanStore,
    base: &ChildEnv,
    found: Option<&Found>,
    peer: Fingerprint,
    now_ms: i64,
) -> Option<DesktopToolsPlan> {
    let ctx = context(base, found);
    let split = split(&ctx.env);
    if split.installable.is_empty() {
        return Some(answer(split));
    }
    let conf = conf_sha256(root);
    let (Some(conf_sha256), true) = (conf, exec.exists(Path::new(PACMAN))) else {
        return Some(blocked(split, Vec::new()));
    };
    let transaction = match dry_run(exec, &split.targets).await {
        DryRun::Plan(transaction) => transaction,
        DryRun::Blocked(refused) => return Some(blocked(split, refused)),
    };
    let plan_id = new_plan_id()?;
    let expires_at_ms = now_ms.saturating_add(PLAN_LIFETIME_MS);
    let command_display = format!(
        "sudo pacman -S --needed --noconfirm {}",
        split.targets.join(" ")
    );
    store.insert(
        now_ms,
        StoredPlan {
            plan_id: plan_id.clone(),
            peer,
            tools: split.installable.clone(),
            targets: split.targets.clone(),
            transaction: transaction.clone(),
            conf_sha256,
            expires_at_ms,
        },
    );
    Some(DesktopToolsPlan {
        plan_id: Some(plan_id),
        expires_at: Some(expires_at_ms),
        command_display: Some(command_display),
        transaction,
        ..answer(split)
    })
}

/// The configuration check and the dry run again, against `root`: the hash
/// of `pacman.conf` and the transaction must equal those of the plan.
///
/// # Errors
/// `transaction_refused` for any difference or failure.
pub async fn recheck_with(
    exec: &dyn Exec,
    root: &EtcRoot,
    plan: &StoredPlan,
) -> Result<(), &'static str> {
    let same_conf = conf_sha256(root).is_some_and(|hash| hash == plan.conf_sha256);
    if !same_conf || !exec.exists(Path::new(PACMAN)) {
        return Err(code::TRANSACTION_REFUSED);
    }
    match dry_run(exec, &plan.targets).await {
        DryRun::Plan(transaction) if transaction == plan.transaction => Ok(()),
        _ => Err(code::TRANSACTION_REFUSED),
    }
}

/// [`recheck_with`] on the real system.
///
/// # Errors
/// `transaction_refused` for any difference or failure.
pub(crate) async fn recheck(plan: &StoredPlan) -> Result<(), &'static str> {
    recheck_with(&super::probe::RealExec, &EtcRoot::system(), plan).await
}
