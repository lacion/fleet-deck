//! Unit tests of the Claude core: mapping, requests, inputs and arguments.

use fleetdeck_proto::command::{ApprovalDecision, UserInput};
use fleetdeck_proto::event::{
    ExpiredReason, InputState, RequestKind, ResolvedBy, ResponseChannel, RunOrigin, ToolStatus,
    TurnStatus,
};
use fleetdeck_proto::{Event, InputId, Operation, RequestId};
use serde_json::{Value, json};

use super::frames::parse_value;
use super::*;
use crate::adapter::{Answer, HostInput, NativeSessionId, ProviderError};

pub(super) fn config(allow_bypass: bool) -> CoreConfig {
    CoreConfig {
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        capabilities: capabilities(allow_bypass),
        resumed: None,
    }
}

pub(super) fn emits(out: &[Output]) -> Vec<Event> {
    out.iter()
        .filter_map(|o| match o {
            Output::Emit(e) => Some(e.clone()),
            Output::Write(_) => None,
        })
        .collect()
}

pub(super) fn writes(out: &[Output]) -> Vec<Value> {
    out.iter()
        .filter_map(|o| match o {
            Output::Write(v) => Some(v.clone()),
            Output::Emit(_) => None,
        })
        .collect()
}

pub(super) fn kinds(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

pub(super) fn feed(core: &mut ClaudeCore, v: Value) -> Vec<Output> {
    core.on_frame(parse_value(v).unwrap())
}

pub(super) fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
}

pub(super) fn input(n: u8, text: &str) -> HostInput {
    HostInput {
        id: InputId::new(uuid(n)),
        origin: RunOrigin::Board,
        input: UserInput::new(text.to_owned()),
    }
}

pub(super) fn lifecycle(core: &mut ClaudeCore, n: u8, state: &str) -> Vec<Event> {
    emits(&feed(
        core,
        json!({"type":"command_lifecycle","command_uuid":uuid(n),"state":state}),
    ))
}

pub(super) fn init(core: &mut ClaudeCore) -> Vec<Event> {
    emits(&feed(
        core,
        json!({"type":"system","subtype":"init","session_id":"s-1","model":"claude-haiku-4-5",
               "permissionMode":"default","claude_code_version":"2.1.289"}),
    ))
}

pub(super) fn result(core: &mut ClaudeCore, subtype: &str, reason: &str) -> Vec<Event> {
    emits(&feed(
        core,
        json!({"type":"result","subtype":subtype,"is_error":subtype != "success",
               "terminal_reason":reason,"total_cost_usd":0.01,
               "usage":{"input_tokens":10,"output_tokens":5}}),
    ))
}

/// A started core with one running turn (input 1).
pub(super) fn running_core() -> ClaudeCore {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    core.prompt(&input(1, "hello")).unwrap();
    lifecycle(&mut core, 1, "started");
    core
}

pub(super) fn tool_use(core: &mut ClaudeCore, id: &str, name: &str, input: Value) -> Vec<Event> {
    emits(&feed(
        core,
        json!({"type":"assistant","message":{"role":"assistant",
               "content":[{"type":"tool_use","id":id,"name":name,"input":input}]}}),
    ))
}

pub(super) fn can_use_tool(core: &mut ClaudeCore, native: &str, tool_use_id: &str) -> Vec<Event> {
    emits(&feed(
        core,
        json!({"type":"control_request","request_id":native,
               "request":{"subtype":"can_use_tool","tool_name":"Bash",
                          "input":{"command":"ls -la"},"tool_use_id":tool_use_id}}),
    ))
}

pub(super) fn answer(request: &str, option: &str) -> Answer {
    Answer {
        request: RequestId::new(request),
        decision: ApprovalDecision {
            answered_by: None,
            option_id: option.to_owned(),
            updated_input: None,
            text: None,
        },
        by: ResolvedBy::Local,
    }
}

