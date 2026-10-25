//! The schemas of §5 and the `window_list` projection of §6 of
//! `omarchy-p7b.md`.

use serde_json::{Value, json};

use super::*;
use crate::coordinator_desktop::{ArgError, DesktopCall, DesktopQuery};

const WINDOW: &str = "0123456789abcdef0123456789abcdef";

fn click(x: u32, y: u32) -> PointerClickArguments {
    PointerClickArguments {
        request_id: "r1".into(),
        station: None,
        window_id: WINDOW.into(),
        x,
        y,
    }
}

fn query(v: Value) -> Result<DesktopQuery, serde_json::Error> {
    serde_json::from_value(v)
}

#[test]
fn pointer_click_serializes_as_its_tool_and_skips_an_absent_station() {
    let c: DesktopCall = serde_json::from_value(json!({"tool": "pointer_click", "arguments": {
        "request_id": "r1", "window_id": WINDOW, "x": 0, "y": 32767}}))
    .unwrap();
    assert_eq!(c, DesktopCall::PointerClick(click(0, 32767)));
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        json!({"tool": "pointer_click", "arguments": {
            "request_id": "r1", "window_id": WINDOW, "x": 0, "y": 32767}})
    );
    assert_eq!(c.validate(), Ok(()));
    let named: DesktopCall = serde_json::from_value(json!({"tool": "pointer_click", "arguments": {
        "request_id": "r1", "station": "st-1", "window_id": WINDOW, "x": 1, "y": 2}}))
    .unwrap();
    let DesktopCall::PointerClick(a) = &named else {
        panic!("not a pointer_click")
    };
    assert_eq!(a.station.as_deref(), Some("st-1"));
    assert_eq!(
        serde_json::to_value(&named).unwrap()["arguments"]["station"],
        "st-1"
    );
}

