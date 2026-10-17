//! Fleet control at the station endpoint (ADR-008 §3, §4.1, §9 test 10): the
//! catalog lists the tools by class, a call is refused at intake while the
//! station switch is off, each tool maps to its op or query, and the
//! authority round trip to the deck.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::mutation::{
    AuthorityCheck, CoordinatorOp, CoordinatorQuery, ProjectItemKind, ProjectTarget,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::bindings::Authority;
use super::tools::{catalog, to_op};
use super::*;

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl Forward for Recorder {
    async fn call(&self, _caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        self.calls.lock().unwrap().push((tool.to_owned(), args));
        Ok(json!({"state": "accepted"}))
    }
}

fn binding(classes: &[ToolClass], scope: GrantScope) -> Binding {
    Binding {
        binding_id: "b-1".to_owned(),
        grant_id: "grant-1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("native-1".to_owned()),
        generation: 1,
        classes: classes.iter().copied().chain([ToolClass::Own]).collect(),
        scope,
        state: BindingState::Active,
        control_skill: "SKILL v1".to_owned(),
    }
}

struct Rig {
    ingress: Ingress,
    bindings: Bindings,
    forward: Arc<Recorder>,
    token: String,
}

async fn rig(classes: &[ToolClass], scope: GrantScope) -> Rig {
    let bindings = Bindings::default();
    let (token, digest) = mint().unwrap();
    bindings.insert(digest, binding(classes, scope));
    let forward = Arc::new(Recorder::default());
    let ingress = Ingress::start(bindings.clone(), forward.clone(), Limits::default())
        .await
        .unwrap();
    Rig {
        ingress,
        bindings,
        forward,
        token: token.into_exposed(),
    }
}

async fn rpc(r: &Rig, msg: Value) -> Value {
    let addr: SocketAddr = r.ingress.addr;
    let body = msg.to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        addr.port(),
        r.token,
        body.len()
    );
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (_, body) = text.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
           "params": {"name": name, "arguments": args}})
}

