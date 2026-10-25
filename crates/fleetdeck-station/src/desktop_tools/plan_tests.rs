//! Tests of `station.desktop_tools_plan`: the classification, the
//! configuration check, the dry run, the plan and its store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::desktop_tools::{DesktopToolsPlan, ResolvedPackage, tool};

use super::etc_check::EtcRoot;
use super::plan::build;
use super::store::{MAX_PLANS, PLAN_LIFETIME_MS, PlanStore, StoredPlan};
use super::test_support::{Call, Etc, FakeExec, Rig, exit, peer};

pub(super) const T0: i64 = 1_792_400_000_000;

fn is_pacman_dry_run(c: &Call) -> bool {
    c.args.first().map(String::as_str) == Some("-Sp")
}

/// A dry run that prints `stdout` and exits with 0.
pub(super) fn dry(stdout: &'static str) -> FakeExec {
    FakeExec::new(move |c| {
        assert!(is_pacman_dry_run(c), "{c:?}");
        exit(0, stdout)
    })
}

/// A station that has only `grim`; `wlrctl` has no package.
pub(super) fn rig_with_grim() -> Rig {
    let rig = Rig::new();
    rig.tool("grim");
    rig
}

pub(super) const ALL_MISSING_LINES: &str = "extra wtype 0.4-1\nextra wf-recorder 0.5.0-1\nextra jq 1.7.1-2\n\
core systemd 256.7-1\nextra hyprland 0.45-1\nextra chromium 130.0-1\nextra libnotify 0.8-1\n";

pub(super) async fn plan_of(
    rig: &Rig,
    etc: &EtcRoot,
    exec: &FakeExec,
    store: &PlanStore,
) -> DesktopToolsPlan {
    build(exec, etc, store, &rig.base(), None, peer(1), T0)
        .await
        .unwrap()
}

pub(super) fn assert_blocked(plan: &DesktopToolsPlan, refused: &[&str]) {
    assert_eq!(plan.blocked.as_deref(), Some("transaction_refused"));
    assert_eq!(plan.refused, refused);
    assert_eq!(plan.plan_id, None);
    assert_eq!(plan.expires_at, None);
    assert_eq!(plan.command_display, None);
    assert!(plan.transaction.is_empty());
    assert!(!plan.system_upgrade);
}

#[tokio::test]
async fn nothing_installable_gives_an_answer_without_a_plan() {
    let rig = Rig::new();
    for t in [
        "wtype",
        "wf-recorder",
        "grim",
        "busctl",
        "jq",
        "Hyprland",
        "chromium",
        "notify-send",
    ] {
        rig.tool(t);
    }
    let etc = Etc::new();
    let store = PlanStore::new();
    let exec = FakeExec::new(|_| panic!("no child runs"));
    let plan = plan_of(&rig, &etc.root, &exec, &store).await;
    assert_eq!(plan.installable, Vec::<String>::new());
    assert_eq!(plan.unavailable, [tool::WLRCTL]);
    assert_eq!(plan.already_present.len(), 8);
    assert_eq!(plan.plan_id, None);
    assert_eq!(plan.expires_at, None);
    assert_eq!(plan.command_display, None);
    assert_eq!(plan.blocked, None);
    assert!(plan.refused.is_empty() && plan.transaction.is_empty());
    assert!(!plan.system_upgrade);
    assert!(store.get("any", &peer(1), T0).is_none());
}

#[tokio::test]
async fn a_valid_plan_has_the_fields_and_is_stored() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let store = PlanStore::new();
    let exec = dry(ALL_MISSING_LINES);
    let plan = plan_of(&rig, &etc.root, &exec, &store).await;
    assert_eq!(
        plan.installable,
        [
            "wtype",
            "wf_recorder",
            "busctl",
            "jq",
            "hyprland",
            "chromium",
            "notify_send"
        ]
    );
    assert_eq!(plan.unavailable, ["wlrctl"]);
    assert_eq!(plan.already_present, ["grim"]);
    assert_eq!(plan.blocked, None);
    assert!(plan.refused.is_empty());
    assert!(!plan.system_upgrade);
    let id = plan.plan_id.clone().unwrap();
    assert_eq!(id.len(), 32);
    assert!(id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    assert_eq!(plan.expires_at, Some(T0 + 120_000));
    assert_eq!(
        plan.command_display.as_deref(),
        Some(
            "sudo pacman -S --needed --noconfirm extra/wtype extra/wf-recorder core/systemd \
extra/jq extra/hyprland extra/chromium extra/libnotify"
        )
    );
    assert_eq!(plan.transaction.len(), 7);
    assert_eq!(
        plan.transaction[3],
        ResolvedPackage {
            repo: "core".to_owned(),
            name: "systemd".to_owned(),
            version: "256.7-1".to_owned(),
        }
    );
    // The dry run argv is fixed: one argument each, the qualified targets.
    let calls = exec.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program.to_str(), Some("/usr/bin/pacman"));
    assert_eq!(
        calls[0].args[..4],
        ["-Sp", "--needed", "--print-format", "%r %n %v"]
    );
    assert_eq!(calls[0].args[4..].len(), 7);
    assert_eq!(calls[0].args[4], "extra/wtype");
    assert_eq!(
        calls[0].env,
        [
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("LANG".to_owned(), "C".to_owned())
        ]
    );
    assert!(calls[0].limit <= std::time::Duration::from_secs(30));
    // The stored plan.
    let stored = store.get(&id, &peer(1), T0).unwrap();
    assert_eq!(stored.tools, plan.installable);
    assert_eq!(stored.targets.len(), 7);
    assert_eq!(stored.transaction, plan.transaction);
    assert_eq!(stored.conf_sha256.len(), 64);
    assert_eq!(stored.expires_at_ms, T0 + PLAN_LIFETIME_MS);
    assert_eq!(stored.peer, peer(1));
}