#[test]
fn a_simple_turn_maps_to_canonical_events_in_order() {
    let mut core = ClaudeCore::new(config(false));
    let out = core.prompt(&input(1, "hello\nsecond line")).unwrap();
    assert_eq!(
        writes(&out),
        vec![
            json!({"type":"user","message":{"role":"user","content":"hello\nsecond line"},
                    "parent_tool_use_id":null,"session_id":"","uuid":uuid(1)})
        ]
    );
    // The CLI reports the input before `system/init`: the events are held.
    assert!(lifecycle(&mut core, 1, "queued").is_empty());
    assert!(lifecycle(&mut core, 1, "started").is_empty());
    let mut events = init(&mut core);
    events.extend(emits(&feed(
        &mut core,
        json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}),
    )));
    events.extend(result(&mut core, "success", "completed"));
    events.extend(lifecycle(&mut core, 1, "completed"));
    assert_eq!(
        kinds(&events),
        [
            "session.started",
            "input.updated",
            "run.started",
            "turn.started",
            "input.updated",
            "message.completed",
            "usage.updated",
            "turn.completed",
            "run.completed",
            "input.updated",
        ]
    );
    let Event::SessionStarted {
        provider_instance,
        cwd,
        capabilities: caps,
        provider_refs,
        ..
    } = &events[0]
    else {
        panic!("not session.started")
    };
    assert_eq!(provider_instance.as_str(), "claude");
    assert_eq!(cwd, "/w");
    assert!(caps.steer);
    assert_eq!(provider_refs["session_id"], "s-1");
    assert!(matches!(
        &events[2],
        Event::RunStarted { input_summary, origin: RunOrigin::Board, .. } if input_summary == "hello"
    ));
    // The run names the host input that started it (DL-179).
    assert!(matches!(
        &events[2],
        Event::RunStarted { input_id: Some(id), .. } if id.as_str() == uuid(1)
    ));
    assert!(matches!(
        &events[7],
        Event::TurnCompleted { status: TurnStatus::Done, usage: Some(u), .. }
            if u.input_tokens == Some(10) && u.cost_usd == Some(0.01)
    ));
    assert!(matches!(
        &events[8],
        Event::RunCompleted {
            status: TurnStatus::Done,
            background_pending: false,
            ..
        }
    ));
    assert!(core.is_idle());
    assert_eq!(
        core.input_state(&InputId::new(uuid(1))),
        Some(InputState::Completed)
    );
}

#[test]
fn unrelated_frames_never_confirm_a_waiting_prompt() {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    core.prompt(&input(1, "x")).unwrap();
    for frame in [
        json!({"type":"system","subtype":"status","status":"requesting"}),
        json!({"type":"system","subtype":"init","session_id":"s-1"}),
        json!({"type":"command_lifecycle","command_uuid":uuid(9),"state":"started"}),
        json!({"type":"user","isReplay":true,"uuid":uuid(1),"message":{"role":"user","content":"x"}}),
    ] {
        // The replay echo confirms only without lifecycle frames; uuid 9 is a
        // lifecycle frame, so the echo after it does not confirm.
        feed(&mut core, frame);
    }
    assert_eq!(core.pending_inputs(), 1);
    assert!(core.current_turn().is_none());
}

#[test]
fn the_replay_echo_confirms_when_the_cli_has_no_lifecycle_frames() {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    core.prompt(&input(1, "x")).unwrap();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","isReplay":true,"uuid":uuid(1),"message":{"role":"user","content":"x"}}),
    ));
    assert_eq!(kinds(&ev), ["run.started", "turn.started", "input.updated"]);
}

#[test]
fn an_input_started_during_a_turn_joins_it() {
    let mut core = running_core();
    let turn = core.current_turn().cloned().unwrap();
    core.prompt(&input(2, "also this")).unwrap();
    let ev = lifecycle(&mut core, 2, "started");
    assert_eq!(kinds(&ev), ["input.updated"]);
    assert!(
        matches!(&ev[0], Event::InputUpdated { turn_id: Some(t), state: InputState::Started, .. } if *t == turn)
    );
}