#[test]
fn pointer_click_refuses_unknown_fields_a_button_null_and_wrong_types() {
    let base = |extra: Value| {
        let mut args = json!({"request_id": "r", "window_id": WINDOW, "x": 1, "y": 2});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        json!({"tool": "pointer_click", "arguments": args})
    };
    assert!(serde_json::from_value::<DesktopCall>(base(json!({}))).is_ok());
    // There is no button, count, duration, modifier, caller identity, grant,
    // address, monitor or scope argument (§5.1).
    for field in [
        "button",
        "count",
        "duration",
        "modifiers",
        "caller",
        "grant_id",
        "address",
        "monitor",
        "scope",
        "input_scope",
        "x2",
    ] {
        assert!(
            serde_json::from_value::<DesktopCall>(base(json!({ field: 1 }))).is_err(),
            "{field}"
        );
    }
    // A null station is not an omission.
    assert!(serde_json::from_value::<DesktopCall>(base(json!({"station": null}))).is_err());
    let mut top = base(json!({}));
    top["x"] = json!(1);
    assert!(serde_json::from_value::<DesktopCall>(top).is_err());
    for bad in [
        json!({"tool": "pointer_click", "arguments": {"request_id": "r", "window_id": WINDOW}}),
        json!({"tool": "pointer_click", "arguments": {"request_id": "r", "window_id": WINDOW, "x": 1}}),
        json!({"tool": "pointer_click", "arguments": {"request_id": "r", "window_id": WINDOW, "x": -1, "y": 0}}),
        json!({"tool": "pointer_click", "arguments": {"request_id": "r", "window_id": WINDOW, "x": 1.5, "y": 0}}),
        json!({"tool": "pointer_click", "arguments": {"request_id": "r", "window_id": WINDOW, "x": "1", "y": 0}}),
        json!({"tool": "pointer_click", "arguments": {"request_id": "r", "window_id": WINDOW, "x": 1, "y": null}}),
        json!({"tool": "scroll", "arguments": {"request_id": "r", "window_id": WINDOW, "x": 1, "y": 2}}),
    ] {
        assert!(
            serde_json::from_value::<DesktopCall>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn pointer_click_coordinates_request_id_station_and_window_id_follow_the_rules() {
    let ok = |a: PointerClickArguments| DesktopCall::PointerClick(a).validate();
    // Coordinates: integers from 0 to 32,767.
    for (x, y) in [(0, 0), (32_767, 32_767), (5, 32_767)] {
        assert_eq!(ok(click(x, y)), Ok(()), "{x} {y}");
    }
    assert_eq!(ok(click(32_768, 0)), Err(ArgError { field: "x" }));
    assert_eq!(ok(click(0, 32_768)), Err(ArgError { field: "y" }));
    assert_eq!(ok(click(u32::MAX, 0)), Err(ArgError { field: "x" }));
    // request_id: 1 to 64 of [A-Za-z0-9_-].
    for bad in ["", &"x".repeat(65), "a b", "a.b", "é"] {
        let mut a = click(1, 1);
        a.request_id = bad.into();
        assert_eq!(
            ok(a),
            Err(ArgError {
                field: "request_id"
            })
        );
    }
    // station: 1 to 128 of [A-Za-z0-9_-].
    for bad in ["", &"s".repeat(129), "s/1", "s s"] {
        let mut a = click(1, 1);
        a.station = Some(bad.into());
        assert_eq!(ok(a), Err(ArgError { field: "station" }));
    }
    // window_id: exactly 32 lowercase hexadecimal digits.
    for bad in [
        "",
        &WINDOW[..31],
        &format!("{WINDOW}0"),
        &WINDOW.to_uppercase(),
        "0123456789abcdef0123456789abcdeg",
    ] {
        let mut a = click(1, 1);
        a.window_id = bad.into();
        assert_eq!(ok(a), Err(ArgError { field: "window_id" }), "{bad}");
    }
    // The error names the field and never shows the value.
    let mut a = click(1, 1);
    a.window_id = "secret-handle".into();
    assert!(!format!("{:?}", ok(a)).contains("secret-handle"));
}

#[test]
fn window_list_serializes_as_its_query_and_has_one_optional_station() {
    let q = query(json!({"tool": "window_list", "arguments": {}})).unwrap();
    assert_eq!(
        q,
        DesktopQuery::WindowList(WindowListArguments { station: None })
    );
    assert_eq!(
        serde_json::to_value(&q).unwrap(),
        json!({"tool": "window_list", "arguments": {}})
    );
    assert_eq!(q.validate(), Ok(()));
    let named = query(json!({"tool": "window_list", "arguments": {"station": "st-1"}})).unwrap();
    assert_eq!(
        named,
        DesktopQuery::WindowList(WindowListArguments {
            station: Some("st-1".into())
        })
    );
    assert_eq!(named.validate(), Ok(()));
    assert_eq!(
        serde_json::to_value(&named).unwrap(),
        json!({"tool": "window_list", "arguments": {"station": "st-1"}})
    );
}

#[test]
fn window_list_refuses_unknown_fields_and_a_null_station() {
    // No request_id, title filter, address selector, scope override or
    // pagination (§5.2).
    for field in [
        "request_id",
        "title",
        "address",
        "scope",
        "input_scope",
        "page",
        "limit",
        "monitor",
    ] {
        assert!(
            query(json!({"tool": "window_list", "arguments": {field: "x"}})).is_err(),
            "{field}"
        );
    }
    assert!(query(json!({"tool": "window_list", "arguments": {"station": null}})).is_err());
    assert!(query(json!({"tool": "window_list", "arguments": {}, "x": 1})).is_err());
    assert!(query(json!({"tool": "window_list"})).is_err());
    // The station follows the schema pattern.
    for bad in ["", &"s".repeat(129), "s/1", "s s"] {
        let q = DesktopQuery::WindowList(WindowListArguments {
            station: Some(bad.into()),
        });
        assert_eq!(q.validate(), Err(ArgError { field: "station" }), "{bad}");
    }
    let q = DesktopQuery::WindowList(WindowListArguments {
        station: Some("s".repeat(128)),
    });
    assert_eq!(q.validate(), Ok(()));
}

fn window(n: u32) -> InputWindow {
    InputWindow {
        window_id: format!("{n:032x}"),
        rect: InputRect {
            x: -10,
            y: 20,
            width: 300,
            height: 200,
        },
        monitors: vec!["DP-1".into()],
        focused: n == 0,
        expires_in_ms: 60_000,
    }
}

fn list(windows: u32) -> InputWindowList {
    InputWindowList {
        station: "st-1".into(),
        monitors: vec![InputMonitor {
            name: "DP-1".into(),
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        }],
        windows: (0..windows).map(window).collect(),
    }
}

#[test]
fn the_window_list_answer_serializes_with_its_closed_fields() {
    let l = list(2);
    let v = serde_json::to_value(&l).unwrap();
    assert_eq!(v["station"], "st-1");
    assert_eq!(
        v["monitors"][0],
        json!({"name": "DP-1", "x": -1920, "y": 0, "width": 1920, "height": 1080})
    );
    assert_eq!(
        v["windows"][0],
        json!({
            "window_id": format!("{:032x}", 0),
            "rect": {"x": -10, "y": 20, "width": 300, "height": 200},
            "monitors": ["DP-1"],
            "focused": true,
            "expires_in_ms": 60_000
        })
    );
    assert_eq!(
        serde_json::from_value::<InputWindowList>(v.clone()).unwrap(),
        l
    );
    // Every struct refuses an unknown field (no title, class or address).
    for path in [
        vec![],
        vec!["monitors", "0"],
        vec!["windows", "0"],
        vec!["windows", "0", "rect"],
    ] {
        let mut bad = v.clone();
        let mut at = &mut bad;
        for key in &path {
            at = match key.parse::<usize>() {
                Ok(i) => &mut at[i],
                Err(_) => &mut at[*key],
            };
        }
        at["title"] = json!("secret");
        assert!(
            serde_json::from_value::<InputWindowList>(bad).is_err(),
            "{path:?}"
        );
    }
    // A missing field is refused too.
    let mut missing = v;
    missing["windows"][0]
        .as_object_mut()
        .unwrap()
        .remove("focused");
    assert!(serde_json::from_value::<InputWindowList>(missing).is_err());
}

#[test]
fn a_window_list_within_the_limits_is_valid() {
    assert_eq!(list(0).validate(), Ok(()));
    assert_eq!(list(1).validate(), Ok(()));
    assert_eq!(list(64).validate(), Ok(()));
}

#[test]
fn a_window_list_with_65_windows_or_monitors_is_too_large() {
    assert_eq!(list(65).validate(), Err(WindowListError::TooLarge));
    let mut l = list(1);
    l.monitors = (0..65)
        .map(|i| InputMonitor {
            name: if i == 0 {
                "DP-1".into()
            } else {
                format!("M{i}")
            },
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        })
        .collect();
    assert_eq!(l.validate(), Err(WindowListError::TooLarge));
    l.monitors.truncate(64);
    assert_eq!(l.validate(), Ok(()));
}

#[test]
fn a_zero_width_or_height_or_a_bad_handle_is_invalid() {
    for edit in [
        (|l: &mut InputWindowList| l.windows[0].rect.width = 0) as fn(&mut InputWindowList),
        |l| l.windows[0].rect.height = 0,
        |l| l.monitors[0].width = 0,
        |l| l.monitors[0].height = 0,
        |l| l.windows[0].window_id = "0x1".into(),
        |l| l.windows[0].window_id = WINDOW.to_uppercase(),
        |l| l.windows[0].window_id = String::new(),
        |l| l.windows[1].monitors = vec!["DP-1".into(), "HDMI-9".into()],
    ] {
        let mut l = list(2);
        edit(&mut l);
        assert_eq!(l.validate(), Err(WindowListError::Invalid));
    }
}

#[test]
fn a_window_list_over_16_kib_is_too_large_and_one_at_the_limit_is_not() {
    // Few windows with a long monitor name list: the count is within the
    // limit, the size is not.
    let mut l = list(1);
    l.windows[0].monitors = (0..64).map(|_| "M".repeat(300)).collect();
    assert!(serde_json::to_vec(&l).unwrap().len() > 16 * 1024);
    assert_eq!(l.validate(), Err(WindowListError::TooLarge));
    // Pad the station name to exactly 16 KiB and then one byte over.
    let mut at_limit = list(1);
    let base = serde_json::to_vec(&at_limit).unwrap().len();
    at_limit.station = "s".repeat(4 + 16 * 1024 - base);
    assert_eq!(serde_json::to_vec(&at_limit).unwrap().len(), 16 * 1024);
    assert_eq!(at_limit.validate(), Ok(()));
    at_limit.station.push('s');
    assert_eq!(at_limit.validate(), Err(WindowListError::TooLarge));
}

#[test]
fn a_window_list_error_has_its_code() {
    assert_eq!(WindowListError::TooLarge.code(), "desktop_result_too_large");
    assert_eq!(WindowListError::Invalid.code(), "desktop_failed");
}
