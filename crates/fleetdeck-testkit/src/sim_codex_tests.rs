//! Tests of the fake Codex app-server (split from `sim_codex.rs` for the
//! module size rule).

use super::*;

fn sim() -> Sim<Vec<u8>> {
    Sim {
        out: Vec::new(),
        signed_in: true,
        thread: None,
        model: SIM_MODEL.to_owned(),
        cwd: "/w".to_owned(),
        seq: 0,
        turn: None,
        background: Vec::new(),
        turns: 0,
        rollback: "ok".to_owned(),
        mcp: false,
    }
}

fn frames(sim: &mut Sim<Vec<u8>>) -> Vec<Value> {
    let text = String::from_utf8(std::mem::take(&mut sim.out)).unwrap();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn method(f: &Value) -> &str {
    f["method"].as_str().unwrap_or("")
}

fn start(sim: &mut Sim<Vec<u8>>, text: &str) -> Vec<Value> {
    sim.on_frame(&json!({"id": "h1", "method": "thread/start", "params": {"cwd": "/w"}}))
        .unwrap();
    frames(sim);
    sim.on_frame(&json!({"id": "h2", "method": "turn/start", "params": {
        "threadId": sim.thread_id(), "clientUserMessageId": "c1",
        "input": [{"type": "text", "text": text}]}}))
        .unwrap();
    frames(sim)
}

#[test]
fn echo_turn_completes() {
    let mut s = sim();
    let out = start(&mut s, "hello");
    assert_eq!(out[0]["id"], "h2");
    let last = out.last().unwrap();
    assert_eq!(method(last), "turn/completed");
    assert_eq!(last["params"]["turn"]["items"][0]["text"], "echo: hello");
}

#[test]
fn ask_waits_for_the_answer() {
    let mut s = sim();
    let out = start(&mut s, "ask touch x");
    let req = out.last().unwrap();
    assert_eq!(method(req), "item/commandExecution/requestApproval");
    let id = req["id"].clone();
    s.on_frame(&json!({"id": id, "result": {"decision": "accept"}}))
        .unwrap();
    let out = frames(&mut s);
    assert_eq!(method(&out[0]), "serverRequest/resolved");
    assert_eq!(out.last().unwrap()["params"]["turn"]["status"], "completed");
}

#[test]
fn question_waits_for_the_answers() {
    let mut s = sim();
    let out = start(&mut s, "question Which color?");
    let req = out.last().unwrap();
    assert_eq!(method(req), "item/tool/requestUserInput");
    assert_eq!(req["params"]["questions"][0]["question"], "Which color?");
    let id = req["id"].clone();
    s.on_frame(&json!({"id": id, "result": {"answers": {"choice": {"answers": ["Blue"]}}}}))
        .unwrap();
    let out = frames(&mut s);
    assert_eq!(method(&out[0]), "serverRequest/resolved");
    let last = out.last().unwrap();
    assert_eq!(last["params"]["turn"]["status"], "completed");
    assert_eq!(last["params"]["turn"]["items"][0]["text"], "answered: Blue");
}

#[test]
fn background_command_completes_on_the_second_poll() {
    let mut s = sim();
    start(&mut s, "bg sleep 1");
    s.on_frame(&json!({"id": "l1", "method": "thread/backgroundTerminals/list", "params": {}}))
        .unwrap();
    let first = frames(&mut s);
    assert_eq!(first[0]["result"]["data"].as_array().unwrap().len(), 1);
    s.on_frame(&json!({"id": "l2", "method": "thread/backgroundTerminals/list", "params": {}}))
        .unwrap();
    let second = frames(&mut s);
    assert_eq!(method(&second[0]), "item/completed");
    assert_eq!(second[1]["result"]["data"], json!([]));
}

#[test]
fn wait_turn_ends_on_steer_or_interrupt() {
    let mut s = sim();
    start(&mut s, "wait please");
    s.on_frame(&json!({"id": "s1", "method": "turn/steer", "params": {
        "clientUserMessageId": "c2", "input": [{"type": "text", "text": "stop"}]}}))
        .unwrap();
    let out = frames(&mut s);
    assert_eq!(
        out.last().unwrap()["params"]["turn"]["items"][0]["text"],
        "steered: stop"
    );
    s.turn = None;
    s.on_frame(&json!({"id": "h3", "method": "turn/start", "params": {
        "input": [{"type": "text", "text": "wait again"}]}}))
        .unwrap();
    frames(&mut s);
    s.on_frame(&json!({"id": "i1", "method": "turn/interrupt", "params": {}}))
        .unwrap();
    let out = frames(&mut s);
    assert_eq!(
        out.last().unwrap()["params"]["turn"]["status"],
        "interrupted"
    );
}
