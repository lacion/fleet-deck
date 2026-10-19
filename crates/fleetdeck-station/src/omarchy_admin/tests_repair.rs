//! Tests of the installer repair on the station: the three reads, the plan
//! store, the stdin of the repair children and the mapping of their exit
//! codes (`board-gaps.md` §3.2 to §3.4).

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::omarchy_repair::{
    AssetBase, InstallPlanFacts, InstallPlanView, JournalState, ObservedState, OmarchyAssetId,
    PlanBlock, PlanClassification, PlanFile, RepairFile, RepairFileState, RepairJournalView,
    RepairResult, RepairStatusFile, RepairStatusView, RestoreAction, RestorePlanFacts,
    RestorePlanFile, RestorePlanView,
};
use serde_json::{Value, json};

use super::repair::{Clock, MAX_PLANS, PLAN_LIFETIME_MS, PlanKind, PlanStore, valid_hex_id};
use super::tests::{Rig, peer, wire_error};
use super::{Holder, OmarchyAdmin};

pub(super) const REPAIR: &str = "0123456789abcdef0123456789abcdef";
pub(super) const OTHER_REPAIR: &str = "fedcba9876543210fedcba9876543210";
pub(super) const HASH: &str = "5e884898da28047151d0e56f8dc6292773603d0d6aabbdd62a11ef721d1542d8";
pub(super) const T0: i64 = 1_792_400_000_000;

/// The fake program: the reads print their file; every other command logs
/// its stdin file, keeps stdin when it is a pipe, and prints the result.
pub(super) const BODY: &str = r#"readlink /proc/$$/fd/0 >> "$REC/fd0"
case "$*" in
  *install-plan*) cat "$REC/install-plan.json"; exit 0;;
  *repair-status*) cat "$REC/repair-status.json"; exit 0;;
  *restore-plan*) cat "$REC/restore-plan.json"; exit 0;;
  *--plan-stdin*) cat > "$REC/stdin";;
esac
cat "$REC/result.json"
exit "$(cat "$REC/exit")"
"#;

fn plan_file(asset: OmarchyAssetId, class: PlanClassification, blocked: bool) -> PlanFile {
    PlanFile {
        asset_id: asset,
        path: format!("/home/o/{}", asset.as_str()),
        classification: class,
        base: AssetBase::Omarchy,
        rel: format!("rel/{}", asset.as_str()),
        dev: Some(7),
        ino: Some(9),
        mode: Some(0o644),
        size: Some(3),
        sha256: Some(HASH.into()),
        replaceable: !blocked,
        blocked: blocked.then_some(PlanBlock::UnsafePath),
    }
}

pub(super) fn install_facts(blocked: bool) -> InstallPlanFacts {
    InstallPlanFacts {
        build_hash: "build-1".into(),
        record_sha256: Some(HASH.into()),
        files: vec![
            plan_file(
                OmarchyAssetId::FleetPluginWidget,
                PlanClassification::UnrecordedConflict,
                false,
            ),
            plan_file(
                OmarchyAssetId::FleetBindings,
                PlanClassification::RecordedEdit,
                blocked,
            ),
            plan_file(
                OmarchyAssetId::HookThemeSet,
                PlanClassification::UnrecordedConflict,
                blocked,
            ),
        ],
    }
}

pub(super) fn restore_facts(repair: &str) -> RestorePlanFacts {
    RestorePlanFacts {
        repair_id: repair.into(),
        files: vec![RestorePlanFile {
            asset_id: OmarchyAssetId::FleetBindings,
            path: "/home/o/fleet_bindings".into(),
            target_sha256: Some(HASH.into()),
            backup_path: "/home/o/fleet_bindings.kept".into(),
            backup_sha256: Some(HASH.into()),
            action: RestoreAction::MoveAsideThenCopyBackup,
        }],
    }
}