#[test]
fn approvals_use_native_ids_and_answers_are_idempotent() {
    let mut core = running_core();
    let ev = tool_use(&mut core, "toolu_1", "Bash", json!({"command":"ls -la"}));
    let Event::ToolStarted {
        node_id,
        input_summary,
        ..
    } = &ev[0]
    else {
        panic!("not tool.started")
    };
    assert_eq!(input_summary, "ls -la");
    let node_id = node_id.clone();
    let ev = can_use_tool(&mut core, "native-1", "toolu_1");
    let Event::ApprovalRequested {
        request_id,
        node_id: req_node,
        request_kind,
        details,
        options,
        response,
        input,
        ..
    } = &ev[0]
    else {
        panic!("not approval.requested")
    };
    assert_eq!(*req_node, Some(node_id));
    assert_eq!(*request_kind, RequestKind::Command);
    assert_eq!(details, "ls -la");
    assert_eq!(*response, ResponseChannel::RpcReply);
    assert_eq!(input.as_ref().unwrap()["command"], "ls -la");
    let ids: Vec<&str> = options.iter().map(|o| o.option_id.as_str()).collect();
    assert_eq!(ids, ["allow", "deny"], "no allow_always without a rule");
    let request_id = request_id.clone();
    // A repeated request with the same native id is the same request.
    assert!(can_use_tool(&mut core, "native-1", "toolu_1").is_empty());

    let bad = core.answer(&answer(request_id.as_str(), "allow_always"));
    assert!(matches!(bad, Err(ProviderError::UnknownOption { .. })));
    let unknown = core.answer(&answer("req-99", "allow"));
    assert!(matches!(unknown, Err(ProviderError::UnknownRequest(_))));

    let out = core.answer(&answer(request_id.as_str(), "allow")).unwrap();
    let frame = &writes(&out)[0];
    assert_eq!(frame["response"]["request_id"], "native-1");
    assert_eq!(frame["response"]["response"]["behavior"], "allow");
    assert_eq!(frame["response"]["response"]["toolUseID"], "toolu_1");
    assert_eq!(
        frame["response"]["response"]["updatedInput"]["command"],
        "ls -la"
    );
    assert!(matches!(
        &emits(&out)[0],
        Event::ApprovalResolved { option_id, by: ResolvedBy::Local, .. } if option_id == "allow"
    ));
    let again = core.answer(&answer(request_id.as_str(), "deny")).unwrap();
    assert!(again.is_empty(), "a second answer writes nothing (L3)");
    assert!(core.open_requests().is_empty());
}

#[test]
fn an_edited_input_replaces_the_requested_one() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_1", "Bash", json!({"command":"ls -la"}));
    can_use_tool(&mut core, "native-1", "toolu_1");
    let mut edited = answer("req-3", "allow");
    edited.decision.updated_input = Some(json!({"command":"ls"}));
    let out = core.answer(&edited).unwrap();
    assert_eq!(
        writes(&out)[0]["response"]["response"]["updatedInput"],
        json!({"command":"ls"})
    );
}

#[test]
fn a_denied_tool_completes_as_denied() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_1", "Bash", json!({"command":"rm x"}));
    can_use_tool(&mut core, "native-1", "toolu_1");
    let mut deny = answer("req-3", "deny");
    deny.decision.text = Some("not now".to_owned());
    let out = core.answer(&deny).unwrap();
    let body = &writes(&out)[0]["response"]["response"];
    assert_eq!(body["behavior"], "deny");
    assert_eq!(body["message"], "not now");
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"denied"}]}}),
    ));
    assert!(matches!(
        &ev[0],
        Event::ToolCompleted {
            status: ToolStatus::Denied,
            ..
        }
    ));
}

#[test]
fn allow_always_sends_session_scoped_allow_rules_only() {
    let mut core = running_core();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_request","request_id":"n1","request":{
            "subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},
            "permission_suggestions":[
                {"type":"addRules","behavior":"allow","destination":"localSettings","rules":[{"toolName":"Bash"}]},
                {"type":"setMode","mode":"acceptEdits","destination":"session"},
                {"type":"addDirectories","directories":["/"],"destination":"session"},
                {"type":"addRules","behavior":"deny","destination":"session","rules":[]}]}}),
    ));
    let Event::ApprovalRequested { options, .. } = &ev[0] else {
        panic!()
    };
    assert_eq!(options.len(), 3);
    let out = core.answer(&answer("req-2", "allow_always")).unwrap();
    let rules = &writes(&out)[0]["response"]["response"]["updatedPermissions"];
    assert_eq!(
        *rules,
        json!([{"type":"addRules","behavior":"allow","destination":"session","rules":[{"toolName":"Bash"}]}])
    );
}