#[tokio::test]
async fn a_plan_is_found_for_the_same_peer_only_and_expires_after_120_s() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let store = PlanStore::new();
    let plan = plan_of(&rig, &etc.root, &dry(ALL_MISSING_LINES), &store).await;
    let id = plan.plan_id.unwrap();
    assert!(store.get(&id, &peer(1), T0).is_some());
    assert!(
        store.get(&id, &peer(1), T0).is_some(),
        "a read does not remove the plan"
    );
    assert!(store.get(&id, &peer(2), T0).is_none());
    assert!(store.get("0".repeat(32).as_str(), &peer(1), T0).is_none());
    assert!(store.get(&id, &peer(1), T0 + 119_999).is_some());
    assert!(store.get(&id, &peer(1), T0 + 120_000).is_none());
    assert!(
        store.get(&id, &peer(1), T0).is_none(),
        "an expired plan is dropped"
    );
    store.remove(&id);
}

#[tokio::test]
async fn remove_drops_a_plan() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let store = PlanStore::new();
    let plan = plan_of(&rig, &etc.root, &dry(ALL_MISSING_LINES), &store).await;
    let id = plan.plan_id.unwrap();
    store.remove(&id);
    assert!(store.get(&id, &peer(1), T0).is_none());
}

#[tokio::test]
async fn at_most_four_plans_are_stored_and_the_oldest_goes_first() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let store = PlanStore::new();
    let mut ids = Vec::new();
    for _ in 0..(MAX_PLANS + 2) {
        let plan = plan_of(&rig, &etc.root, &dry(ALL_MISSING_LINES), &store).await;
        ids.push(plan.plan_id.unwrap());
    }
    let alive: Vec<bool> = ids
        .iter()
        .map(|id| store.get(id, &peer(1), T0).is_some())
        .collect();
    assert_eq!(alive, [false, false, true, true, true, true]);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        ids.len()
    );
}

#[tokio::test]
async fn an_expired_plan_goes_before_the_oldest_live_one() {
    let store = PlanStore::new();
    let plan = |id: &str, expires: i64| StoredPlan {
        plan_id: id.to_owned(),
        peer: peer(1),
        tools: vec![],
        targets: vec![],
        transaction: vec![],
        conf_sha256: String::new(),
        expires_at_ms: expires,
    };
    store.insert(T0, plan("a", T0 + 10));
    for id in ["b", "c", "d"] {
        store.insert(T0 + 1, plan(id, T0 + 1_000_000));
    }
    store.insert(T0 + 20, plan("e", T0 + 1_000_000));
    let alive: Vec<bool> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|id| store.get(id, &peer(1), T0 + 21).is_some())
        .collect();
    assert_eq!(alive, [false, true, true, true, true]);
}

#[tokio::test]
async fn a_dependency_from_core_or_extra_is_kept_in_the_transaction() {
    let rig = Rig::new();
    for t in [
        "wtype",
        "wf-recorder",
        "grim",
        "busctl",
        "Hyprland",
        "chromium",
        "notify-send",
    ] {
        rig.tool(t);
    }
    let etc = Etc::new();
    let plan = plan_of(
        &rig,
        &etc.root,
        &dry("core libfoo 1-1\nextra jq 1.7.1-2\n"),
        &PlanStore::new(),
    )
    .await;
    assert_eq!(plan.installable, ["jq"]);
    assert_eq!(
        plan.command_display.as_deref(),
        Some("sudo pacman -S --needed --noconfirm extra/jq")
    );
    let names: Vec<&str> = plan.transaction.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["libfoo", "jq"]);
}