pub(super) fn repair_result(code: Option<&str>, state: RepairFileState) -> RepairResult {
    RepairResult {
        code: code.map(str::to_owned),
        repair_id: Some(REPAIR.into()),
        files: vec![
            RepairFile {
                asset_id: OmarchyAssetId::FleetBindings,
                path: "/home/o/fleet_bindings".into(),
                backup_path: Some("/home/o/fleet_bindings.kept".into()),
                state,
                kept_path: None,
            },
            RepairFile {
                asset_id: OmarchyAssetId::HookThemeSet,
                path: "/home/o/hook".into(),
                backup_path: None,
                state: RepairFileState::Unchanged,
                kept_path: None,
            },
        ],
    }
}

pub(super) fn put(rig: &Rig, name: &str, text: &str) {
    fs::write(rig.path(&format!("rec/{name}")), text).unwrap();
}

pub(super) fn put_json(rig: &Rig, name: &str, value: &impl serde::Serialize) {
    put(rig, name, &serde_json::to_string(value).unwrap());
}

/// A rig for the repair tests: every file of the fake program has a default.
pub(super) fn rig() -> Rig {
    let rig = Rig::new(BODY);
    put_json(&rig, "install-plan.json", &install_facts(false));
    put_json(&rig, "restore-plan.json", &restore_facts(REPAIR));
    put_json(
        &rig,
        "repair-status.json",
        &RepairStatusView {
            journals: Vec::new(),
        },
    );
    put_json(
        &rig,
        "result.json",
        &repair_result(None, RepairFileState::Replaced),
    );
    put(&rig, "exit", "0");
    rig
}

/// A service on the config of `rig` with a clock that the test moves.
pub(super) fn timed(rig: &Rig) -> (Arc<OmarchyAdmin>, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(T0));
    let clock = Arc::clone(&now);
    let admin = OmarchyAdmin::with_clock(
        rig.admin.cfg.clone(),
        Clock::new(move || clock.load(Ordering::SeqCst)),
    );
    (Arc::new(admin), now)
}

pub(super) fn view<T: serde::de::DeserializeOwned>(outcome: Outcome) -> T {
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        Outcome::Err(e) => panic!("wire error: {e}"),
    }
}

pub(super) async fn act(admin: &Arc<OmarchyAdmin>, who: u8, params: Value) -> Outcome {
    admin.act_call(params, peer(who)).await
}

pub(super) fn replace_params(plan: &str, edits: &[&str]) -> Value {
    json!({"action": "install_replace", "elevated": true, "plan_id": plan,
           "replace_recorded_edits": edits})
}

pub(super) fn restore_params(plan: &str, repair: &str) -> Value {
    json!({"action": "restore", "elevated": true, "plan_id": plan,
           "repair_id": repair, "asset_ids": ["fleet_bindings"]})
}

pub(super) fn resume_params(repair: &str) -> Value {
    json!({"action": "install_resume", "elevated": true, "repair_id": repair})
}

pub(super) async fn install_plan(admin: &Arc<OmarchyAdmin>, who: u8) -> InstallPlanView {
    view(admin.install_plan_call(json!({}), peer(who)).await)
}

pub(super) async fn restore_plan(
    admin: &Arc<OmarchyAdmin>,
    who: u8,
    repair: &str,
) -> RestorePlanView {
    view(
        admin
            .restore_plan_call(json!({"repair_id": repair}), peer(who))
            .await,
    )
}

pub(super) fn tail(rig: &Rig, run: usize) -> Vec<String> {
    rig.action_runs()[run]
        .args
        .iter()
        .skip(2)
        .cloned()
        .collect()
}

// --- the reads -----------------------------------------------------------

