//! Helpers of the land publication tests (FG-8): the fake deck link, the
//! coordinator admission, and the staging of a run in the state that a crash
//! leaves, with the same operands that the writer stores.

use std::sync::Arc;

use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::{CommandOrigin, ReceiptState};
use fleetdeck_proto::fleet_git::{LandPhase, LandPlan};
use fleetdeck_proto::mutation::CommandAuthority;

use super::rig::*;
use crate::manager::land_publish::{Ctx, branch_job};
use crate::store::land_publish::{OutcomeChange, Pending, PendingChange, Require, Step};
use crate::store::land_runs::{
    LandRunRow, PENDING_BRANCH_PUSH, PENDING_DEFAULT_PUSH, PENDING_PR_CREATE,
};

/// A deck link whose answer to `coordinator.authority_check` the test sets:
/// `allow` valid answers, then invalid ones; `hang` never answers.
pub(super) struct Deck {
    pub(super) calls: std::sync::atomic::AtomicUsize,
    pub(super) allow: std::sync::atomic::AtomicUsize,
    pub(super) hang: std::sync::atomic::AtomicBool,
}

impl Deck {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            allow: std::sync::atomic::AtomicUsize::new(usize::MAX),
            hang: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Revokes the grant: every later check answers invalid.
    pub(super) fn revoke(&self) {
        self.allow.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl crate::coordinator::Transport for Deck {
    async fn request(
        &self,
        method: &str,
        _command_id: Option<&CommandId>,
        _params: serde_json::Value,
    ) -> Result<fleetdeck_proto::link::Outcome, String> {
        use std::sync::atomic::Ordering::SeqCst;
        assert_eq!(method, "coordinator.authority_check");
        self.calls.fetch_add(1, SeqCst);
        if self.hang.load(SeqCst) {
            std::future::pending::<()>().await;
        }
        let left = self.allow.load(SeqCst);
        let valid = left > 0;
        if valid && left != usize::MAX {
            self.allow.store(left - 1, SeqCst);
        }
        Ok(fleetdeck_proto::link::Outcome::Ok(
            serde_json::json!({ "valid": valid }),
        ))
    }
}

/// Admits a coordinator land of `session` with a deck that answers
/// `valid` until `Deck::revoke`.
pub(super) async fn admit_coordinator(
    rig: &Rig,
    session: &str,
    land_id: &str,
    plan: LandPlan,
) -> Arc<Deck> {
    let deck = Deck::new();
    rig.hub.fleet_bindings().unwrap().set_deck(
        1,
        Arc::clone(&deck) as Arc<dyn crate::coordinator::Transport>,
    );
    let authority = CommandAuthority {
        grant_id: "g1".to_owned(),
        caller_station: "other".into(),
        caller_session: "caller".into(),
        generation: 1,
    };
    let command = Rig::land_command(session, land_id, plan);
    let decided = rig
        .submit(
            &format!("cmd-{land_id}"),
            CommandOrigin::Coordinator,
            Some(authority),
            command,
        )
        .await;
    assert_eq!(
        decided.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        decided.receipt.reject
    );
    deck
}

/// The context of the publication steps of the rig.
pub(super) fn ctx(rig: &Rig) -> Ctx {
    rig.shared().0.ctx()
}

/// The run after steps 1 to 4 (phase `checked`).
pub(super) async fn checked(rig: &Rig, land_id: &str) -> LandRunRow {
    let row = rig.run(land_id).await;
    assert_eq!(row.phase, LandPhase::Checked, "{row:?}");
    row
}

async fn apply(rig: &Rig, land_id: &str, step: Step) {
    assert!(rig.store.land_step(land_id.to_owned(), step).await.unwrap());
}

/// The intent of the branch push, as the writer stores it, with no push made.
pub(super) async fn branch_intent(rig: &Rig, land_id: &str) -> LandRunRow {
    let row = rig.row(land_id).await;
    let step = Step {
        phase: Some(LandPhase::BranchPushIntent),
        pending: PendingChange::Set(Pending {
            action: PENDING_BRANCH_PUSH,
            attempt: row.attempt,
            url: rig.url.clone(),
            ref_name: format!("refs/heads/{}", row.plan.publish_ref),
            oid: row.rebased_oid.clone().unwrap(),
            lease: Some(row.branch_lease.clone().unwrap_or_default()),
            expect: None,
        }),
        outcome: OutcomeChange::Clear,
        require: Require::NoPending,
        event: true,
        ..Step::default()
    };
    apply(rig, land_id, step).await;
    rig.row(land_id).await
}

/// The branch published (phase `branch_pushed`) by the real job.
pub(super) async fn branch_done(rig: &Rig, land_id: &str) -> LandRunRow {
    branch_job(&ctx(rig), land_id).await;
    let row = rig.row(land_id).await;
    assert_eq!(row.phase, LandPhase::BranchPushed, "{row:?}");
    row
}

/// The intent of the push to the default branch, with no push made.
pub(super) async fn default_intent(rig: &Rig, land_id: &str) -> LandRunRow {
    let row = rig.row(land_id).await;
    let step = Step {
        phase: Some(LandPhase::DefaultPushIntent),
        pending: PendingChange::Set(Pending {
            action: PENDING_DEFAULT_PUSH,
            attempt: row.attempt,
            url: rig.url.clone(),
            ref_name: format!("refs/heads/{}", row.default_ref.clone().unwrap()),
            oid: row.rebased_oid.clone().unwrap(),
            lease: None,
            expect: row.base_oid.clone(),
        }),
        outcome: OutcomeChange::Clear,
        require: Require::NoPending,
        event: true,
        ..Step::default()
    };
    apply(rig, land_id, step).await;
    rig.row(land_id).await
}

/// The intent of the pull request creation, with no create made.
pub(super) async fn pr_intent(rig: &Rig, land_id: &str) -> LandRunRow {
    let row = rig.row(land_id).await;
    let step = Step {
        phase: Some(LandPhase::PrIntent),
        pending: PendingChange::Set(Pending {
            action: PENDING_PR_CREATE,
            attempt: row.attempt,
            url: "https://localhost/api/v3".to_owned(),
            ref_name: format!("refs/heads/{}", row.plan.publish_ref),
            oid: row.rebased_oid.clone().unwrap(),
            lease: None,
            expect: Some(format!(
                "{}\t{}",
                row.plan.publish_ref,
                row.default_ref.clone().unwrap()
            )),
        }),
        outcome: OutcomeChange::Clear,
        require: Require::NoPending,
        event: true,
        ..Step::default()
    };
    apply(rig, land_id, step).await;
    rig.row(land_id).await
}

/// The oid of a ref of the bare remote; `None` when it does not exist.
pub(super) fn remote_ref(rig: &Rig, name: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(&rig.bare)
        .args(["rev-parse", "--verify", "--quiet", name])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A commit on the remote `main` that the session does not have, so that the
/// rebase of a land changes the commits.
pub(super) fn move_main(rig: &Rig) {
    let other = rig.root.join("mover");
    rig.git(&rig.root, &["clone", "-q", &rig.url, "mover"]);
    rig.commit(&other, "m.txt", "moved");
    rig.git(&other, &["push", "-q", "origin", "main"]);
}
