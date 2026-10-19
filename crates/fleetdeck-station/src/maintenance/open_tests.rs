//! Tests of `maintenance.open`: the checks, the admission, the launch and
//! the repeat of a request.

use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use super::files::{self, RunStateKind};
use super::runner_client::connect;
use super::service_support::{
    Launch, OTHER_PLAN, OTHER_REQUEST, PLAN, REQUEST, Rig, id_of, peer, write_spec, write_state,
};

const NOW: i64 = 1_700_000_000_000;

fn error_of(v: &Value) -> &str {
    v["error"].as_str().unwrap()
}

async fn open_error(rig: &Rig, params: Value) -> (String, String) {
    match rig
        .svc
        .call("maintenance.open", params, peer(1), true)
        .await
    {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

/// Ends a run whose child loops on `read`: a connection and an end of file.
async fn finish(rig: &Rig, id: &str) {
    let spec = files::read_spec(rig.dir.path(), id).unwrap();
    let mut client = connect(rig.dir.path(), &spec).await.unwrap();
    client.input(&[4]).await.unwrap();
    while let Ok(frame) = client.next().await {
        if matches!(frame, super::frame::Frame::Exit(_)) {
            break;
        }
    }
    assert_eq!(rig.host.finished().await, [0]);
}

#[tokio::test]
async fn maintenance_open_refuses_a_bad_body_with_a_fixed_text() {
    let rig = Rig::with_plan(Launch::Fail);
    let hex = "a".repeat(32);
    for params in [
        json!({}),
        json!({"plan_id": PLAN}),
        json!({"plan_id": PLAN, "request_id": REQUEST, "cols": 100, "rows": 30, "x": 1}),
        json!({"plan_id": PLAN, "request_id": "not-hex", "cols": 100, "rows": 30}),
        json!({"plan_id": "ABCDEF0123456789ABCDEF0123456789", "request_id": hex, "cols": 100, "rows": 30}),
        json!({"plan_id": PLAN, "request_id": REQUEST, "cols": 0, "rows": 30}),
        json!({"plan_id": PLAN, "request_id": REQUEST, "cols": 100, "rows": 1001}),
        json!({"plan_id": PLAN, "request_id": REQUEST, "cols": 70000, "rows": 30}),
        json!(null),
        json!("text"),
    ] {
        let (code, text) = open_error(&rig, params.clone()).await;
        assert_eq!(code, "invalid_request", "{params}");
        assert_eq!(text, "The request is not valid.");
    }
    assert!(rig.specs().is_empty());
    assert_eq!(rig.host.launch_count(), 0);
}

#[tokio::test]
async fn maintenance_open_an_unknown_plan_is_plan_changed() {
    let rig = Rig::new(Launch::Fail);
    let v = rig.open(REQUEST, PLAN, true).await;
    assert_eq!(error_of(&v), "plan_changed");
    assert_eq!(
        v["text"],
        "The plan changed or expired. Review the desktop tools again."
    );
    assert!(rig.specs().is_empty());
    assert_eq!(rig.host.launch_count(), 0);
}

#[tokio::test]
async fn maintenance_open_a_plan_of_another_deck_is_plan_changed() {
    let rig = Rig::with_plan(Launch::Fail);
    let params = json!({"plan_id": PLAN, "request_id": REQUEST, "cols": 100, "rows": 30});
    let got = rig
        .svc
        .call("maintenance.open", params, peer(2), true)
        .await;
    assert!(
        matches!(&got, Outcome::Err(e) if e.code == "plan_changed"),
        "{got:?}"
    );
    assert_eq!(rig.host.launch_count(), 0);
}

#[tokio::test]
async fn maintenance_open_with_shell_off_is_shell_disabled() {
    let rig = Rig::with_plan(Launch::Fail);
    let v = rig.open(REQUEST, PLAN, false).await;
    assert_eq!(error_of(&v), "shell_disabled");
    assert!(rig.specs().is_empty());
    assert_eq!(rig.host.launch_count(), 0);
    // The plan was not used.
    assert!(rig.plans.removed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn maintenance_open_a_failed_recheck_is_transaction_refused() {
    let rig = Rig::with_plan(Launch::Fail);
    *rig.plans.recheck.lock().unwrap() = Err("transaction_refused");
    let v = rig.open(REQUEST, PLAN, true).await;
    assert_eq!(error_of(&v), "transaction_refused");
    assert_eq!(
        v["text"],
        "The package transaction needs packages that Fleet Deck does not allow. Nothing was installed."
    );
    assert!(rig.specs().is_empty());
    assert_eq!(rig.host.launch_count(), 0);
    assert!(rig.plans.removed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn maintenance_open_writes_one_spec_removes_the_plan_launches_and_answers_running() {
    let rig = Rig::with_plan(Launch::Runner(
        "while read -r a; do echo n-$a; done".to_owned(),
    ));
    let v = rig.open(REQUEST, PLAN, true).await;
    assert_eq!(v["state"], "running", "{v}");
    assert_eq!(v["request_id"], REQUEST);
    assert_eq!(v["tools"], json!(["wtype"]));
    assert_eq!(v["exit_code"], Value::Null);
    let id = v["maintenance_id"].as_str().unwrap().to_owned();
    // One spec: the durable record.
    let specs = rig.specs();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];
    assert_eq!(spec.maintenance_id, id);
    assert_eq!(spec.request_id, REQUEST);
    assert_eq!(spec.plan_id, PLAN);
    assert_eq!(spec.deck, peer(1).to_string());
    assert_eq!(spec.tools, ["wtype"]);
    assert_eq!((spec.cols, spec.rows), (100, 30));
    assert_eq!(spec.transaction.len(), 1);
    assert_ne!(spec.station_token, spec.runner_token);
    assert_eq!(v["created_at"], spec.created_at);
    // The plan is gone.
    assert_eq!(*rig.plans.removed.lock().unwrap(), [PLAN]);
    // The exact argv.
    let launches = rig.host.launches.lock().unwrap().clone();
    assert_eq!(launches.len(), 1);
    let dir = rig.dir.path().to_str().unwrap();
    let expected: Vec<String> = [
        "/usr/bin/systemd-run",
        "--user",
        "--collect",
        "--quiet",
        &format!("--unit=fleetdeck-maintenance-{id}"),
        "--service-type=exec",
        "--property=Restart=no",
        "--expand-environment=no",
        "--",
        "/opt/fleetdeck/bin/current",
        "maintenance-run",
        "--id",
        &id,
        "--dir",
        dir,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let got: Vec<String> = launches[0]
        .iter()
        .map(|a| a.to_str().unwrap().to_owned())
        .collect();
    assert_eq!(got, expected);
    let env = rig.host.envs.lock().unwrap().clone();
    assert_eq!(env[0][0].0, "PATH");
    // The same request again: the same record, no second launch, also
    // without the plan.
    let again = rig.open(REQUEST, PLAN, true).await;
    assert_eq!(again["maintenance_id"], id);
    assert_eq!(again["state"], "running");
    assert_eq!(rig.host.launch_count(), 1);
    assert_eq!(rig.specs().len(), 1);
    finish(&rig, &id).await;
    // After the exit, the same request gets the recorded result.
    let done = rig.open(REQUEST, PLAN, true).await;
    assert_eq!(done["state"], "exited");
    assert_eq!(done["exit_code"], 0);
    assert_eq!(done["post_check"], "ok");
    assert_eq!(rig.host.launch_count(), 1);
}

#[tokio::test]
async fn maintenance_open_the_same_request_with_another_plan_is_a_conflict() {
    let rig = Rig::with_plan(Launch::Fail);
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    let v = rig.open(REQUEST, OTHER_PLAN, true).await;
    assert_eq!(error_of(&v), "request_id_conflict");
    assert_eq!(v["text"], "This request was already used for another plan.");
    assert_eq!(rig.host.launch_count(), 0);
    // The same plan gets the record, whatever the plan store holds.
    let same = rig.open(REQUEST, PLAN, true).await;
    assert_eq!(same["maintenance_id"], id_of(1));
    assert_eq!(same["state"], "admitted");
}

#[tokio::test]
async fn maintenance_open_the_same_request_of_another_deck_is_a_conflict() {
    let rig = Rig::with_plan(Launch::Fail);
    let mut spec = write_spec(&rig, &id_of(1), REQUEST, NOW);
    spec.deck = peer(2).to_string();
    std::fs::remove_file(files::spec_path(rig.dir.path(), &id_of(1))).unwrap();
    files::write_spec(rig.dir.path(), &spec).unwrap();
    let v = rig.open(REQUEST, PLAN, true).await;
    assert_eq!(error_of(&v), "request_id_conflict");
    assert_eq!(rig.host.launch_count(), 0);
}

#[tokio::test]
async fn maintenance_open_a_running_or_admitted_record_is_busy() {
    let rig = Rig::with_plan(Launch::Fail);
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    write_state(&rig, &id_of(1), RunStateKind::Running, None);
    let v = rig.open(OTHER_REQUEST, PLAN, true).await;
    assert_eq!(error_of(&v), "maintenance_busy");
    assert_eq!(
        v["text"],
        "Another installation is running on this station."
    );
    // An admitted record (a spec, no state, the unit active) is busy too.
    let rig = Rig::with_plan(Launch::Fail);
    write_spec(&rig, &id_of(2), REQUEST, NOW);
    let v = rig.open(OTHER_REQUEST, PLAN, true).await;
    assert_eq!(error_of(&v), "maintenance_busy");
    assert_eq!(rig.host.launch_count(), 0);
}

#[tokio::test]
async fn maintenance_open_a_finished_record_does_not_block_a_new_run() {
    let rig = Rig::with_plan(Launch::Code(0));
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    write_state(&rig, &id_of(1), RunStateKind::Exited, Some(0));
    let v = rig.open(OTHER_REQUEST, PLAN, true).await;
    assert_eq!(v["state"], "admitted", "{v}");
    assert_ne!(v["maintenance_id"], id_of(1));
    assert_eq!(rig.host.launch_count(), 1);
}

#[tokio::test]
async fn maintenance_open_a_launch_error_leaves_the_record_admitted_and_is_never_repeated() {
    for launch in [Launch::Fail, Launch::Code(1)] {
        let rig = Rig::with_plan(launch);
        let v = rig.open(REQUEST, PLAN, true).await;
        assert_eq!(v["state"], "admitted", "{v}");
        assert_eq!(rig.specs().len(), 1);
        assert_eq!(rig.host.launch_count(), 1);
        // The same request: the same record, no second launch.
        let again = rig.open(REQUEST, PLAN, true).await;
        assert_eq!(again["maintenance_id"], v["maintenance_id"]);
        assert_eq!(again["state"], "admitted");
        assert_eq!(rig.host.launch_count(), 1);
        assert_eq!(rig.specs().len(), 1);
    }
}