#[tokio::test]
async fn omarchy_install_plan_answers_a_view_without_the_facts_and_keeps_the_facts() {
    let rig = rig();
    let (admin, _) = timed(&rig);
    let got = install_plan(&admin, 1).await;
    assert!(valid_hex_id(&got.plan_id), "{}", got.plan_id);
    assert_eq!(got.expires_at, T0 + PLAN_LIFETIME_MS);
    let files: Vec<_> = got
        .files
        .iter()
        .map(|f| (f.asset_id, f.classification, f.replaceable))
        .collect();
    assert_eq!(
        files,
        vec![
            (
                OmarchyAssetId::FleetPluginWidget,
                PlanClassification::UnrecordedConflict,
                true
            ),
            (
                OmarchyAssetId::FleetBindings,
                PlanClassification::RecordedEdit,
                true
            ),
            (
                OmarchyAssetId::HookThemeSet,
                PlanClassification::UnrecordedConflict,
                true
            ),
        ]
    );
    assert_eq!(got.files[0].path, "/home/o/fleet_plugin_widget");
    assert_eq!(got.blocked, None);
    let text = serde_json::to_string(&got).unwrap();
    for hidden in [HASH, "sha256", "build-1", "\"ino\"", "\"dev\"", "rel/"] {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    assert_eq!(admin.plans.ids(), vec![got.plan_id.clone()]);
    // One child ran: no status child, no action slot.
    assert_eq!(
        rig.runs().len(),
        1,
        "{:?}",
        rig.runs().iter().map(|r| &r.args).collect::<Vec<_>>()
    );
    assert_eq!(tail(&rig, 0), ["omarchy", "install-plan", "--json"]);
    assert_eq!(rig.runs()[0].args[0], "--data-dir");
}

#[tokio::test]
async fn omarchy_install_plan_blocked_is_the_code_of_the_first_blocked_file() {
    let rig = rig();
    put_json(&rig, "install-plan.json", &install_facts(true));
    let got = install_plan(&rig.admin, 1).await;
    assert_eq!(got.blocked.as_deref(), Some("unsafe_path"));
    assert!(!got.files[1].replaceable && !got.files[2].replaceable);
    // The plan is stored: the view is not the end of it.
    assert_eq!(rig.admin.plans.ids(), vec![got.plan_id]);
}

#[tokio::test]
async fn omarchy_the_reads_take_no_action_slot_and_check_their_params() {
    let rig = rig();
    let _held = rig.admin.try_hold(Holder::Tick).unwrap();
    install_plan(&rig.admin, 1).await;
    let status: RepairStatusView = view(rig.admin.repair_status_call(Value::Null).await);
    assert!(status.journals.is_empty());
    restore_plan(&rig.admin, 1, REPAIR).await;
    let runs = rig.runs().len();
    for params in [json!({"x": 1}), json!([]), json!("a")] {
        for call in [
            rig.admin.install_plan_call(params.clone(), peer(1)).await,
            rig.admin.repair_status_call(params.clone()).await,
        ] {
            let (code, text) = wire_error(call);
            assert_eq!(code, "invalid_request", "{params}");
            assert_eq!(text, "The request is not valid.");
        }
    }
    assert_eq!(rig.runs().len(), runs);
}

#[tokio::test]
async fn omarchy_repair_status_answers_the_view_of_the_cli() {
    let rig = rig();
    let want = RepairStatusView {
        journals: vec![RepairJournalView {
            repair_id: REPAIR.into(),
            created_at: T0,
            state: JournalState::Incomplete,
            files: vec![RepairStatusFile {
                asset_id: OmarchyAssetId::FleetBindings,
                state: ObservedState::BackedUp,
                backup_path: Some("/home/o/fleet_bindings.kept".into()),
            }],
        }],
    };
    put_json(&rig, "repair-status.json", &want);
    for params in [Value::Null, json!({})] {
        let got: RepairStatusView = view(rig.admin.repair_status_call(params).await);
        assert_eq!(got, want);
    }
    assert_eq!(tail(&rig, 0), ["omarchy", "repair-status", "--json"]);
}

#[tokio::test]
async fn omarchy_restore_plan_runs_its_fixed_argv_and_answers_a_view() {
    let rig = rig();
    let (admin, _) = timed(&rig);
    let got = restore_plan(&admin, 1, REPAIR).await;
    assert!(valid_hex_id(&got.plan_id));
    assert_eq!(got.repair_id, REPAIR);
    assert_eq!(got.expires_at, T0 + PLAN_LIFETIME_MS);
    assert_eq!(got.files.len(), 1);
    assert_eq!(got.files[0].asset_id, OmarchyAssetId::FleetBindings);
    assert_eq!(got.files[0].path, "/home/o/fleet_bindings");
    assert_eq!(got.files[0].action, RestoreAction::MoveAsideThenCopyBackup);
    let text = serde_json::to_string(&got).unwrap();
    assert!(!text.contains(HASH) && !text.contains(".kept"), "{text}");
    assert_eq!(
        tail(&rig, 0),
        ["omarchy", "restore-plan", "--repair", REPAIR, "--json"]
    );
}

#[tokio::test]
async fn omarchy_restore_plan_checks_its_params_before_any_child() {
    let rig = rig();
    for params in [
        json!({}),
        Value::Null,
        json!({"repair_id": "ABCDEF0123456789abcdef0123456789"}),
        json!({"repair_id": "0123456789abcdef"}),
        json!({"repair_id": "0123456789abcdef0123456789abcdeg"}),
        json!({"repair_id": 5}),
        json!({"repair_id": REPAIR, "path": "/x"}),
    ] {
        let (code, text) = wire_error(rig.admin.restore_plan_call(params.clone(), peer(1)).await);
        assert_eq!(code, "invalid_request", "{params}");
        assert_eq!(text, "The request is not valid.");
    }
    assert!(rig.runs().is_empty());
    // Facts for another repair are not trusted.
    put_json(&rig, "restore-plan.json", &restore_facts(OTHER_REPAIR));
    let (code, _) = wire_error(
        rig.admin
            .restore_plan_call(json!({"repair_id": REPAIR}), peer(1))
            .await,
    );
    assert_eq!(code, "failed");
    assert!(rig.admin.plans.ids().is_empty());
}

#[tokio::test]
async fn omarchy_a_failed_read_is_busy_or_failed_with_a_fixed_text() {
    for (body, want, text) in [
        (
            "exit 3",
            "busy",
            "Another Omarchy action is running on this station.",
        ),
        ("exit 1", "failed", "The Omarchy repair command failed."),
        ("exit 4", "failed", "The Omarchy repair command failed."),
        ("kill -9 $$", "failed", "The Omarchy repair command failed."),
    ] {
        for read in ["install_plan", "repair_status", "restore_plan"] {
            let rig = Rig::new(&format!(
                "echo SECRET-TEXT-/home/ann >&2; echo 'not json'; {body}"
            ));
            let outcome = match read {
                "install_plan" => rig.admin.install_plan_call(json!({}), peer(1)).await,
                "repair_status" => rig.admin.repair_status_call(json!({})).await,
                _ => {
                    rig.admin
                        .restore_plan_call(json!({"repair_id": REPAIR}), peer(1))
                        .await
                }
            };
            let (code, got) = wire_error(outcome);
            assert_eq!((code.as_str(), got.as_str()), (want, text), "{read} {body}");
            assert!(rig.admin.plans.ids().is_empty());
        }
    }
}

#[tokio::test]
async fn omarchy_a_read_with_an_unparsable_or_oversized_answer_or_a_timeout_is_failed() {
    let text = "The Omarchy repair command failed.";
    // Exit 0 with a stdout that is not the type.
    let rig = Rig::new("echo '{\"journals\": 5}'");
    for outcome in [
        rig.admin.repair_status_call(json!({})).await,
        rig.admin.install_plan_call(json!({}), peer(1)).await,
        rig.admin
            .restore_plan_call(json!({"repair_id": REPAIR}), peer(1))
            .await,
    ] {
        assert_eq!(wire_error(outcome), ("failed".into(), text.into()));
    }
    // Too much output.
    let rig = Rig::new("head -c 65537 /dev/zero | tr '\\0' x");
    let got = wire_error(rig.admin.install_plan_call(json!({}), peer(1)).await);
    assert_eq!(got, ("failed".into(), text.into()));
    // The `status` limit.
    let rig = Rig::with("sleep 5", |cfg| {
        cfg.limits.status = Duration::from_millis(300)
    });
    let got = wire_error(rig.admin.repair_status_call(json!({})).await);
    assert_eq!(got, ("failed".into(), text.into()));
    // A program that does not start.
    let rig = Rig::with("exit 0", |cfg| {
        cfg.program = "/nonexistent/fleetdeck".into()
    });
    let got = wire_error(rig.admin.repair_status_call(json!({})).await);
    assert_eq!(got, ("failed".into(), text.into()));
}

// --- the plan store ------------------------------------------------------

fn install_kind() -> PlanKind {
    PlanKind::Install(install_facts(false))
}

fn restore_kind(repair: &str) -> PlanKind {
    PlanKind::Restore {
        repair_id: repair.into(),
        facts: restore_facts(repair),
    }
}

#[test]
fn omarchy_the_store_keeps_four_plans_and_a_fifth_removes_the_oldest() {
    let store = PlanStore::default();
    assert_eq!(MAX_PLANS, 4);
    for n in 0..5_i64 {
        store.insert(T0 + n, format!("p{n}"), install_kind(), peer(1));
    }
    assert_eq!(store.ids(), ["p1", "p2", "p3", "p4"]);
    assert!(store.take_install(T0 + 5, "p0", peer(1)).is_none());
    assert!(store.take_install(T0 + 5, "p1", peer(1)).is_some());
    assert_eq!(store.ids(), ["p2", "p3", "p4"]);
}

#[test]
fn omarchy_a_plan_lasts_two_minutes_and_an_expired_plan_is_dropped_at_the_lookup() {
    assert_eq!(PLAN_LIFETIME_MS, 120_000);
    let store = PlanStore::default();
    store.insert(T0, "a".into(), install_kind(), peer(1));
    // The last millisecond before the expiry still finds it.
    assert!(
        store
            .take_install(T0 + PLAN_LIFETIME_MS - 1, "a", peer(1))
            .is_some()
    );
    store.insert(T0, "b".into(), install_kind(), peer(1));
    assert!(
        store
            .take_install(T0 + PLAN_LIFETIME_MS, "b", peer(1))
            .is_none()
    );
    assert!(store.ids().is_empty(), "the expired plan is dropped");
    // A new plan drops the expired ones before it counts the four.
    for n in 0..4_i64 {
        store.insert(T0, format!("old{n}"), install_kind(), peer(1));
    }
    store.insert(T0 + PLAN_LIFETIME_MS, "new".into(), install_kind(), peer(1));
    assert_eq!(store.ids(), ["new"]);
}

#[test]
fn omarchy_a_plan_has_a_kind_a_deck_and_a_repair_and_a_used_plan_is_gone() {
    let store = PlanStore::default();
    store.insert(T0, "i".into(), install_kind(), peer(1));
    store.insert(T0, "r".into(), restore_kind(REPAIR), peer(1));
    // The wrong kind, another deck and another repair do not take a plan,
    // and do not remove it.
    assert!(store.take_restore(T0, "i", peer(1), REPAIR).is_none());
    assert!(store.take_install(T0, "r", peer(1)).is_none());
    assert!(store.take_install(T0, "i", peer(2)).is_none());
    assert!(store.take_restore(T0, "r", peer(2), REPAIR).is_none());
    assert!(store.take_restore(T0, "r", peer(1), OTHER_REPAIR).is_none());
    assert_eq!(store.ids(), ["i", "r"]);
    // An unknown ID.
    assert!(store.take_install(T0, "nope", peer(1)).is_none());
    // The right call takes the plan once.
    assert_eq!(
        store.take_install(T0, "i", peer(1)),
        Some(install_facts(false))
    );
    assert!(store.take_install(T0, "i", peer(1)).is_none());
    assert_eq!(
        store.take_restore(T0, "r", peer(1), REPAIR),
        Some(restore_facts(REPAIR))
    );
    assert!(store.ids().is_empty());
}

#[tokio::test]
async fn omarchy_the_plan_ids_are_32_random_hex_characters() {
    let rig = rig();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        let id = install_plan(&rig.admin, 1).await.plan_id;
        assert!(valid_hex_id(&id), "{id}");
        assert!(seen.insert(id));
    }
    assert!(!valid_hex_id("ABCDEF0123456789abcdef0123456789"));
    assert!(!valid_hex_id("0123456789abcdef0123456789abcde"));
    // Six plans were made: the store keeps the last four.
    assert_eq!(rig.admin.plans.ids().len(), 4);
}