#[test]
fn unknown_control_requests_fail_closed() {
    let mut core = running_core();
    let out = feed(
        &mut core,
        json!({"type":"control_request","request_id":"x1","request":{"subtype":"brand_new_prompt"}}),
    );
    let w = writes(&out);
    assert_eq!(w[0]["response"]["subtype"], "error");
    assert_eq!(w[0]["response"]["request_id"], "x1");
    assert!(
        matches!(&emits(&out)[0], Event::Error { code, .. } if code == "unknown_control_request")
    );
    // A permission request without a tool name is malformed: also denied.
    let out = feed(
        &mut core,
        json!({"type":"control_request","request_id":"x2","request":{"subtype":"can_use_tool"}}),
    );
    assert_eq!(writes(&out)[0]["response"]["subtype"], "error");
    for sub in ["hook_callback", "mcp_message"] {
        let out = feed(
            &mut core,
            json!({"type":"control_request","request_id":"x3","request":{"subtype":sub}}),
        );
        assert_eq!(writes(&out)[0]["response"]["subtype"], "error");
    }
    assert!(core.open_requests().is_empty());
}

#[test]
fn a_cancelled_request_expires() {
    let mut core = running_core();
    can_use_tool(&mut core, "native-1", "toolu_x");
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_cancel_request","request_id":"native-1"}),
    ));
    assert!(matches!(
        &ev[0],
        Event::ApprovalExpired {
            reason: ExpiredReason::CancelledByProvider,
            ..
        }
    ));
    let late = core.answer(&answer("req-2", "allow"));
    assert!(matches!(late, Err(ProviderError::RequestClosed { .. })));
}

#[test]
fn a_file_edit_reports_the_file() {
    let mut core = running_core();
    let ev = tool_use(
        &mut core,
        "toolu_1",
        "Write",
        json!({"file_path":"/w/a.txt","content":"x"}),
    );
    assert!(matches!(&ev[0], Event::ToolStarted { files, .. } if files == &["/w/a.txt"]));
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}),
    ));
    assert_eq!(kinds(&ev), ["file.changed", "tool.completed"]);
    assert!(
        matches!(&ev[0], Event::FileChanged { path, node_id: Some(_), .. } if path == "/w/a.txt")
    );
}

#[test]
fn plan_mode_captures_the_plan() {
    let mut core = running_core();
    let ev = tool_use(
        &mut core,
        "toolu_p",
        "ExitPlanMode",
        json!({"plan":"# Plan\n1. x"}),
    );
    assert!(matches!(
        &ev[1],
        Event::PlanProposed { plan_id, turn_id: Some(_), markdown } if plan_id.as_str() == "plan-1" && markdown == "# Plan\n1. x"
    ));
}

#[test]
fn live_deltas_map_to_message_and_reasoning_deltas() {
    let mut core = running_core();
    let delta =
        |d: Value| json!({"type":"stream_event","event":{"type":"content_block_delta","delta":d}});
    let mut ev = emits(&feed(
        &mut core,
        delta(json!({"type":"text_delta","text":"he"})),
    ));
    ev.extend(emits(&feed(
        &mut core,
        delta(json!({"type":"thinking_delta","thinking":"hm"})),
    )));
    ev.extend(emits(&feed(
        &mut core,
        delta(json!({"type":"thinking_delta","thinking":""})),
    )));
    assert_eq!(kinds(&ev), ["message.delta", "reasoning.delta"]);
}

#[test]
fn set_mode_accepts_only_the_session_modes() {
    let mut core = running_core();
    assert!(matches!(
        core.set_mode("manual"),
        Err(ProviderError::InvalidMode(_))
    ));
    assert!(matches!(
        core.set_mode("bypassPermissions"),
        Err(ProviderError::InvalidMode(_))
    ));
    let out = core.set_mode("plan").unwrap();
    let w = &writes(&out)[0];
    assert_eq!(w["request"]["subtype"], "set_permission_mode");
    assert_eq!(w["request"]["mode"], "plan");
    let id = w["request_id"].as_str().unwrap().to_owned();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{}}}),
    ));
    assert_eq!(
        ev,
        [Event::ModeChanged {
            mode: "plan".to_owned()
        }]
    );
    assert_eq!(core.mode(), Some("plan"));
    let mut open = ClaudeCore::new(config(true));
    assert!(open.set_mode("bypassPermissions").is_ok());
}

