use serde_json::{Value, json};

use super::*;

fn call(v: Value) -> DesktopCall {
    serde_json::from_value(v).unwrap()
}

fn query(v: Value) -> DesktopQuery {
    serde_json::from_value(v).unwrap()
}

fn notify(title: &str, body: &str) -> DesktopCall {
    call(json!({"tool": "notify", "arguments": {"request_id": "r1", "title": title, "body": body}}))
}

#[test]
fn a_call_has_the_tool_and_arguments_keys_and_defaults_are_filled() {
    let c = notify("hi", "");
    let DesktopCall::Notify(a) = &c else {
        panic!("not a notify")
    };
    assert_eq!(a.urgency, CoordUrgency::Normal);
    assert_eq!(a.station, None);
    // `station` is skipped when absent; the urgency default is written.
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        json!({"tool": "notify", "arguments": {
            "request_id": "r1", "title": "hi", "body": "", "urgency": "normal"}})
    );
    let w = call(json!({"tool": "wall_show", "arguments": {
        "request_id": "r2", "display_id": "d1",
        "content": {"kind": "session", "session": {"station_id": "s", "session_id": "x"}}}}));
    let DesktopCall::WallShow(a) = &w else {
        panic!("not a wall_show")
    };
    assert!(!a.pin);
}

