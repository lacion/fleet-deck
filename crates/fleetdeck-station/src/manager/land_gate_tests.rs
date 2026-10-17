//! The authority checks of a coordinator land and the gated start (FG-8,
//! design §11.5, revision 3 B3, revision 4): the check is the last await
//! before every remote write, after every awaited preparation; a refusal
//! ends the run with no write process; a read is never preceded by a check.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use fleetdeck_proto::fleet_git::{LandPhase, LandState};
use tokio::task::JoinHandle;

use super::rig::*;
use super::stage::*;
use crate::manager::land::oid7;
use crate::store::land_runs::LandRunRow;

const WINDOW: [&str; 6] = [
    "prepare",
    "lock",
    "intent",
    "check_done",
    "spawn",
    "outcome",
];

fn assert_refused(row: &LandRunRow) {
    assert_eq!(row.state, LandState::Failed, "{row:?}");
    assert_eq!(row.code.as_deref(), Some("authority_lost"), "{row:?}");
    assert_eq!(row.last_write_outcome.as_deref(), Some("not_started"));
    assert!(row.pending_action.is_none() && !row.reconciling);
}

#[tokio::test]
async fn a_check_that_answers_invalid_before_the_branch_intent_ends_with_no_push() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    let deck = admit_coordinator(&rig, "s1", "land-1", rig.plan("s1")).await;
    // The check at the dispatch (a) passes; the one before the push does not.
    deck.allow.store(1, Ordering::SeqCst);
    let refs = rig.refs();
    let row = rig.run_all("land-1").await;
    assert_refused(&row);
    assert_eq!(
        row.message.as_deref(),
        Some(
            "The coordinator lost its permission during the land (the coordinator's authority \
             ended before this action ran).\nNothing was pushed."
        )
    );
    assert_eq!(rig.refs(), refs);
    assert!(rig.lines("pushes").is_empty());
    assert_eq!(deck.calls(), 2);
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_check_that_answers_invalid_before_the_default_push_names_the_published_branch() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    move_main(&rig);
    let deck = admit_coordinator(&rig, "s1", "land-1", rig.plan("s1")).await;
    deck.allow.store(2, Ordering::SeqCst);
    let main = remote_ref(&rig, "refs/heads/main");
    let row = rig.run_all("land-1").await;
    assert_refused(&row);
    let r = row.branch_published_oid.clone().unwrap();
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), Some(r.clone()));
    assert_eq!(remote_ref(&rig, "refs/heads/main"), main);
    assert_eq!(
        row.message.as_deref(),
        Some(
            format!(
                "The coordinator lost its permission during the land (the coordinator's \
                 authority ended before this action ran).\nThe rebased branch fd/alpha/s1 is \
                 on origin ({}); main did not change.",
                oid7(&r)
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn no_answer_in_five_seconds_ends_authority_unknown() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    let deck = admit_coordinator(&rig, "s1", "land-1", rig.plan("s1")).await;
    deck.hang.store(true, Ordering::SeqCst);
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Failed);
    assert_eq!(row.code.as_deref(), Some("authority_unknown"));
    assert_eq!(
        row.message.as_deref(),
        Some(
            "Fleet Deck could not confirm the coordinator's permission, so the land \
             stopped.\nNothing was pushed."
        )
    );
    assert!(rig.lines("pushes").is_empty());
}

#[tokio::test]
async fn a_reconciliation_read_has_no_check_and_a_repeated_write_has_one() {
    for done in [true, false] {
        let Some(rig) = Rig::new(false).await else {
            return;
        };
        rig.session("s1", false).await;
        move_main(&rig);
        let deck = admit_coordinator(&rig, "s1", "land-1", rig.plan("s1")).await;
        let row = checked(&rig, "land-1").await;
        let row = {
            let intent = branch_intent(&rig, "land-1").await;
            assert_eq!(intent.land_id, row.land_id);
            intent
        };
        let r = row.rebased_oid.clone().unwrap();
        if done {
            rig.git(
                &rig.home,
                &[
                    "push",
                    "-q",
                    &rig.url,
                    &format!("{r}:refs/heads/fd/alpha/s1"),
                ],
            );
        }
        let before = deck.calls();
        let row = rig.reconcile("land-1").await;
        assert_eq!(row.state, LandState::Landed, "done={done}: {row:?}");
        // Done: the read found `R`; the only check is the one before the
        // default push. Not done: the repeated branch push has one, too.
        assert_eq!(
            deck.calls() - before,
            if done { 1 } else { 2 },
            "done={done}"
        );
    }
}

/// Blocks one call of the preparation, revokes the grant while it waits,
/// and releases it: the run must end `authority_lost` with no write.
async fn revoked_while(
    rig: &Arc<Rig>,
    deck: &Arc<Deck>,
    block: &str,
    work: JoinHandle<LandRunRow>,
) -> LandRunRow {
    rig.entered(block).await;
    deck.revoke();
    rig.flag(&format!("block_{block}"), false);
    work.await.unwrap()
}

fn drive_task(rig: &Arc<Rig>) -> JoinHandle<LandRunRow> {
    let rig = Arc::clone(rig);
    tokio::spawn(async move { rig.drive("land-1").await })
}

fn reconcile_task(rig: &Arc<Rig>) -> JoinHandle<LandRunRow> {
    let rig = Arc::clone(rig);
    tokio::spawn(async move { rig.reconcile("land-1").await })
}

/// A coordinator land of `s1` at phase `checked`.
async fn coordinator_checked(rig: &Rig, pr: bool) -> Arc<Deck> {
    rig.session("s1", false).await;
    move_main(rig);
    let plan = if pr {
        rig.pr_plan("s1")
    } else {
        rig.plan("s1")
    };
    let deck = admit_coordinator(rig, "s1", "land-1", plan).await;
    checked(rig, "land-1").await;
    deck
}

#[tokio::test]
async fn a_revocation_during_a_preparation_ends_authority_lost_with_no_write() {
    // (1) The destination check of the branch push.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let deck = coordinator_checked(&rig, false).await;
    let rig = Arc::new(rig);
    let refs = rig.refs();
    rig.flag("block_remote", true);
    let row = revoked_while(&rig, &deck, "remote", drive_task(&rig)).await;
    assert_refused(&row);
    assert_eq!((rig.refs(), rig.lines("pushes").len()), (refs, 0), "(1)");

    // (2) The destination check of the default push.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let deck = coordinator_checked(&rig, false).await;
    branch_done(&rig, "land-1").await;
    let rig = Arc::new(rig);
    let main = remote_ref(&rig, "refs/heads/main");
    rig.flag("block_remote", true);
    let row = revoked_while(&rig, &deck, "remote", drive_task(&rig)).await;
    assert_refused(&row);
    assert!(
        row.message
            .as_deref()
            .unwrap()
            .contains("The rebased branch fd/alpha/s1")
    );
    assert_eq!(remote_ref(&rig, "refs/heads/main"), main, "(2)");

    // (3) `pr_find` before the pull request create.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    let deck = coordinator_checked(&rig, true).await;
    branch_done(&rig, "land-1").await;
    let rig = Arc::new(rig);
    rig.flag("block_forge_get", true);
    let row = revoked_while(&rig, &deck, "forge_get", drive_task(&rig)).await;
    assert_refused(&row);
    assert!(rig.lines("creates").is_empty(), "(3)");

    // (4) The reconciliation read before a recovery branch push.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let deck = coordinator_checked(&rig, false).await;
    branch_intent(&rig, "land-1").await;
    let rig = Arc::new(rig);
    let refs = rig.refs();
    rig.flag("block_ls-remote", true);
    let row = revoked_while(&rig, &deck, "ls-remote", reconcile_task(&rig)).await;
    assert_refused(&row);
    assert_eq!((rig.refs(), rig.lines("pushes").len()), (refs, 0), "(4)");

    // (5) The destination check of that recovery push.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let deck = coordinator_checked(&rig, false).await;
    branch_intent(&rig, "land-1").await;
    let rig = Arc::new(rig);
    let refs = rig.refs();
    rig.flag("block_remote", true);
    let row = revoked_while(&rig, &deck, "remote", reconcile_task(&rig)).await;
    assert_refused(&row);
    assert_eq!((rig.refs(), rig.lines("pushes").len()), (refs, 0), "(5)");

    // (6) The reconciliation read before a recovery default push.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let deck = coordinator_checked(&rig, false).await;
    branch_done(&rig, "land-1").await;
    default_intent(&rig, "land-1").await;
    let rig = Arc::new(rig);
    let main = remote_ref(&rig, "refs/heads/main");
    rig.flag("block_ls-remote", true);
    let row = revoked_while(&rig, &deck, "ls-remote", reconcile_task(&rig)).await;
    assert_refused(&row);
    assert_eq!(remote_ref(&rig, "refs/heads/main"), main, "(6)");

    // (7) The `pr_find` read before a recovery pull request create.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    let deck = coordinator_checked(&rig, true).await;
    branch_done(&rig, "land-1").await;
    let intent = pr_intent(&rig, "land-1").await;
    assert_eq!(intent.phase, LandPhase::PrIntent);
    let rig = Arc::new(rig);
    rig.flag("block_forge_get", true);
    let row = revoked_while(&rig, &deck, "forge_get", reconcile_task(&rig)).await;
    assert_refused(&row);
    assert!(rig.lines("creates").is_empty(), "(7)");
}

fn assert_windows(trace: &[&str], windows: usize) {
    let want: Vec<&str> = WINDOW.iter().copied().cycle().take(6 * windows).collect();
    assert_eq!(trace, want.as_slice());
    // The authority check answer is followed directly by the spawn.
    for pair in trace.windows(2) {
        if pair[0] == "check_done" {
            assert_eq!(pair[1], "spawn", "{trace:?}");
        }
    }
}

#[tokio::test]
async fn the_authority_check_is_directly_followed_by_the_spawn_of_every_write() {
    // The branch push and the default push, then the same two after a
    // restart (the recovery writes).
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    move_main(&rig);
    let deck = admit_coordinator(&rig, "s1", "land-1", rig.plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    assert_windows(&rig.locks.take_trace(), 2);
    assert!(deck.calls() >= 3);

    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let _deck = coordinator_checked(&rig, false).await;
    branch_intent(&rig, "land-1").await;
    let row = rig.reconcile("land-1").await;
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    assert_windows(&rig.locks.take_trace(), 2);

    // The pull request create.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    let _deck = coordinator_checked(&rig, true).await;
    // `coordinator_checked` ran steps 1 to 4; the run goes on from `checked`.
    let row = rig.drive("land-1").await;
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    assert_windows(&rig.locks.take_trace(), 2);
}