#[test]
fn a_failed_control_request_is_an_error_and_unknown_responses_are_ignored() {
    let mut core = running_core();
    let out = core.set_mode("plan").unwrap();
    let id = writes(&out)[0]["request_id"].as_str().unwrap().to_owned();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":"nope"}}),
    ));
    assert!(
        matches!(&ev[0], Event::Error { code, message, .. } if code == "control_request_failed" && message == "nope")
    );
    let echo = feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":"native-7","response":{}}}),
    );
    assert!(echo.is_empty());
}

#[test]
fn a_control_answer_before_init_is_released_at_once() {
    let mut core = ClaudeCore::new(config(false));
    let w = writes(&core.initialize().unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    let held = feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{"current_permission_mode":"default","models":[]}}}),
    );
    assert!(emits(&held).is_empty());
    let id = writes(&core.set_mode("plan").unwrap())[0]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{}}}),
    ));
    assert!(matches!(&ev[..], [Event::ModeChanged { mode }] if mode == "plan"));
}

#[test]
fn other_frames_before_init_stay_held() {
    let mut core = ClaudeCore::new(config(false));
    core.prompt(&input(1, "x")).unwrap();
    assert!(lifecycle(&mut core, 1, "queued").is_empty());
    let ev = init(&mut core);
    assert_eq!(kinds(&ev)[0], "session.started");
    assert!(kinds(&ev).contains(&"input.updated".to_owned()));
}

#[test]
fn cli_args_always_pass_a_native_mode() {
    let caps = capabilities(false);
    let args = cli_args(&Launch::default(), &caps);
    let at = args.iter().position(|a| a == "--permission-mode").unwrap();
    assert_eq!(args[at + 1], "default");
    assert!(!args.contains(&"--model".to_owned()));
    for (asked, runs) in [
        ("plan", "plan"),
        ("manual", "default"),
        ("bypassPermissions", "default"),
        ("nonsense", "default"),
    ] {
        let launch = Launch {
            mode: Some(asked.to_owned()),
            ..Launch::default()
        };
        let args = cli_args(&launch, &caps);
        assert_eq!(args[at + 1], runs, "mode {asked}");
    }
    let launch = Launch {
        model: Some("haiku".to_owned()),
        resume: Some(NativeSessionId("s-1".to_owned())),
        fork: true,
        extra_args: vec!["--safe-mode".to_owned()],
        ..Launch::default()
    };
    let args = cli_args(&launch, &capabilities(true));
    let tail: Vec<&str> = args[at + 2..].iter().map(String::as_str).collect();
    assert_eq!(
        tail,
        [
            "--model",
            "haiku",
            "--resume",
            "s-1",
            "--fork-session",
            "--safe-mode"
        ]
    );
}

#[test]
fn input_ids_become_stable_v4_uuids() {
    let ulid = InputId::new("01JZ8Y3K9W4V7T2Q5R6S8N0M1P");
    let a = input_uuid(&ulid).unwrap();
    assert_eq!(a, input_uuid(&ulid).unwrap());
    let parts: Vec<&str> = a.split('-').collect();
    assert_eq!(
        parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
        [8, 4, 4, 4, 12]
    );
    assert!(parts[2].starts_with('4'), "version 4: {a}");
    assert!(
        matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
        "variant: {a}"
    );
    let fixture = InputId::new("00000000-0003-4F7B-8000-55C4F699BCF0");
    assert_eq!(
        input_uuid(&fixture).unwrap(),
        "00000000-0003-4f7b-8000-55c4f699bcf0"
    );
    assert!(matches!(
        input_uuid(&InputId::new("not an id")),
        Err(ProviderError::InvalidInput(_))
    ));
    let mut core = ClaudeCore::new(config(false));
    core.prompt(&input(1, "a")).unwrap();
    assert!(matches!(
        core.prompt(&input(1, "again")),
        Err(ProviderError::InvalidInput(_))
    ));
}