#[test]
fn unknown_fields_and_tools_are_refused_at_every_level() {
    for bad in [
        json!({"tool": "notify", "arguments": {"request_id": "r", "title": "t", "body": "", "x": 1}}),
        json!({"tool": "notify", "arguments": {"request_id": "r", "title": "t"}}),
        json!({"tool": "notify", "arguments": {"request_id": "r", "title": "t", "body": "",
            "urgency": "critical"}}),
        json!({"tool": "terminal_open", "arguments": {}}),
        json!({"tool": "desktop_screenshot", "arguments": {"request_id": "r", "monitor": "DP-1"}}),
        json!({"tool": "screenshot", "arguments": {"request_id": "r", "monitor": "DP-1"}, "x": 1}),
        json!({"tool": "wall_show", "arguments": {"request_id": "r", "display_id": "d",
            "content": {"kind": "artifact", "artifact": {"station_id": "s",
                "artifact_id": "a", "version": 1, "x": 1}}}}),
        json!({"tool": "wall_show", "arguments": {"request_id": "r", "display_id": "d",
            "content": {"kind": "url", "url": "http://x"}}}),
    ] {
        assert!(
            serde_json::from_value::<DesktopCall>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    assert!(
        serde_json::from_value::<DesktopQuery>(
            json!({"tool": "capture_status", "arguments": {"request_id": "r", "x": 1}})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<DesktopQuery>(json!({"tool": "notify", "arguments": {}})).is_err()
    );
}

#[test]
fn a_title_is_1_to_80_scalars_and_a_body_0_to_300() {
    assert!(notify("a", "").validate().is_ok());
    assert!(notify(&"é".repeat(80), &"é".repeat(300)).validate().is_ok());
    assert_eq!(notify("", "").validate(), Err(ArgError { field: "title" }));
    assert_eq!(
        notify(&"é".repeat(81), "").validate(),
        Err(ArgError { field: "title" })
    );
    assert_eq!(
        notify("a", &"é".repeat(301)).validate(),
        Err(ArgError { field: "body" })
    );
}

#[test]
fn ids_and_monitors_follow_the_schema_patterns() {
    let shot = |request: &str, station: Option<&str>, monitor: &str| {
        let mut args = json!({"request_id": request, "monitor": monitor});
        if let Some(s) = station {
            args["station"] = json!(s);
        }
        call(json!({"tool": "screenshot", "arguments": args}))
    };
    assert!(shot("r_1-A", Some("abc123"), "DP-1").validate().is_ok());
    assert!(shot("r", None, "eDP-1.a_b").validate().is_ok());
    assert!(
        shot(&"a".repeat(64), None, &"m".repeat(64))
            .validate()
            .is_ok()
    );
    assert!(shot("r", Some(&"s".repeat(128)), "DP-1").validate().is_ok());
    for (c, field) in [
        (shot("", None, "DP-1"), "request_id"),
        (shot(&"a".repeat(65), None, "DP-1"), "request_id"),
        (shot("a.b", None, "DP-1"), "request_id"),
        (shot("a b", None, "DP-1"), "request_id"),
        (shot("é", None, "DP-1"), "request_id"),
        (shot("r", Some(""), "DP-1"), "station"),
        (shot("r", Some(&"s".repeat(129)), "DP-1"), "station"),
        (shot("r", Some("a/b"), "DP-1"), "station"),
        (shot("r", None, ""), "monitor"),
        (shot("r", None, &"m".repeat(65)), "monitor"),
        (shot("r", None, "DP 1"), "monitor"),
        (shot("r", None, "DP-1;x"), "monitor"),
        (shot("r", None, "é"), "monitor"),
    ] {
        assert_eq!(c.validate(), Err(ArgError { field }), "{c:?}");
    }
}

#[test]
fn a_url_is_1_to_2048_scalars_and_the_grammar_is_not_checked_here() {
    let open = |url: &str, monitor: Option<&str>| {
        let mut args = json!({"request_id": "r", "url": url});
        if let Some(m) = monitor {
            args["monitor"] = json!(m);
        }
        call(json!({"tool": "preview_open", "arguments": args}))
    };
    assert!(open("http://localhost:3000/", None).validate().is_ok());
    assert!(open(&"x".repeat(2048), Some("DP-1")).validate().is_ok());
    // The URL grammar runs later, on the deck.
    assert!(open("not a url", None).validate().is_ok());
    assert_eq!(open("", None).validate(), Err(ArgError { field: "url" }));
    assert_eq!(
        open(&"x".repeat(2049), None).validate(),
        Err(ArgError { field: "url" })
    );
    assert_eq!(
        open("http://a", Some("")).validate(),
        Err(ArgError { field: "monitor" })
    );
}

#[test]
fn a_wall_reference_and_a_window_handle_are_checked() {
    let wall = |display: &str, content: Value| {
        call(json!({"tool": "wall_show", "arguments": {
            "request_id": "r", "display_id": display, "content": content}}))
    };
    let session = |s: &str, x: &str| {
        json!({"kind": "session",
        "session": {"station_id": s, "session_id": x}})
    };
    let artifact = |a: &str, v: u32| {
        json!({"kind": "artifact",
        "artifact": {"station_id": "s", "artifact_id": a, "version": v}})
    };
    assert!(wall("d1", session("s", "x")).validate().is_ok());
    assert!(
        wall(&"d".repeat(64), artifact("a", u32::MAX))
            .validate()
            .is_ok()
    );
    for (c, field) in [
        (wall("", session("s", "x")), "display_id"),
        (wall(&"d".repeat(65), session("s", "x")), "display_id"),
        (wall("d.1", session("s", "x")), "display_id"),
        (wall("d", session("", "x")), "content.session.station_id"),
        (wall("d", session("s", "x y")), "content.session.session_id"),
        (wall("d", artifact("a", 0)), "artifact.version"),
        (wall("d", artifact("", 1)), "artifact.artifact_id"),
    ] {
        assert_eq!(c.validate(), Err(ArgError { field }), "{c:?}");
    }
    // A version above u32::MAX is a parse error, not a value.
    assert!(
        serde_json::from_value::<DesktopCall>(json!({"tool": "wall_show", "arguments": {
            "request_id": "r", "display_id": "d", "content": artifact("a", 1)}}))
        .is_ok()
    );
    assert!(
        serde_json::from_value::<ArtifactRef>(
            json!({"station_id": "s", "artifact_id": "a", "version": 4_294_967_296_u64})
        )
        .is_err()
    );
    let close = |w: &str| {
        call(json!({"tool": "window_close", "arguments": {"request_id": "r", "window_id": w}}))
    };
    assert!(close(&"a".repeat(32)).validate().is_ok());
    assert_eq!(close("").validate(), Err(ArgError { field: "window_id" }));
    assert_eq!(
        close(&"a".repeat(65)).validate(),
        Err(ArgError { field: "window_id" })
    );
    assert_eq!(
        close("a.b").validate(),
        Err(ArgError { field: "window_id" })
    );
}

#[test]
fn a_query_is_validated_too() {
    assert!(
        query(json!({"tool": "desktop_info", "arguments": {}}))
            .validate()
            .is_ok()
    );
    assert_eq!(
        query(json!({"tool": "desktop_info", "arguments": {"station": ""}})).validate(),
        Err(ArgError { field: "station" })
    );
    for tool in ["desktop_action_status", "capture_status"] {
        assert!(
            query(json!({"tool": tool, "arguments": {"request_id": "r"}}))
                .validate()
                .is_ok()
        );
        assert_eq!(
            query(json!({"tool": tool, "arguments": {"request_id": "r r"}})).validate(),
            Err(ArgError {
                field: "request_id"
            })
        );
    }
    let image = |v: u32| {
        query(
            json!({"tool": "read_capture_image", "arguments": {"artifact": {
            "station_id": "s", "artifact_id": "a", "version": v}}}),
        )
    };
    assert!(image(1).validate().is_ok());
    assert_eq!(
        image(0).validate(),
        Err(ArgError {
            field: "artifact.version"
        })
    );
}

#[test]
fn the_error_never_holds_the_value() {
    let secret = "ghp_not_a_real_secret value";
    let err = call(json!({"tool": "screenshot", "arguments": {
        "request_id": secret, "monitor": "DP-1"}}))
    .validate()
    .unwrap_err();
    assert_eq!(err.field, "request_id");
    assert!(!format!("{err:?}").contains("ghp_"));
}

#[test]
fn a_desktop_info_result_has_the_exact_shape() {
    let info = CoordinatorDesktopInfo {
        station: "s1".into(),
        kind: DesktopKind::Omarchy,
        monitors: vec![CoordinatorMonitor {
            name: "DP-1".into(),
            width: 2560,
            height: 1440,
            scale_pct: 100,
            focused: true,
            dpms_on: None,
            capture_allowed: false,
        }],
        locked: None,
        capabilities: CoordinatorCapabilities {
            notify: true,
            preview: false,
            screenshot: false,
        },
        walls: vec![CoordinatorWall {
            display_id: "d1".into(),
            monitor: "DP-1".into(),
        }],
    };
    let v = serde_json::to_value(&info).unwrap();
    assert_eq!(
        v,
        json!({
            "station": "s1", "kind": "omarchy",
            "monitors": [{"name": "DP-1", "width": 2560, "height": 1440, "scale_pct": 100,
                "focused": true, "dpms_on": null, "capture_allowed": false}],
            "locked": null,
            "capabilities": {"notify": true, "preview": false, "screenshot": false},
            "walls": [{"display_id": "d1", "monitor": "DP-1"}]
        })
    );
    assert_eq!(
        serde_json::from_value::<CoordinatorDesktopInfo>(v).unwrap(),
        info
    );
    let mut extra = serde_json::to_value(&info).unwrap();
    extra["monitors"][0]["serial"] = json!("X");
    assert!(serde_json::from_value::<CoordinatorDesktopInfo>(extra).is_err());
}

#[test]
fn the_status_structs_skip_absent_fields_and_round_trip() {
    let unknown = DesktopActionStatus {
        request_id: "r".into(),
        state: ActionState::Unknown,
        operation_id: None,
        station: None,
        result: None,
        error: None,
        message: None,
    };
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        json!({"request_id": "r", "state": "unknown"})
    );
    let done = DesktopActionStatus {
        request_id: "r".into(),
        state: ActionState::Done,
        operation_id: Some("op".into()),
        station: Some("s".into()),
        result: Some(json!({"window_id": "w"})),
        error: None,
        message: None,
    };
    let v = serde_json::to_value(&done).unwrap();
    assert_eq!(
        v,
        json!({"request_id": "r", "state": "done", "operation_id": "op", "station": "s",
            "result": {"window_id": "w"}})
    );
    assert_eq!(
        serde_json::from_value::<DesktopActionStatus>(v).unwrap(),
        done
    );

    let committed = CoordinatorCaptureStatus {
        request_id: "r".into(),
        state: CaptureState::Committed,
        capture_id: Some("c".into()),
        station: Some("s".into()),
        artifact: Some(ArtifactRef {
            station_id: "s".into(),
            artifact_id: "a".into(),
            version: 1,
        }),
        monitor: Some("DP-1".into()),
        size: Some(10),
        sha256: Some("ab".into()),
        at: Some(5),
        error: None,
        message: None,
        requested_seconds: None,
        duration_ms: None,
        stop_reason: None,
    };
    let v = serde_json::to_value(&committed).unwrap();
    assert_eq!(v["artifact"]["version"], 1);
    assert_eq!(v["state"], "committed");
    assert!(v.get("error").is_none());
    // A screenshot status has none of the recording fields (R13 P5b-2).
    for key in ["requested_seconds", "duration_ms", "stop_reason"] {
        assert!(v.get(key).is_none(), "{key}");
    }
    assert_eq!(
        serde_json::from_value::<CoordinatorCaptureStatus>(v).unwrap(),
        committed
    );
    // A recording status carries the three extra fields.
    let recording = CoordinatorCaptureStatus {
        requested_seconds: Some(30),
        duration_ms: Some(12_345),
        stop_reason: Some(crate::recording::StopReason::OwnerStop),
        ..committed
    };
    let v = serde_json::to_value(&recording).unwrap();
    assert_eq!(v["requested_seconds"], 30);
    assert_eq!(v["duration_ms"], 12_345);
    assert_eq!(v["stop_reason"], "owner_stop");
    assert_eq!(
        serde_json::from_value::<CoordinatorCaptureStatus>(v).unwrap(),
        recording
    );
    for state in ["unknown", "pending", "deleted", "committed", "failed"] {
        let s: CaptureState = serde_json::from_value(json!(state)).unwrap();
        assert_eq!(serde_json::to_value(s).unwrap(), json!(state));
    }
    for state in ["unknown", "pending", "done", "failed"] {
        let s: ActionState = serde_json::from_value(json!(state)).unwrap();
        assert_eq!(serde_json::to_value(s).unwrap(), json!(state));
    }
    assert!(serde_json::from_value::<ActionState>(json!("deleted")).is_err());
}

#[test]
fn an_image_answer_round_trips() {
    let answer = CaptureImageAnswer {
        meta: CaptureImageMeta {
            artifact: ArtifactRef {
                station_id: "s".into(),
                artifact_id: "a".into(),
                version: 1,
            },
            capture_id: "c".into(),
            monitor: "DP-1".into(),
            at: 7,
            size: 3,
            sha256: "ab".into(),
        },
        png_base64: "AAAA".into(),
    };
    let v = serde_json::to_value(&answer).unwrap();
    assert_eq!(v["meta"]["size"], 3);
    assert_eq!(v["png_base64"], "AAAA");
    assert_eq!(
        serde_json::from_value::<CaptureImageAnswer>(v).unwrap(),
        answer
    );
}

fn record(seconds: u32) -> DesktopCall {
    DesktopCall::RecordMonitor(RecordMonitorArguments {
        request_id: "req_1".into(),
        station: Some("st-1".into()),
        monitor: "DP-1".into(),
        seconds,
    })
}

#[test]
fn record_monitor_serializes_as_its_tool_and_refuses_unknown_fields() {
    let call = record(30);
    let v = serde_json::to_value(&call).unwrap();
    assert_eq!(
        v,
        json!({"tool": "record_monitor", "arguments": {
            "request_id": "req_1", "station": "st-1", "monitor": "DP-1", "seconds": 30}})
    );
    assert_eq!(serde_json::from_value::<DesktopCall>(v).unwrap(), call);
    // The station is optional and is not written when absent.
    let no_station = DesktopCall::RecordMonitor(RecordMonitorArguments {
        request_id: "r".into(),
        station: None,
        monitor: "DP-1".into(),
        seconds: 1,
    });
    let v = serde_json::to_value(&no_station).unwrap();
    assert!(v["arguments"].get("station").is_none());
    assert_eq!(
        serde_json::from_value::<DesktopCall>(v).unwrap(),
        no_station
    );
    // Unknown fields and the missing required fields are refused.
    for bad in [
        json!({"tool": "record_monitor", "arguments": {
            "request_id": "r", "monitor": "DP-1", "seconds": 5, "extra": 1}}),
        json!({"tool": "record_monitor", "arguments": {"request_id": "r", "monitor": "DP-1"}}),
        json!({"tool": "record_monitor", "arguments": {"request_id": "r", "seconds": 5}}),
        json!({"tool": "record_monitor", "arguments": {"monitor": "DP-1", "seconds": 5}}),
        json!({"tool": "record_monitor", "arguments": {
            "request_id": "r", "monitor": "DP-1", "seconds": -1}}),
        json!({"tool": "record_demo", "arguments": {
            "request_id": "r", "monitor": "DP-1", "seconds": 5}}),
    ] {
        assert!(
            serde_json::from_value::<DesktopCall>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn record_monitor_validation_names_the_field_and_bounds_the_seconds() {
    assert_eq!(record(1).validate(), Ok(()));
    assert_eq!(record(120).validate(), Ok(()));
    assert_eq!(record(121).validate(), Ok(()));
    assert_eq!(record(3600).validate(), Ok(()));
    assert_eq!(record(0).validate(), Err(ArgError { field: "seconds" }));
    assert_eq!(record(3601).validate(), Err(ArgError { field: "seconds" }));
    assert_eq!(
        record(u32::MAX).validate(),
        Err(ArgError { field: "seconds" })
    );
    let with = |f: fn(&mut RecordMonitorArguments)| {
        let DesktopCall::RecordMonitor(mut a) = record(5) else {
            unreachable!()
        };
        f(&mut a);
        DesktopCall::RecordMonitor(a).validate()
    };
    assert_eq!(with(|_| {}), Ok(()));
    assert_eq!(
        with(|a| a.request_id = String::new()),
        Err(ArgError {
            field: "request_id"
        })
    );
    assert_eq!(
        with(|a| a.request_id = "a b".into()),
        Err(ArgError {
            field: "request_id"
        })
    );
    assert_eq!(
        with(|a| a.request_id = "x".repeat(65)),
        Err(ArgError {
            field: "request_id"
        })
    );
    assert_eq!(
        with(|a| a.station = Some("bad station".into())),
        Err(ArgError { field: "station" })
    );
    assert_eq!(
        with(|a| a.station = Some("s".repeat(129))),
        Err(ArgError { field: "station" })
    );
    assert_eq!(
        with(|a| a.monitor = String::new()),
        Err(ArgError { field: "monitor" })
    );
    assert_eq!(
        with(|a| a.monitor = "DP 1".into()),
        Err(ArgError { field: "monitor" })
    );
    assert_eq!(
        with(|a| a.monitor = "m".repeat(65)),
        Err(ArgError { field: "monitor" })
    );
    assert_eq!(with(|a| a.monitor = "eDP-1.2_x".into()), Ok(()));
}
