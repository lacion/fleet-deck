//! The typed actions of the mutating tools (R13 P6 §4.3 to §4.5, §7.1).

use fleetdeck_proto::coordinator_desktop::{
    NotifyArguments, PreviewOpenArguments, WindowCloseArguments,
};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;

use super::*;

fn notify(title: &str, body: &str, urgency: CoordUrgency) -> DesktopCall {
    DesktopCall::Notify(NotifyArguments {
        request_id: "r1".into(),
        station: None,
        title: title.into(),
        body: body.into(),
        urgency,
    })
}

fn preview(url: &str, monitor: Option<&str>) -> DesktopCall {
    DesktopCall::PreviewOpen(PreviewOpenArguments {
        request_id: "r2".into(),
        station: None,
        url: url.into(),
        monitor: monitor.map(str::to_owned),
    })
}

fn close(window_id: &str) -> DesktopCall {
    DesktopCall::WindowClose(WindowCloseArguments {
        request_id: "r3".into(),
        station: Some("st".into()),
        window_id: window_id.into(),
    })
}

fn session() -> SessionId {
    "coord-1".into()
}

fn built(call: &DesktopCall) -> Built {
    build(call, "coord-1", &session()).expect("a valid call")
}

#[test]
fn a_notification_title_is_prefixed_with_the_label_and_cut_to_80_scalars() {
    let b = built(&notify("Deploy done", "all green", CoordUrgency::Normal));
    assert_eq!(
        b.action,
        DesktopAction::Notify {
            title: "coord-1: Deploy done".into(),
            body: "all green".into(),
            urgency: Urgency::Normal,
        }
    );
    // 80 scalars of title: the prefixed title is cut at 80.
    let long = "x".repeat(80);
    let DesktopAction::Notify { title, .. } = built(&notify(&long, "", CoordUrgency::Low)).action
    else {
        panic!("a notification");
    };
    assert_eq!(title.chars().count(), 80);
    assert!(title.starts_with("coord-1: xxx"), "{title}");
    // Markup is stripped and the body is cut at 300.
    let DesktopAction::Notify {
        title,
        body,
        urgency,
    } = built(&notify(
        "<b>bold</b> news",
        &"b".repeat(400),
        CoordUrgency::Low,
    ))
    .action
    else {
        panic!("a notification");
    };
    assert_eq!(title, "coord-1: bold news");
    assert_eq!(body.chars().count(), 300);
    assert_eq!(urgency, Urgency::Low);
}

#[test]
fn the_action_hash_covers_the_final_title_and_the_context() {
    let ctx = CoordinatorContext {
        grant_id: "g".into(),
        binding_id: "b".into(),
        generation: 1,
        caller_station: StationId::from("st"),
        caller_session: session(),
        caller_label: "coord-1".into(),
    };
    let b = built(&notify("Hi", "", CoordUrgency::Normal));
    let raw = DesktopAction::Notify {
        title: "Hi".into(),
        body: String::new(),
        urgency: Urgency::Normal,
    };
    let node = "node";
    let final_hash = action_hash_for(&b.action, node, Some(&ctx));
    assert_ne!(final_hash, action_hash_for(&raw, node, Some(&ctx)));
    let mut other = ctx.clone();
    other.generation = 2;
    assert_ne!(final_hash, action_hash_for(&b.action, node, Some(&other)));
}

#[test]
fn a_url_that_fails_the_shared_grammar_is_invalid_params() {
    for url in [
        "ftp://host/x",
        "javascript:alert(1)",
        "http://user@host/",
        "http://host name/",
        "https://",
        "file:///etc/passwd",
    ] {
        assert_eq!(
            build(&preview(url, None), "coord-1", &session()).err(),
            Some("invalid_params"),
            "{url}"
        );
    }
}

#[test]
fn a_preview_opens_the_url_for_the_caller_and_names_only_the_monitor() {
    let b = built(&preview("http://localhost:5173/app", Some("DP-1")));
    assert_eq!(
        b.action,
        DesktopAction::BrowserOpen {
            url: "http://localhost:5173/app".into(),
            monitor: Some("DP-1".into()),
            session: Some(session()),
        }
    );
    assert_eq!(b.resource, json!({"monitor": "DP-1"}));
    assert_eq!(b.monitor.as_deref(), Some("DP-1"));
    let b = built(&preview("https://host.tail1234.ts.net/x?q=1", None));
    assert_eq!(b.resource, json!({}));
    // The resource never holds the URL.
    assert!(!b.resource.to_string().contains("host"));
}

