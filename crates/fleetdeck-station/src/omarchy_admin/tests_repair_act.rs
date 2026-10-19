//! Tests of the repair actions on the station: the stdin of the repair
//! children, the exit codes, the order of the checks and the rules of the
//! params (`board-gaps.md` §3.2 to §3.4).

use std::fs;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_proto::omarchy_admin::{ActOutcome, OmarchyActResult};
use fleetdeck_proto::omarchy_repair::{
    InstallPlanInput, OmarchyAssetId, RepairFileState, RestorePlanInput,
};
use serde_json::{Value, json};

use super::Holder;
use super::repair::{PLAN_LIFETIME_MS, PlanKind};
use super::tests::{Rig, peer, result, wire_error};
use super::tests_repair::{
    OTHER_REPAIR, REPAIR, T0, act, install_facts, install_plan, put, put_json, repair_result,
    replace_params, restore_facts, restore_params, restore_plan, resume_params, rig, tail, timed,
};

#[tokio::test]
async fn omarchy_install_replace_sends_the_stored_facts_on_stdin_and_uses_the_plan_once() {
    let rig = rig();
    let plan = install_plan(&rig.admin, 1).await;
    let params = replace_params(&plan.plan_id, &["fleet_bindings"]);
    let got = result(act(&rig.admin, 1, params.clone()).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    assert_eq!(got.code, None);
    assert_eq!(got.message, None);
    assert_eq!(got.status.running, None);
    assert_eq!(
        got.repair,
        Some(repair_result(None, RepairFileState::Replaced))
    );
    // The fixed argv and the stdin of the contract.
    let runs = rig.action_runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(
        tail(&rig, 1),
        [
            "omarchy",
            "install",
            "--yes",
            "--replace-conflicts",
            "--plan-stdin"
        ]
    );
    let sent = fs::read_to_string(rig.path("rec/stdin")).unwrap();
    let input: InstallPlanInput = serde_json::from_str(&sent).unwrap();
    assert_eq!(input.facts, install_facts(false));
    assert_eq!(
        input.replace_recorded_edits,
        vec![OmarchyAssetId::FleetBindings]
    );
    // The plan is used: the same call again is `plan_changed`, no child.
    let again = result(act(&rig.admin, 1, params).await);
    assert_eq!(again.outcome, ActOutcome::Failed);
    assert_eq!(again.code.as_deref(), Some("plan_changed"));
    assert_eq!(again.repair, None);
    assert_eq!(rig.action_runs().len(), 2);
    assert_eq!(rig.admin.running(), None);
}

#[tokio::test]
async fn omarchy_install_replace_without_edits_sends_an_empty_list() {
    let rig = rig();
    let plan = install_plan(&rig.admin, 1).await;
    let params = json!({"action": "install_replace", "elevated": true,
                        "plan_id": plan.plan_id, "request_id": "r-1_A"});
    let got = result(act(&rig.admin, 1, params).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    let sent: Value =
        serde_json::from_str(&fs::read_to_string(rig.path("rec/stdin")).unwrap()).unwrap();
    assert_eq!(sent["replace_recorded_edits"], json!([]));
}

#[tokio::test]
async fn omarchy_restore_sends_the_stored_facts_and_the_asset_ids() {
    let rig = rig();
    put_json(
        &rig,
        "result.json",
        &repair_result(None, RepairFileState::Restored),
    );
    let plan = restore_plan(&rig.admin, 1, REPAIR).await;
    let got = result(act(&rig.admin, 1, restore_params(&plan.plan_id, REPAIR)).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    assert_eq!(
        got.repair,
        Some(repair_result(None, RepairFileState::Restored))
    );
    assert_eq!(tail(&rig, 1), ["omarchy", "restore", "--plan-stdin"]);
    let input: RestorePlanInput =
        serde_json::from_str(&fs::read_to_string(rig.path("rec/stdin")).unwrap()).unwrap();
    assert_eq!(input.facts, restore_facts(REPAIR));
    assert_eq!(input.asset_ids, vec![OmarchyAssetId::FleetBindings]);
    // Used once.
    let again = result(act(&rig.admin, 1, restore_params(&plan.plan_id, REPAIR)).await);
    assert_eq!(again.code.as_deref(), Some("plan_changed"));
}

#[tokio::test]
async fn omarchy_install_resume_runs_its_fixed_argv_with_the_repair_id() {
    let rig = rig();
    let got = result(act(&rig.admin, 1, resume_params(REPAIR)).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    assert_eq!(
        got.repair,
        Some(repair_result(None, RepairFileState::Replaced))
    );
    assert_eq!(tail(&rig, 0), ["omarchy", "install", "--resume", REPAIR]);
    assert!(!rig.path("rec/stdin").exists());
}

#[tokio::test]
async fn omarchy_only_the_two_plan_actions_get_a_stdin_pipe() {
    let rig = rig();
    install_plan(&rig.admin, 1).await;
    let plan = install_plan(&rig.admin, 1).await;
    result(act(&rig.admin, 1, replace_params(&plan.plan_id, &[])).await);
    result(act(&rig.admin, 1, resume_params(REPAIR)).await);
    let rplan = restore_plan(&rig.admin, 1, REPAIR).await;
    result(act(&rig.admin, 1, restore_params(&rplan.plan_id, REPAIR)).await);
    result(
        act(
            &rig.admin,
            1,
            json!({"action": "install", "elevated": true}),
        )
        .await,
    );
    let log = fs::read_to_string(rig.path("rec/fd0")).unwrap();
    let kinds: Vec<&str> = log
        .lines()
        .map(|l| if l == "/dev/null" { "null" } else { "pipe" })
        .collect();
    // install-plan, install-plan, replace, resume, restore-plan, restore, install
    assert_eq!(
        kinds,
        ["null", "null", "pipe", "null", "null", "pipe", "null"],
        "{log}"
    );
    assert!(
        log.lines()
            .filter(|l| *l != "/dev/null")
            .all(|l| l.starts_with("pipe:"))
    );
}

#[tokio::test]
async fn omarchy_the_exit_codes_of_a_repair_action_map_to_the_outcome_and_the_code() {
    // Exit 0: done with the result. Exit 4: failed with the code of the
    // result and the result. Exit 3: busy.
    for action in ["install_replace", "install_resume", "restore"] {
        for coded in [
            "plan_changed",
            "unsafe_path",
            "backup_failed",
            "repair_incomplete",
            "restore_conflict",
            "build_mismatch",
        ] {
            let rig = rig();
            let want = repair_result(Some(coded), RepairFileState::BackedUp);
            put_json(&rig, "result.json", &want);
            put(&rig, "exit", "4");
            let got = run_one(&rig, action).await;
            assert_eq!(got.outcome, ActOutcome::Failed, "{action} {coded}");
            assert_eq!(got.code.as_deref(), Some(coded), "{action}");
            assert_eq!(got.message, None);
            assert_eq!(got.repair, Some(want), "{action} {coded}");
        }
        let rig = rig();
        put(&rig, "exit", "3");
        let got = run_one(&rig, action).await;
        assert_eq!(got.outcome, ActOutcome::Failed, "{action}");
        assert_eq!(got.code.as_deref(), Some("busy"), "{action}");
        assert_eq!((got.message, got.repair), (None, None), "{action}");
    }
}

/// Runs `action` with a fresh plan when it needs one.
async fn run_one(rig: &Rig, action: &str) -> OmarchyActResult {
    let params = match action {
        "install_replace" => replace_params(&install_plan(&rig.admin, 1).await.plan_id, &[]),
        "install_resume" => resume_params(REPAIR),
        _ => restore_params(&restore_plan(&rig.admin, 1, REPAIR).await.plan_id, REPAIR),
    };
    result(act(&rig.admin, 1, params).await)
}

#[tokio::test]
async fn omarchy_a_repair_action_that_ends_in_any_other_way_is_failed_without_a_result() {
    let bad_results = [
        // Exit 0 or 4 with an empty or invalid stdout.
        ("printf ''; exit 0", "failed"),
        ("echo 'not json'; exit 0", "failed"),
        ("printf ''; exit 4", "failed"),
        ("echo '{\"x\":1}'; exit 4", "failed"),
        // Exit 4 with no code, and with a code outside the six.
        (
            "echo '{\"code\":null,\"repair_id\":null,\"files\":[]}'; exit 4",
            "failed",
        ),
        (
            "echo '{\"code\":\"busy\",\"repair_id\":null,\"files\":[]}'; exit 4",
            "failed",
        ),
        // Exit 1 and the others: no message.
        ("echo SECRET-/home/ann >&2; exit 1", "failed"),
        ("exit 2", "failed"),
        ("kill -9 $$", "failed"),
    ];
    for action in ["install_replace", "install_resume", "restore"] {
        for (body, want) in bad_results {
            // The reads that make the plans print their files; the action
            // child runs `body`.
            let rig = Rig::new(&format!(
                r#"case "$*" in
  *install-plan*) cat "$REC/install-plan.json"; exit 0;;
  *restore-plan*) cat "$REC/restore-plan.json"; exit 0;;
esac
cat >/dev/null; {body}"#
            ));
            put_json(&rig, "install-plan.json", &install_facts(false));
            put_json(&rig, "restore-plan.json", &restore_facts(REPAIR));
            let got = run_one(&rig, action).await;
            assert_eq!(got.outcome, ActOutcome::Failed, "{action} {body}");
            assert_eq!(got.code.as_deref(), Some(want), "{action} {body}");
            assert_eq!((got.message, got.repair), (None, None), "{action} {body}");
        }
    }
}

#[tokio::test]
async fn omarchy_a_repair_action_past_its_limit_or_with_too_much_output_is_failed() {
    for (body, want) in [
        ("sleep 30", "timeout"),
        ("head -c 65537 /dev/zero | tr '\\0' x", "output_too_large"),
    ] {
        let rig = Rig::with(&format!("cat >/dev/null; {body}"), |cfg| {
            cfg.limits.install = Duration::from_millis(400);
        });
        let got = result(act(&rig.admin, 1, resume_params(REPAIR)).await);
        assert_eq!(got.outcome, ActOutcome::Failed, "{body}");
        assert_eq!(got.code.as_deref(), Some(want), "{body}");
        assert_eq!((got.message, got.repair), (None, None));
        assert_eq!(rig.admin.running(), None);
    }
    let rig = Rig::with("exit 0", |cfg| {
        cfg.program = "/nonexistent/fleetdeck".into()
    });
    let got = result(act(&rig.admin, 1, resume_params(REPAIR)).await);
    assert_eq!(got.code.as_deref(), Some("failed"));
    assert_eq!(got.repair, None);
}

#[tokio::test]
async fn omarchy_a_plan_larger_than_the_stdin_cap_is_failed_and_still_used() {
    let rig = rig();
    let (admin, _) = timed(&rig);
    let hex = "ab".repeat(16);
    let mut facts = install_facts(false);
    facts.files[0].path = "x".repeat(70_000);
    admin
        .plans
        .insert(T0, hex.clone(), PlanKind::Install(facts), peer(1));
    let got = result(act(&admin, 1, replace_params(&hex, &[])).await);
    assert_eq!(got.outcome, ActOutcome::Failed);
    assert_eq!(got.code.as_deref(), Some("failed"));
    assert_eq!(got.repair, None);
    assert!(rig.action_runs().is_empty());
    assert!(admin.plans.ids().is_empty());
    assert_eq!(admin.running(), None);
}

#[tokio::test]
async fn omarchy_plan_changed_for_a_missing_expired_wrong_kind_or_other_deck_plan() {
    let rig = rig();
    let (admin, now) = timed(&rig);
    let install = install_plan(&admin, 1).await.plan_id;
    let restore = restore_plan(&admin, 1, REPAIR).await.plan_id;
    let unknown = "ab".repeat(16);
    let cases = [
        // Missing.
        ("missing", replace_params(&unknown, &[]), 1),
        // The wrong kind, for each action.
        ("kind", replace_params(&restore, &[]), 1),
        ("kind", restore_params(&install, REPAIR), 1),
        // Another repair.
        ("repair", restore_params(&restore, OTHER_REPAIR), 1),
        // Another deck link.
        ("deck", replace_params(&install, &[]), 2),
        ("deck", restore_params(&restore, REPAIR), 2),
    ];
    for (what, params, who) in cases {
        let got = result(act(&admin, who, params).await);
        assert_eq!(got.outcome, ActOutcome::Failed, "{what}");
        assert_eq!(got.code.as_deref(), Some("plan_changed"), "{what}");
        assert_eq!(
            (got.message.clone(), got.repair.clone()),
            (None, None),
            "{what}"
        );
        assert!(got.status.omarchy.present, "{what}");
        assert_eq!(got.status.running, None, "{what}");
    }
    // No repair child ran; the failed checks keep the plans (Q12).
    assert!(
        rig.action_runs()
            .iter()
            .all(|r| r.args.iter().any(|a| a.ends_with("-plan"))),
        "{:?}",
        rig.action_runs()
    );
    assert_eq!(admin.plans.ids(), [install.clone(), restore.clone()]);
    // The right deck can still use them.
    let got = result(act(&admin, 1, replace_params(&install, &[])).await);
    assert_eq!(got.outcome, ActOutcome::Done);
    // An expired plan is `plan_changed` and is dropped.
    now.fetch_add(PLAN_LIFETIME_MS, Ordering::SeqCst);
    let got = result(act(&admin, 1, restore_params(&restore, REPAIR)).await);
    assert_eq!(got.code.as_deref(), Some("plan_changed"));
    assert!(admin.plans.ids().is_empty());
    assert_eq!(admin.running(), None);
}

#[tokio::test]
async fn omarchy_the_checks_run_in_order_elevation_slot_omarchy_then_the_plan() {
    let rig = rig();
    let plan = install_plan(&rig.admin, 1).await.plan_id;
    let params = replace_params(&plan, &[]);
    // No elevation: before the slot and the plan.
    let mut unelevated = params.clone();
    unelevated["elevated"] = json!(false);
    let (code, _) = wire_error(act(&rig.admin, 1, unelevated).await);
    assert_eq!(code, "elevation_required");
    // The slot is held: `busy`, and the plan is not used.
    let held = rig.admin.try_hold(Holder::Tick).unwrap();
    let (code, _) = wire_error(act(&rig.admin, 1, params.clone()).await);
    assert_eq!(code, "busy");
    drop(held);
    assert_eq!(rig.admin.plans.ids(), std::slice::from_ref(&plan));
    // No Omarchy: `omarchy_missing`, and the plan is not used.
    let without = Rig::with("exit 0", |cfg| cfg.omarchy_env = ChildEnv::empty());
    without.admin.plans.insert(
        T0,
        plan.clone(),
        PlanKind::Install(install_facts(false)),
        peer(1),
    );
    let (code, _) = wire_error(act(&without.admin, 1, params.clone()).await);
    assert_eq!(code, "omarchy_missing");
    assert_eq!(without.admin.plans.ids(), std::slice::from_ref(&plan));
    assert!(without.runs().is_empty());
}

#[tokio::test]
async fn omarchy_the_params_of_each_action_follow_the_rules() {
    let rig = rig();
    let plan = "ab".repeat(16);
    let repair = REPAIR;
    let bad = [
        // install_replace: needs a plan, no repair, no assets.
        json!({"action": "install_replace", "elevated": true}),
        json!({"action": "install_replace", "elevated": true, "plan_id": plan, "repair_id": repair}),
        json!({"action": "install_replace", "elevated": true, "plan_id": plan, "asset_ids": ["fleet_bindings"]}),
        json!({"action": "install_replace", "elevated": true, "plan_id": "ABAB"}),
        json!({"action": "install_replace", "elevated": true, "plan_id": "AB".repeat(16)}),
        // install_resume: needs a repair, nothing else but the request ID.
        json!({"action": "install_resume", "elevated": true}),
        json!({"action": "install_resume", "elevated": true, "repair_id": "short"}),
        json!({"action": "install_resume", "elevated": true, "repair_id": repair, "plan_id": plan}),
        json!({"action": "install_resume", "elevated": true, "repair_id": repair, "replace_recorded_edits": []}),
        json!({"action": "install_resume", "elevated": true, "repair_id": repair, "asset_ids": ["fleet_bindings"]}),
        // restore: a repair, a plan and a non-empty asset list.
        json!({"action": "restore", "elevated": true, "repair_id": repair, "plan_id": plan}),
        json!({"action": "restore", "elevated": true, "repair_id": repair, "plan_id": plan, "asset_ids": []}),
        json!({"action": "restore", "elevated": true, "plan_id": plan, "asset_ids": ["fleet_bindings"]}),
        json!({"action": "restore", "elevated": true, "repair_id": repair, "asset_ids": ["fleet_bindings"]}),
        json!({"action": "restore", "elevated": true, "repair_id": repair, "plan_id": plan,
               "asset_ids": ["fleet_bindings"], "replace_recorded_edits": []}),
        // The request ID: 1 to 64 characters of [A-Za-z0-9_-].
        json!({"action": "install_resume", "elevated": true, "repair_id": repair, "request_id": ""}),
        json!({"action": "install_resume", "elevated": true, "repair_id": repair, "request_id": "a".repeat(65)}),
        json!({"action": "install_resume", "elevated": true, "repair_id": repair, "request_id": "a b"}),
        json!({"action": "install_resume", "elevated": true, "repair_id": repair, "request_id": "a/b"}),
        // The old actions carry none of the new fields.
        json!({"action": "install", "elevated": true, "plan_id": plan}),
        json!({"action": "uninstall", "elevated": true, "repair_id": repair}),
        json!({"action": "screensaver_on", "elevated": true, "asset_ids": []}),
        json!({"action": "provision_retry", "elevated": true, "request_id": "r"}),
        json!({"action": "install", "elevated": true, "replace_recorded_edits": []}),
    ];
    for params in bad {
        let (code, text) = wire_error(act(&rig.admin, 1, params.clone()).await);
        assert_eq!(code, "invalid_request", "{params}");
        assert_eq!(text, "The request is not valid.", "{params}");
    }
    assert!(rig.runs().is_empty());
    // The valid forms reach the plan check.
    for params in [
        json!({"action": "install_replace", "elevated": true, "plan_id": plan}),
        json!({"action": "install_replace", "elevated": true, "plan_id": plan,
               "replace_recorded_edits": ["fleet_bindings"], "request_id": "a".repeat(64)}),
        json!({"action": "restore", "elevated": true, "repair_id": repair, "plan_id": plan,
               "asset_ids": ["fleet_bindings", "hook_post_boot"], "request_id": "Z_-9"}),
    ] {
        let got = result(act(&rig.admin, 1, params.clone()).await);
        assert_eq!(got.code.as_deref(), Some("plan_changed"), "{params}");
    }
}
