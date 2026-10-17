//! Step 6 of a land, mode `pr` (FG-8, design §11.5): `pr_find`, then the
//! pull request creation through the gated start of `fleet_git_writer.rs`.
//! A fresh run and a recovery use the same loop; a recovery reads the pull
//! request of the stored operands first. A forge write is not a Git write, so
//! this runs on the land task, not in the writer.

use fleetdeck_proto::fleet_git::{LandPhase, LandState, code};
use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind};

use super::fleet_git_writer::{Gate, Gated, gated_write};
use super::land::Shared;
use super::land_publish::{Ctx, Kind};
use crate::integrations::exec::Fail;
use crate::integrations::pr_find::{PrCreate, PrRef, pr_create_direct, pr_find};
use crate::store::land_publish::{
    OutcomeChange, Pending, PendingChange, ReconcileChange, Require, Step, StepEnd, WRITE_APPLIED,
    WRITE_REJECTED, WRITE_UNKNOWN,
};
use crate::store::land_runs::{LandRunRow, PENDING_PR_CREATE};
use crate::workspace::fleet_git::redact;

/// The API base of a forge, the `pending_url` of a pull request creation.
fn api_base(binding: &ForgeBinding) -> String {
    match binding.kind {
        ForgeKind::Github if binding.host == "github.com" => "https://api.github.com".to_owned(),
        ForgeKind::Github => format!("https://{}/api/v3", binding.host),
        ForgeKind::Gitlab => format!("https://{}/api/v4", binding.host),
    }
}

/// How the last pull request creation ended, when it did not apply.
enum Created {
    Done,
    Failed { kind: Kind, message: String },
}

/// Step 6, mode `pr` (design §11.5): `pr_find` first, then the create through
/// the gated start. A fresh run and a recovery use the same loop; a recovery
/// reads the pull request of the stored operands first.
pub(super) async fn pr_step(shared: &Shared, land_id: &str) {
    let ctx = shared.ctx();
    let Some(row) = ctx.running(land_id).await else {
        return;
    };
    let recovery = row.pending_action.as_deref() == Some(PENDING_PR_CREATE);
    if !recovery && (row.pending_action.is_some() || row.phase != LandPhase::BranchPushed) {
        return;
    }
    let Some(binding) = row.plan.forge.clone() else {
        ctx.fail(
            &row,
            code::INTERNAL,
            "The land failed: a pull request land has no forge.",
        )
        .await;
        return;
    };
    let integrations = (shared.integrations)();
    let forge = integrations.forge(&binding);
    let default_ref = row.default_ref.clone().unwrap_or_default();
    let (head, base) = match (&row.pending_expect, recovery) {
        (Some(expect), true) => expect.split_once('\t').map_or_else(
            || (row.plan.publish_ref.clone(), default_ref.clone()),
            |(h, b)| (h.to_owned(), b.to_owned()),
        ),
        _ => (row.plan.publish_ref.clone(), default_ref.clone()),
    };
    let mut written = recovery;
    let mut kind = Kind::Unknown;
    let mut message = String::new();
    let mut retried = false;
    loop {
        let found = pr_find(&forge, &head, &base, ctx.timeouts.forge).await;
        let Some(row) = ctx.running(land_id).await else {
            return;
        };
        let found = match found {
            Ok(found) => found,
            Err(fail) => {
                if written {
                    ctx.wait(&row).await;
                } else {
                    ctx.fail(&row, code::PR_FAILED, &pr_failed(&fail.message))
                        .await;
                }
                return;
            }
        };
        match found.as_slice() {
            [one] => return landed(&ctx, &row, one, OutcomeChange::Keep).await,
            [] => {}
            _ => {
                let first = format!(
                    "More than one open pull request has the head {} and the base {}. \
                     Close the extra ones and land again.",
                    row.plan.branch_ref, base
                );
                ctx.fail(&row, code::PR_AMBIGUOUS, &first).await;
                return;
            }
        }
        // None found. After a rejected create, or the repeated one, it ends.
        if written && (kind == Kind::Rejected || retried) {
            ctx.fail(&row, code::PR_FAILED, &pr_failed(&message)).await;
            return;
        }
        retried = written;
        match create(&ctx, &forge, &binding, &row, &head, &base, written).await {
            Gated::Written(Created::Done) | Gated::Stopped | Gated::Refused => return,
            Gated::Written(Created::Failed {
                kind: k,
                message: m,
            }) => {
                (kind, message, written) = (k, m, true);
            }
        }
    }
}

/// "The pull request failed: <redacted line>" (design §11.10).
fn pr_failed(message: &str) -> String {
    format!("The pull request failed: {}", redact(message))
}

async fn landed(ctx: &Ctx, row: &LandRunRow, pr: &PrRef, outcome: OutcomeChange) {
    let step = Step {
        pending: PendingChange::Clear,
        reconcile: ReconcileChange::Clear,
        outcome,
        pr_url: Some(pr.url.clone()),
        pr_number: Some(pr.number),
        end: Some(StepEnd {
            state: LandState::Landed,
            code: None,
            text: None,
        }),
        ..Step::default()
    };
    ctx.step(&row.land_id, step).await;
}

/// One creation through the gated start.
async fn create(
    ctx: &Ctx,
    forge: &crate::integrations::forge::Forge<'_>,
    binding: &ForgeBinding,
    row: &LandRunRow,
    head: &str,
    base: &str,
    recovery: bool,
) -> Gated<Created> {
    let intent = if recovery {
        Step {
            outcome: OutcomeChange::Clear,
            require: Require::Pending,
            ..Step::default()
        }
    } else {
        Step {
            phase: Some(LandPhase::PrIntent),
            pending: PendingChange::Set(Pending {
                action: PENDING_PR_CREATE,
                attempt: row.attempt,
                url: api_base(binding),
                ref_name: format!("refs/heads/{}", row.plan.publish_ref),
                oid: row.rebased_oid.clone().unwrap_or_default(),
                lease: None,
                expect: Some(format!("{head}\t{base}")),
            }),
            outcome: OutcomeChange::Clear,
            require: Require::NoPending,
            event: true,
            ..Step::default()
        }
    };
    let gate = Gate {
        store: &ctx.store,
        policy: &ctx.policy,
        locks: &ctx.locks,
        run: row,
    };
    gated_write(
        &gate,
        intent,
        || {
            pr_create_direct(
                forge,
                head,
                base,
                &row.plan.pr_title,
                &row.plan.pr_body,
                ctx.timeouts.forge,
            )
        },
        |made| record_create(ctx, row, made),
    )
    .await
}

async fn record_create(ctx: &Ctx, row: &LandRunRow, made: PrCreate) -> Created {
    let (kind, fail, value): (Kind, Fail, &'static str) = match made {
        PrCreate::Applied(pr) => {
            landed(ctx, row, &pr, OutcomeChange::Set(WRITE_APPLIED)).await;
            return Created::Done;
        }
        PrCreate::Rejected(fail) => (Kind::Rejected, fail, WRITE_REJECTED),
        PrCreate::Unknown(fail) => (Kind::Unknown, fail, WRITE_UNKNOWN),
    };
    if let Err(e) = ctx
        .store
        .land_write_outcome(row.land_id.clone(), value)
        .await
    {
        tracing::error!(land = %row.land_id, error = %e, "a write outcome was not stored");
    }
    Created::Failed {
        kind,
        message: fail.message,
    }
}
