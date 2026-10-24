//! Tests of `maintenance.resize` and `maintenance.attach`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};
use tokio::time::timeout;

use super::calls::attach_with;
use super::files::{self, RunStateKind};
use super::frame::Frame;
use super::relay_tests::relay_of;
use super::runner_client::connect;
use super::service_support::{Launch, REQUEST, Rig, id_of, peer, write_spec, write_state};
use super::test_support::contains;

const WAIT: Duration = Duration::from_secs(30);
const NOW: i64 = 1_700_000_000_000;
/// A child that prints its terminal size for each line that it reads.
const SIZE_LOOP: &str = "while read -r a; do stty size; done";

async fn resize(rig: &Rig, id: &str, cols: u16, rows: u16) -> Outcome {
    rig.svc
        .call(
            "maintenance.resize",
            json!({"maintenance_id": id, "cols": cols, "rows": rows}),
            peer(1),
            true,
        )
        .await
}

fn code_of(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Err(e) => &e.code,
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

async fn opened(rig: &Rig) -> String {
    let v = rig.open(REQUEST, super::service_support::PLAN, true).await;
    assert_eq!(v["state"], "running", "{v}");
    v["maintenance_id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn maintenance_resize_refuses_a_bad_body_an_unknown_id_and_an_ended_run() {
    let rig = Rig::new(Launch::Fail);
    for params in [
        json!({}),
        json!({"maintenance_id": "not-hex", "cols": 80, "rows": 24}),
        json!({"maintenance_id": id_of(1), "cols": 0, "rows": 24}),
        json!({"maintenance_id": id_of(1), "cols": 80, "rows": 24, "x": 1}),
    ] {
        let got = rig
            .svc
            .call("maintenance.resize", params.clone(), peer(1), true)
            .await;
        assert_eq!(code_of(&got), "invalid_request", "{params}");
    }
    let unknown = resize(&rig, &id_of(1), 80, 24).await;
    assert_eq!(code_of(&unknown), "unknown_maintenance");
    // An exited run, a lost run and an admitted run have no terminal.
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    write_state(&rig, &id_of(1), RunStateKind::Exited, Some(0));
    let ended = resize(&rig, &id_of(1), 80, 24).await;
    assert_eq!(code_of(&ended), "maintenance_ended");
    write_spec(&rig, &id_of(2), "77777777777777777777777777777777", NOW);
    write_state(&rig, &id_of(2), RunStateKind::Lost, None);
    assert_eq!(
        code_of(&resize(&rig, &id_of(2), 80, 24).await),
        "maintenance_ended"
    );
    write_spec(&rig, &id_of(3), "88888888888888888888888888888888", NOW);
    assert_eq!(
        code_of(&resize(&rig, &id_of(3), 80, 24).await),
        "maintenance_ended"
    );
}

#[tokio::test]
async fn maintenance_resize_a_running_run_without_a_socket_is_ended() {
    let rig = Rig::new(Launch::Fail);
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    write_state(&rig, &id_of(1), RunStateKind::Running, None);
    let got = resize(&rig, &id_of(1), 80, 24).await;
    assert_eq!(code_of(&got), "maintenance_ended");
}

#[tokio::test]
async fn maintenance_resize_goes_through_the_live_relay_and_keeps_it_attached() {
    let rig = Rig::with_plan(Launch::Runner(SIZE_LOOP.to_owned()));
    let id = opened(&rig).await;
    let relay = relay_of(&rig.svc, &id);
    timeout(WAIT, async {
        while rig.svc.relay_sender(&id).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(resize(&rig, &id, 120, 40).await, Outcome::Ok(_)));
    // The relay still has the runner: its input reaches the child, which
    // sees the new size.
    timeout(WAIT, async {
        while !contains(&relay.port.text(), b"40 120") {
            relay.board.send(b"size\n".to_vec()).unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(!relay.port.closed.load(Ordering::SeqCst));
    relay.board.send(vec![4]).unwrap();
    relay.port.until_closed().await;
    assert_eq!(rig.host.finished().await, [0]);
}

#[tokio::test]
async fn maintenance_resize_without_a_relay_uses_a_direct_client() {
    let rig = Rig::with_plan(Launch::Runner(SIZE_LOOP.to_owned()));
    let id = opened(&rig).await;
    assert!(rig.svc.relay_sender(&id).is_none());
    assert!(matches!(resize(&rig, &id, 90, 25).await, Outcome::Ok(_)));
    let spec = files::read_spec(rig.dir.path(), &id).unwrap();
    let (mut reader, mut writer) = connect(rig.dir.path(), &spec).await.unwrap().split();
    let output = Arc::new(Mutex::new(Vec::new()));
    let collecting = tokio::spawn({
        let output = Arc::clone(&output);
        async move {
            while let Ok(frame) = reader.next().await {
                if let Frame::Output(bytes) = frame {
                    output.lock().unwrap().extend(bytes);
                }
            }
        }
    });
    timeout(WAIT, async {
        while !contains(&output.lock().unwrap(), b"25 90") {
            writer.input(b"size\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    writer.input(&[4]).await.unwrap();
    assert_eq!(rig.host.finished().await, [0]);
    collecting.await.unwrap();
}

fn known_issuer(purposes: &Arc<Mutex<Vec<String>>>) -> impl Fn(&str) -> Result<String, String> {
    let purposes = Arc::clone(purposes);
    move |purpose: &str| {
        purposes.lock().unwrap().push(purpose.to_owned());
        Ok("token-1".to_owned())
    }
}

#[tokio::test]
async fn maintenance_attach_an_unknown_id_is_unknown_maintenance_and_a_known_id_gets_a_token() {
    let rig = Rig::new(Launch::Fail);
    let purposes = Arc::new(Mutex::new(Vec::new()));
    let issue = known_issuer(&purposes);
    let svc = Some(rig.svc.as_ref());
    let params = |id: &str| json!({"maintenance_id": id});
    let got = attach_with(svc, Some(&issue), params(&id_of(1)));
    assert_eq!(code_of(&got), "unknown_maintenance");
    assert!(purposes.lock().unwrap().is_empty());
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    let got = attach_with(svc, Some(&issue), params(&id_of(1)));
    assert_eq!(got, Outcome::Ok(json!({"stream_token": "token-1"})));
    assert_eq!(
        *purposes.lock().unwrap(),
        [format!("maintenance:{}", id_of(1))]
    );
}

#[tokio::test]
async fn maintenance_attach_refuses_a_bad_body_no_service_and_no_data_channel() {
    let rig = Rig::new(Launch::Fail);
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    let purposes = Arc::new(Mutex::new(Vec::new()));
    let issue = known_issuer(&purposes);
    let svc = Some(rig.svc.as_ref());
    for params in [
        json!({}),
        json!({"maintenance_id": "../etc"}),
        json!({"maintenance_id": id_of(1), "x": 1}),
        Value::Null,
    ] {
        let got = attach_with(svc, Some(&issue), params.clone());
        assert_eq!(code_of(&got), "invalid_request", "{params}");
    }
    let params = json!({"maintenance_id": id_of(1)});
    let none = attach_with(None, Some(&issue), params.clone());
    assert_eq!(code_of(&none), "unavailable");
    let no_data = attach_with(svc, None, params);
    assert_eq!(code_of(&no_data), "unsupported");
    assert!(purposes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn maintenance_attach_a_token_that_cannot_be_issued_is_internal() {
    let rig = Rig::new(Launch::Fail);
    write_spec(&rig, &id_of(1), REQUEST, NOW);
    let issue = |_: &str| Err("too many tokens".to_owned());
    let got = attach_with(
        Some(rig.svc.as_ref()),
        Some(&issue),
        json!({"maintenance_id": id_of(1)}),
    );
    assert_eq!(code_of(&got), "internal");
}