#[test]
fn claude_capabilities_match_the_matrix() {
    let caps = capabilities(false);
    for op in Operation::ALL {
        // Claude stream-json has no conversation rollback (DL-087).
        assert_eq!(caps.supports(op), op != Operation::Revert, "{op:?}");
    }
    assert!(!caps.requests_survive_restart);
    assert!(!caps.native_turn_ids);
    assert_eq!(caps.response_channels, [ResponseChannel::RpcReply]);
    assert_eq!(caps.most_restrictive_mode(), Some(SAFEST_MODE));
    assert_eq!(
        caps.modes,
        ["default", "plan", "acceptEdits", "dontAsk", "auto"],
        "bypass only when the station allows it"
    );
    assert_eq!(
        capabilities(true).modes.last().map(String::as_str),
        Some(BYPASS_MODE)
    );
    for mode in NATIVE_MODES {
        assert!(capabilities(true).modes.iter().any(|m| m == mode));
    }
}

#[test]
fn version_output_parses() {
    assert_eq!(
        parse_version("2.1.289 (Claude Code)\n").as_deref(),
        Some("2.1.289")
    );
    assert_eq!(parse_version("error: no"), None);
    assert_eq!(parse_version(""), None);
}

#[test]
fn the_first_init_emits_the_slash_commands_without_the_terminal_only_ones() {
    let mut core = ClaudeCore::new(config(false));
    let frame = json!({"type":"system","subtype":"init","session_id":"s-1",
        "slash_commands":["compact","/doctor","review"],
        "terminal_slash_commands":["doctor"],
        "skills":["review", 7, "deploy"]});
    let events = emits(&feed(&mut core, frame.clone()));
    assert!(matches!(events[0], Event::SessionStarted { .. }));
    assert_eq!(
        events[1],
        Event::SlashCommands {
            commands: vec!["compact".to_owned(), "review".to_owned()],
            skills: vec!["review".to_owned(), "deploy".to_owned()]
        }
    );
    assert_eq!(events.len(), 2);
    // Claude sends `init` again for each turn: nothing more is emitted.
    assert!(emits(&feed(&mut core, frame)).is_empty());
}

#[test]
fn the_first_init_with_the_v1_plugin_emits_a_notice_right_after_the_session_start() {
    let mut core = ClaudeCore::new(config(false));
    let frame = json!({"type":"system","subtype":"init","session_id":"s-1",
        "slash_commands":["compact"],
        "plugins":[
            {"name":"other","path":"/p/other","source":"other@m"},
            {"name":"fleetdeck","path":"/p/fleetdeck","source":"fleetdeck@fleetdeck","version":"0.23.6"}]});
    let events = emits(&feed(&mut core, frame.clone()));
    assert!(matches!(events[0], Event::SessionStarted { .. }));
    let Event::SessionNotice { code, text } = &events[1] else {
        panic!("expected a notice, got {:?}", events[1]);
    };
    assert_eq!(code, "v1_plugin");
    assert!(text.starts_with("The Fleet Deck v1 plugin is active in this session."));
    assert!(matches!(events[2], Event::SlashCommands { .. }));
    assert_eq!(events.len(), 3);
    // Claude sends `init` again for each turn: no second notice.
    assert!(emits(&feed(&mut core, frame)).is_empty());
}

#[test]
fn an_init_without_the_v1_plugin_emits_no_notice() {
    let mut core = ClaudeCore::new(config(false));
    let frame = json!({"type":"system","subtype":"init","session_id":"s-1",
        "plugins":[{"name":"other","path":"/p/other","source":"other@m"}]});
    let events = emits(&feed(&mut core, frame));
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Event::SessionStarted { .. }));
    let mut core = ClaudeCore::new(config(false));
    let frame = json!({"type":"system","subtype":"init","session_id":"s-2","plugins":"fleetdeck"});
    assert_eq!(emits(&feed(&mut core, frame)).len(), 1);
}

#[test]
fn an_init_without_slash_commands_emits_only_the_session_start() {
    let mut core = ClaudeCore::new(config(false));
    let events = init(&mut core);
    assert_eq!(events.len(), 1);
    let frame = json!({"type":"system","subtype":"init","session_id":"s-2",
        "slash_commands":"compact","terminal_slash_commands":[1, null]});
    let mut core = ClaudeCore::new(config(false));
    assert_eq!(emits(&feed(&mut core, frame)).len(), 1);
}