fn listed(classes: &[ToolClass]) -> BTreeSet<String> {
    let classes: BTreeSet<ToolClass> = classes.iter().copied().collect();
    catalog(&classes)["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

const INSPECT: &[&str] = &[
    "read_session",
    "session_diff",
    "list_projects",
    "project_items",
    "list_artifacts",
    "read_artifact",
    "read_file",
    "search_files",
    "project_branches",
    "project_fetch",
];
const MANAGE: &[&str] = &[
    "resume_session",
    "rename_session",
    "set_mode",
    "move_to_project",
    "land_session",
    "move_session",
];

#[test]
fn the_catalog_lists_the_fleet_control_tools_by_class_and_never_by_the_switch() {
    use ToolClass::{Answer, Inspect, Mail, Manage, Read, Spawn};
    // The ADR-006 classes keep exactly their tools: an old grant gains nothing.
    let old = listed(&[Read, Mail, Spawn]);
    for tool in INSPECT.iter().chain(MANAGE) {
        assert!(!old.contains(*tool), "{tool}");
    }
    assert!(!old.contains("answer_request") && !old.contains("start_in_project"));
    // `inspect`: the nine read tools without the one that needs `answer` too.
    assert_eq!(listed(&[Inspect]), set(INSPECT));
    let both = listed(&[Inspect, Answer]);
    assert!(both.contains("list_needs_you") && both.contains("answer_request"));
    assert!(!listed(&[Inspect]).contains("list_needs_you"));
    assert!(!listed(&[Answer]).contains("list_needs_you"));
    // `manage`: four tools; `start_in_project` needs `spawn` and `manage`.
    let manage = listed(&[Manage]);
    assert_eq!(manage, set(MANAGE));
    assert!(!listed(&[Spawn]).contains("start_in_project"));
    assert!(listed(&[Spawn, Manage]).contains("start_in_project"));
    assert_eq!(listed(&[Answer]), set(&["answer_request"]));
    // Every tool of the catalog has a one-sentence description.
    let tools = catalog(
        &[Read, Mail, Spawn, Inspect, Manage, Answer]
            .into_iter()
            .collect(),
    );
    for tool in tools["tools"].as_array().unwrap() {
        let text = tool["description"].as_str().unwrap();
        assert!(text.ends_with('.') && text.len() > 10, "{tool}");
    }
    assert_eq!(ToolClass::parse("inspect"), Some(Inspect));
    assert_eq!(ToolClass::parse("manage"), Some(Manage));
    assert_eq!(ToolClass::parse("answer"), Some(Answer));
    assert_eq!(ToolClass::parse("admin"), None);
}

#[tokio::test]
async fn intake_refuses_a_fleet_control_call_while_the_station_switch_is_off() {
    let r = rig(&[ToolClass::Inspect], GrantScope::Lineage).await;
    // The catalog does not depend on the switch.
    let list = rpc(
        &r,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert!(list["result"]["tools"].as_array().unwrap().len() > 5);
    assert!(!r.bindings.fleet_control());
    let reply = rpc(&r, call("read_session", json!({"session_id": "s1"}))).await;
    assert_eq!(reply["result"]["isError"], json!(true));
    assert_eq!(
        reply["result"]["content"][0]["text"],
        json!("not_granted: fleet control is off on this station")
    );
    assert!(
        r.forward.calls.lock().unwrap().is_empty(),
        "never forwarded"
    );
    // With the switch on the call goes to the deck.
    r.bindings.set_fleet_control(true);
    let reply = rpc(&r, call("read_session", json!({"session_id": "s1"}))).await;
    assert_eq!(reply["result"]["isError"], json!(false));
    assert_eq!(r.forward.calls.lock().unwrap().len(), 1);
    // And off again at once: the value is read for each request.
    r.bindings.set_fleet_control(false);
    let reply = rpc(&r, call("read_session", json!({"session_id": "s1"}))).await;
    assert_eq!(reply["result"]["isError"], json!(true));
    assert_eq!(r.forward.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_adr_006_tool_under_a_fleet_control_grant_is_refused_too_but_not_under_a_lineage_grant()
{
    // Scope fleet, only ADR-006 classes: still a fleet-control grant.
    let fleet = rig(&[ToolClass::Read, ToolClass::Mail], GrantScope::Fleet).await;
    let to = json!({"station": "alpha", "session_id": "w1"});
    for tool in ["list_sessions", "send_mail"] {
        let args = if tool == "send_mail" {
            json!({"to": to, "text": "hi"})
        } else {
            json!({})
        };
        let reply = rpc(&fleet, call(tool, args)).await;
        assert_eq!(
            reply["result"]["content"][0]["text"],
            json!("not_granted: fleet control is off on this station"),
            "{tool}"
        );
    }
    assert!(fleet.forward.calls.lock().unwrap().is_empty());
    // A lineage grant of ADR-006 classes is not fleet control: the switch is
    // not consulted.
    let lineage = rig(&[ToolClass::Read, ToolClass::Mail], GrantScope::Lineage).await;
    let reply = rpc(&lineage, call("send_mail", json!({"to": to, "text": "hi"}))).await;
    assert_eq!(reply["result"]["isError"], json!(false));
    assert_eq!(lineage.forward.calls.lock().unwrap().len(), 1);
    // The session's own tools are not fleet control either.
    let reply = rpc(&fleet, call("set_focus", json!({"text": "x"}))).await;
    assert_ne!(
        reply["result"]["content"][0]["text"],
        json!("not_granted: fleet control is off on this station")
    );
}

#[test]
fn each_fleet_control_tool_maps_to_its_op_or_query() {
    let s = "s1";
    let q = |tool: &str, args: Value| {
        assert!(to_op(tool, &args).unwrap().is_none(), "{tool} is a query");
        super::fleet_tools::query(tool, &args)
    };
    assert_eq!(
        q("read_session", json!({"session_id": s})).unwrap(),
        CoordinatorQuery::SessionTail {
            session_id: s.into(),
            max_items: 50
        }
    );
    assert_eq!(
        q("read_session", json!({"session_id": s, "max_items": 200})).unwrap(),
        CoordinatorQuery::SessionTail {
            session_id: s.into(),
            max_items: 200
        }
    );
    assert!(
        q("read_session", json!({"session_id": s, "max_items": 201}))
            .unwrap_err()
            .starts_with("invalid_params")
    );
    assert_eq!(
        q("session_diff", json!({"session_id": s})).unwrap(),
        CoordinatorQuery::SessionDiff {
            session_id: s.into()
        }
    );
    assert_eq!(
        q("list_needs_you", json!({})).unwrap(),
        CoordinatorQuery::NeedsYou
    );
    assert_eq!(
        q("list_projects", json!({})).unwrap(),
        CoordinatorQuery::Projects
    );
    assert_eq!(
        q("project_items", json!({"project_id": "p1", "kind": "prs"})).unwrap(),
        CoordinatorQuery::ProjectItems {
            project_id: "p1".to_owned(),
            kind: ProjectItemKind::Prs
        }
    );
    assert_eq!(
        q("list_artifacts", json!({})).unwrap(),
        CoordinatorQuery::Artifacts { session_id: None }
    );
    assert_eq!(
        q("read_artifact", json!({"artifact_id": "a1", "version": 2})).unwrap(),
        CoordinatorQuery::Artifact {
            artifact_id: "a1".to_owned(),
            version: Some(2)
        }
    );
    assert_eq!(
        q("read_file", json!({"session_id": s, "path": "src/a.rs"})).unwrap(),
        CoordinatorQuery::FileRead {
            session_id: s.into(),
            path: "src/a.rs".to_owned()
        }
    );
    // The argument is `pattern`, as the proto field.
    assert_eq!(
        q(
            "search_files",
            json!({"session_id": s, "pattern": "todo", "mode": "content"})
        )
        .unwrap(),
        CoordinatorQuery::FileSearch {
            session_id: s.into(),
            pattern: "todo".to_owned(),
            mode: "content".to_owned()
        }
    );
    assert!(
        q(
            "search_files",
            json!({"session_id": s, "query": "x", "mode": "name"})
        )
        .is_err()
    );
    // A query refuses an argument that the schema does not have.
    assert!(
        q(
            "read_file",
            json!({"session_id": s, "path": "a", "caller": "x"})
        )
        .is_err()
    );
    assert!(q("read_file", json!({"session_id": s})).is_err());

    let op = |tool: &str, args: Value| to_op(tool, &args).map(|o| o.unwrap());
    assert_eq!(
        op(
            "resume_session",
            json!({"station": "alpha", "session_id": s})
        )
        .unwrap(),
        CoordinatorOp::Resume {
            station: "alpha".into(),
            session_id: s.into()
        }
    );
    assert_eq!(
        op(
            "rename_session",
            json!({"station": "alpha", "session_id": s, "title": "T"})
        )
        .unwrap(),
        CoordinatorOp::Rename {
            station: "alpha".into(),
            session_id: s.into(),
            title: "T".to_owned()
        }
    );
    assert_eq!(
        op(
            "set_mode",
            json!({"station": "alpha", "session_id": s, "mode": "plan"})
        )
        .unwrap(),
        CoordinatorOp::SetMode {
            station: "alpha".into(),
            session_id: s.into(),
            mode: "plan".to_owned()
        }
    );
    for (target, want) in [
        (
            json!({"kind": "project", "id": "p1"}),
            ProjectTarget::Project {
                id: "p1".to_owned(),
            },
        ),
        (json!({"kind": "none"}), ProjectTarget::None),
        (json!({"kind": "auto"}), ProjectTarget::Auto),
    ] {
        assert_eq!(
            op(
                "move_to_project",
                json!({"station": "alpha", "session_id": s, "target": target})
            )
            .unwrap(),
            CoordinatorOp::ProjectAssign {
                station: "alpha".into(),
                session_id: s.into(),
                target: want
            }
        );
    }
    // `worktree` is true when absent.
    assert_eq!(
        op(
            "start_in_project",
            json!({"project_id": "p1", "prompt": "go"})
        )
        .unwrap(),
        CoordinatorOp::ProjectStart {
            project_id: "p1".to_owned(),
            prompt: "go".to_owned(),
            provider_instance: None,
            worktree: true
        }
    );
    assert!(matches!(
        op(
            "start_in_project",
            json!({"project_id": "p1", "prompt": "go", "worktree": false, "provider_instance": "claude"})
        )
        .unwrap(),
        CoordinatorOp::ProjectStart {
            worktree: false,
            provider_instance: Some(_),
            ..
        }
    ));
    assert_eq!(
        op(
            "answer_request",
            json!({"station": "alpha", "session_id": s, "request_id": "r1", "option_id": "allow"})
        )
        .unwrap(),
        CoordinatorOp::Answer {
            station: "alpha".into(),
            session_id: s.into(),
            request_id: "r1".into(),
            option_id: Some("allow".to_owned()),
            question_answers: None,
            text: None
        }
    );
    assert!(
        op(
            "answer_request",
            json!({"station": "alpha", "session_id": s, "request_id": "r1", "caller": "x"})
        )
        .unwrap_err()
        .starts_with("invalid_params")
    );
    // The Fleet Git tools (FG-10): two queries and one op, as thin forwards.
    assert_eq!(
        q("project_branches", json!({"project_id": "p1"})).unwrap(),
        CoordinatorQuery::ProjectBranches {
            project_id: "p1".to_owned()
        }
    );
    assert_eq!(
        q("project_fetch", json!({"project_id": "p1"})).unwrap(),
        CoordinatorQuery::ProjectFetch {
            project_id: "p1".to_owned()
        }
    );
    assert!(q("project_fetch", json!({"project_id": "p1", "caller": "x"})).is_err());
    assert!(q("project_branches", json!({})).is_err());
    assert_eq!(
        op("land_session", json!({"station": "alpha", "session_id": s})).unwrap(),
        CoordinatorOp::Land {
            station: "alpha".into(),
            session_id: s.into()
        }
    );
    assert!(
        op(
            "land_session",
            json!({"station": "alpha", "session_id": s, "caller": "x"})
        )
        .unwrap_err()
        .starts_with("invalid_params")
    );
    // `move_session` (FG-9) is a thin forward of the op `move`.
    assert_eq!(
        op(
            "move_session",
            json!({"station": "alpha", "session_id": s, "to_station": "bravo"})
        )
        .unwrap(),
        CoordinatorOp::Move {
            station: "alpha".into(),
            session_id: s.into(),
            to_station: "bravo".into()
        }
    );
    assert!(
        op(
            "move_session",
            json!({"station": "alpha", "session_id": s, "to_station": "bravo", "caller": "x"})
        )
        .unwrap_err()
        .starts_with("invalid_params")
    );
    assert!(op("move_session", json!({"station": "alpha", "session_id": s})).is_err());
}

/// A deck link whose answer to `coordinator.authority_check` is set by the
/// test.
struct Deck {
    seen: Mutex<Vec<(String, Value)>>,
    reply: Mutex<Option<Result<Outcome, String>>>,
    hang: bool,
}

#[async_trait]
impl Transport for Deck {
    async fn request(
        &self,
        method: &str,
        command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, String> {
        assert!(command_id.is_none(), "a check is not a mutation");
        self.seen.lock().unwrap().push((method.to_owned(), params));
        if self.hang {
            std::future::pending::<()>().await;
        }
        self.reply.lock().unwrap().take().expect("one reply")
    }
}

fn check() -> AuthorityCheck {
    AuthorityCheck {
        grant_id: "g1".to_owned(),
        generation: 2,
        caller_station: "alpha".into(),
        caller_session: "coord-1".into(),
        target_session: "worker-1".into(),
        command_id: "cmd-1".into(),
        launch: None,
    }
}

async fn verdict(reply: Option<Result<Outcome, String>>, hang: bool) -> (Authority, Vec<Value>) {
    let bindings = Bindings::default();
    let deck = Arc::new(Deck {
        seen: Mutex::default(),
        reply: Mutex::new(reply),
        hang,
    });
    bindings.set_deck(1, deck.clone());
    let got = if hang {
        bindings
            .authority_check_within(&check(), Duration::from_millis(50))
            .await
    } else {
        bindings.authority_check(&check()).await
    };
    let seen = deck.seen.lock().unwrap().clone();
    assert!(seen.iter().all(|(m, _)| m == "coordinator.authority_check"));
    (got, seen.into_iter().map(|(_, p)| p).collect())
}

#[tokio::test]
async fn the_authority_round_trip_maps_each_answer_to_one_verdict() {
    let valid = Outcome::Ok(json!({"valid": true}));
    let (got, sent) = verdict(Some(Ok(valid)), false).await;
    assert_eq!(got, Authority::Valid);
    // One request, with exactly the facts of the check.
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0], serde_json::to_value(check()).unwrap());

    let invalid = Outcome::Ok(json!({"valid": false, "reason": "revoked"}));
    assert_eq!(verdict(Some(Ok(invalid)), false).await.0, Authority::Lost);
    let forbidden = Outcome::Err(WireError::new("forbidden", "no"));
    assert_eq!(verdict(Some(Ok(forbidden)), false).await.0, Authority::Lost);
    let other = Outcome::Err(WireError::new("feature_not_negotiated", "no v2"));
    assert_eq!(verdict(Some(Ok(other)), false).await.0, Authority::Unknown);
    let unparsable = Outcome::Ok(json!({"unexpected": 1}));
    assert_eq!(
        verdict(Some(Ok(unparsable)), false).await.0,
        Authority::Unknown
    );
    let gone = Err("the link ended".to_owned());
    assert_eq!(verdict(Some(gone), false).await.0, Authority::Unknown);
    // No answer within the deadline.
    assert_eq!(verdict(None, true).await.0, Authority::Unknown);
    // No deck link at all.
    assert_eq!(
        Bindings::default().authority_check(&check()).await,
        Authority::Unknown
    );
}

#[test]
fn the_deck_link_is_cleared_only_by_its_own_serial() {
    let bindings = Bindings::default();
    let deck = || -> Arc<dyn Transport> {
        Arc::new(Deck {
            seen: Mutex::default(),
            reply: Mutex::default(),
            hang: false,
        })
    };
    assert!(bindings.deck().is_none());
    bindings.set_deck(1, deck());
    bindings.set_deck(2, deck());
    bindings.clear_deck(1); // the older link ended: the newer stays
    assert!(bindings.deck().is_some());
    bindings.clear_deck(2);
    assert!(bindings.deck().is_none());
}

#[test]
fn a_grant_holds_a_mutating_class_and_the_session_tools_binding_does_not() {
    let bindings = Bindings::default();
    let add = |digest: &str, id: &str, session: &str, classes: &[ToolClass], grant: &str| {
        let mut b = binding(classes, GrantScope::Lineage);
        b.binding_id = id.to_owned();
        b.session_id = session.into();
        b.grant_id = grant.to_owned();
        bindings.insert(digest.to_owned(), b);
    };
    add(
        "d1",
        "b1",
        "mail",
        &[ToolClass::Read, ToolClass::Mail],
        "g1",
    );
    add("d2", "b2", "reader", &[ToolClass::Read], "g2");
    add("d3", "b3", "inspector", &[ToolClass::Inspect], "g3");
    add("d4", "b4", "answerer", &[ToolClass::Answer], "g4");
    add("d5", "b5", "plain", &[], "");
    for (session, mutating, grant) in [
        ("mail", true, true),
        ("reader", false, true),
        ("inspector", false, true),
        ("answerer", true, true),
        ("plain", false, false),
        ("unknown", false, false),
    ] {
        assert_eq!(
            bindings.holds_mutating(&session.into()),
            mutating,
            "{session}"
        );
        assert_eq!(bindings.holds_grant(&session.into()), grant, "{session}");
    }
    // A revoked binding holds nothing.
    bindings.revoke_session(&"mail".into());
    assert!(!bindings.holds_mutating(&"mail".into()));
    assert!(!bindings.holds_grant(&"mail".into()));
}