#[test]
fn a_close_names_the_window_handle_and_nothing_else() {
    let b = built(&close("0123456789abcdef0123456789abcdef"));
    assert_eq!(
        b.action,
        DesktopAction::PreviewClose {
            window_id: "0123456789abcdef0123456789abcdef".into()
        }
    );
    assert_eq!(
        b.resource,
        json!({"window_id": "0123456789abcdef0123456789abcdef"})
    );
    assert_eq!(b.op_tool, OpTool::WindowClose);
}

#[test]
fn every_tool_of_the_call_method_is_served_wall_show_included() {
    use fleetdeck_proto::coordinator_desktop::{
        ScreenshotArguments, WallContent, WallShowArguments,
    };
    assert!(served(&notify("t", "", CoordUrgency::Normal)));
    assert!(served(&preview("http://localhost", None)));
    assert!(served(&close("w")));
    let shot = DesktopCall::Screenshot(ScreenshotArguments {
        request_id: "r".into(),
        station: None,
        monitor: "DP-1".into(),
    });
    assert!(served(&shot));
    let wall = DesktopCall::WallShow(WallShowArguments {
        request_id: "r".into(),
        station: None,
        display_id: "d".into(),
        content: WallContent::Session {
            session: fleetdeck_proto::display_directive::SessionRef {
                station_id: StationId::from("s"),
                session_id: "x".into(),
            },
        },
        pin: false,
    });
    assert!(served(&wall));
}

#[test]
fn a_screenshot_is_the_screen_capture_of_one_monitor_and_names_only_the_monitor() {
    use fleetdeck_proto::coordinator_desktop::ScreenshotArguments;
    let shot = DesktopCall::Screenshot(ScreenshotArguments {
        request_id: "r4".into(),
        station: None,
        monitor: "DP-1".into(),
    });
    let b = built(&shot);
    assert_eq!(
        b.action,
        DesktopAction::ScreenCapture {
            monitor: "DP-1".into()
        }
    );
    assert_eq!(b.op_tool, OpTool::Screenshot);
    assert_eq!(b.resource, json!({"monitor": "DP-1"}));
    assert_eq!(b.monitor.as_deref(), Some("DP-1"));
    assert_eq!(shape(&shot), Some((Tool::Screenshot, None)));
    assert_eq!(handler_ms(Tool::Screenshot), 40_000);
    assert_eq!(action_name(Tool::Screenshot), "desktop.capture");
}

#[test]
fn the_payload_hash_ignores_the_request_id_and_covers_the_target_and_the_rest() {
    let target = StationId::from("st");
    let args = |request: &str, title: &str, station: Option<&str>| {
        serde_json::to_value(NotifyArguments {
            request_id: request.into(),
            station: station.map(str::to_owned),
            title: title.into(),
            body: String::new(),
            urgency: CoordUrgency::Normal,
        })
        .expect("json")
    };
    let one = payload_hash(args("r1", "t", None), &target);
    // Another request ID, and the explicit default station: the same hash.
    assert_eq!(one, payload_hash(args("r2", "t", Some("st")), &target));
    assert_eq!(one.len(), 64);
    // Another title or another target: another hash.
    assert_ne!(one, payload_hash(args("r1", "u", None), &target));
    assert_ne!(
        one,
        payload_hash(args("r1", "t", None), &StationId::from("other"))
    );
    // The compact JSON of the sorted keys, hashed.
    let want = sha256_hex(r#"{"body":"","station":"st","title":"t","urgency":"normal"}"#);
    assert_eq!(one, want);
}

#[test]
fn the_handler_deadline_is_100_s_for_a_preview_and_40_s_for_the_others() {
    assert_eq!(handler_ms(Tool::PreviewOpen), 100_000);
    assert_eq!(handler_ms(Tool::Notify), 40_000);
    assert_eq!(handler_ms(Tool::WindowClose), 40_000);
}
