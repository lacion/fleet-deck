//! Tests of `recheck`: the configuration check and the dry run again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use fleetdeck_proto::desktop_tools::code;

use super::plan::{build, recheck_with};
use super::store::{PlanStore, StoredPlan};
use super::test_support::{Etc, FakeExec, Rig, exit, peer};

const T0: i64 = 1_792_400_000_000;
const LINES: &str = "extra wtype 0.4-1\nextra jq 1.7.1-2\n";

/// A stored plan for `wtype` and `jq` (the rest of the tools exists).
async fn stored(etc: &Etc) -> StoredPlan {
    let rig = Rig::new();
    for t in [
        "wf-recorder",
        "grim",
        "busctl",
        "Hyprland",
        "chromium",
        "notify-send",
    ] {
        rig.tool(t);
    }
    let store = PlanStore::new();
    let exec = FakeExec::new(|_| exit(0, LINES));
    let plan = build(&exec, &etc.root, &store, &rig.base(), None, peer(1), T0)
        .await
        .unwrap();
    store.get(&plan.plan_id.unwrap(), &peer(1), T0).unwrap()
}

#[tokio::test]
async fn an_equal_state_passes() {
    let etc = Etc::new();
    let plan = stored(&etc).await;
    assert_eq!(plan.targets, ["extra/wtype", "extra/jq"]);
    let exec = FakeExec::new(|_| exit(0, LINES));
    assert_eq!(recheck_with(&exec, &etc.root, &plan).await, Ok(()));
    let calls = exec.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args[4..], ["extra/wtype", "extra/jq"]);
}

#[tokio::test]
async fn a_changed_hash_is_refused() {
    let etc = Etc::new();
    let plan = stored(&etc).await;
    fs::write(etc.path("pacman.conf"), "[options]\nXferCommand = /tmp/x\n").unwrap();
    let exec = FakeExec::new(|_| exit(0, LINES));
    assert_eq!(
        recheck_with(&exec, &etc.root, &plan).await,
        Err(code::TRANSACTION_REFUSED)
    );
    assert!(exec.calls().is_empty());
}

#[tokio::test]
async fn a_failed_configuration_check_is_refused() {
    let etc = Etc::new();
    let plan = stored(&etc).await;
    etc.chmod("pacman.d", 0o777);
    let exec = FakeExec::new(|_| exit(0, LINES));
    assert_eq!(
        recheck_with(&exec, &etc.root, &plan).await,
        Err(code::TRANSACTION_REFUSED)
    );
}

#[tokio::test]
async fn a_changed_transaction_is_refused() {
    let etc = Etc::new();
    let plan = stored(&etc).await;
    for lines in [
        "extra wtype 0.5-1\nextra jq 1.7.1-2\n",
        "extra wtype 0.4-1\nextra jq 1.7.1-2\ncore extra-dep 1-1\n",
        "extra jq 1.7.1-2\nextra wtype 0.4-1\n",
        "extra wtype 0.4-1\naur jq 1.7.1-2\n",
        "extra wtype 0.4-1\n",
        "",
    ] {
        let exec = FakeExec::new(move |_| exit(0, lines));
        assert_eq!(
            recheck_with(&exec, &etc.root, &plan).await,
            Err(code::TRANSACTION_REFUSED),
            "{lines:?}"
        );
    }
}

#[tokio::test]
async fn a_failed_dry_run_or_a_missing_pacman_is_refused() {
    let etc = Etc::new();
    let plan = stored(&etc).await;
    let failing = FakeExec::new(|_| exit(1, LINES));
    assert_eq!(
        recheck_with(&failing, &etc.root, &plan).await,
        Err(code::TRANSACTION_REFUSED)
    );
    let timeout = FakeExec::new(|_| super::probe::ProbeRun::Timeout);
    assert_eq!(
        recheck_with(&timeout, &etc.root, &plan).await,
        Err(code::TRANSACTION_REFUSED)
    );
    let missing = FakeExec::new(|_| exit(0, LINES)).without_pacman();
    assert_eq!(
        recheck_with(&missing, &etc.root, &plan).await,
        Err(code::TRANSACTION_REFUSED)
    );
}
