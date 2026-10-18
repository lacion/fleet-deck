//! The compositor snapshot of an input operation (R13 P7 §5.3, §7.3 step 5):
//! the strict parsers, the logical monitor rectangles, the coverage and the
//! global point of a pointer movement.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::*;

fn client(address: &str, at: (i64, i64), size: (i64, i64), workspace: i64) -> Value {
    json!({
        "address": address, "class": "c", "title": "t", "initialClass": "c",
        "initialTitle": "t", "mapped": true, "hidden": false,
        "at": [at.0, at.1], "size": [size.0, size.1], "workspace": {"id": workspace}
    })
}

fn monitor(name: &str, x: i64, y: i64, w: i64, h: i64, scale: f64, transform: i64) -> Value {
    json!({
        "name": name, "x": x, "y": y, "width": w, "height": h, "scale": scale,
        "transform": transform, "activeWorkspace": {"id": 1}
    })
}

fn snap(clients: &[Value], monitors: &[Value], layers: &str) -> Snapshot {
    Snapshot {
        clients: parse_clients(&Value::Array(clients.to_vec()).to_string()).unwrap(),
        monitors: parse_monitors(&Value::Array(monitors.to_vec()).to_string()).unwrap(),
        layers: parse_layers(layers).unwrap(),
        locked: Some(false),
    }
}

fn one_monitor() -> Vec<Value> {
    vec![monitor("DP-1", 0, 0, 2000, 1000, 1.0, 0)]
}

#[test]
fn a_client_needs_its_geometry_and_flags_and_keeps_its_names() {
    let list = parse_clients(&json!([client("0xABC", (5, 6), (7, 8), 2)]).to_string()).unwrap();
    let c = &list[0];
    assert_eq!(c.address, "0xabc");
    assert_eq!(
        c.rect,
        Rect {
            x: 5,
            y: 6,
            w: 7,
            h: 8
        }
    );
    assert_eq!((c.workspace, c.mapped, c.hidden), (2, true, false));
    assert_eq!((c.title.as_str(), c.initial_class.as_str()), ("t", "c"));
    let mut bad = client("0x1", (0, 0), (1, 1), 1);
    for field in [
        "mapped",
        "hidden",
        "at",
        "size",
        "workspace",
        "class",
        "address",
    ] {
        let mut broken = bad.clone();
        broken.as_object_mut().unwrap().remove(field);
        assert!(
            parse_clients(&json!([broken]).to_string()).is_none(),
            "{field}"
        );
    }
    bad["at"] = json!([1]);
    assert!(parse_clients(&json!([bad]).to_string()).is_none());
    assert!(parse_clients("not json").is_none());
    assert!(parse_clients("{}").is_none());
    assert!(parse_clients("").is_none());
    // A title of another type is malformed; an absent title is empty.
    let mut weird = client("0x1", (0, 0), (1, 1), 1);
    weird["title"] = json!(5);
    assert!(parse_clients(&json!([weird]).to_string()).is_none());
    let mut bare = client("0x1", (0, 0), (1, 1), 1);
    bare.as_object_mut().unwrap().remove("title");
    assert_eq!(
        parse_clients(&json!([bare]).to_string()).unwrap()[0].title,
        ""
    );
}

#[test]
fn a_monitor_has_a_logical_rectangle_for_scale_and_rotation() {
    let list = parse_monitors(
        &json!([
            monitor("A", -1920, 0, 1920, 1080, 1.0, 0),
            monitor("B", 0, 0, 3440, 1440, 1.25, 0),
            monitor("C", 3000, -200, 1920, 1080, 1.0, 1),
            monitor("D", 0, 2000, 3840, 2160, 2.0, 2),
        ])
        .to_string(),
    )
    .unwrap();
    let rect = |i: usize| list[i].rect;
    assert_eq!(
        rect(0),
        Rect {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1080
        }
    );
    assert_eq!(
        rect(1),
        Rect {
            x: 0,
            y: 0,
            w: 2752,
            h: 1152
        }
    );
    // An odd transform swaps the sides.
    assert_eq!(
        rect(2),
        Rect {
            x: 3000,
            y: -200,
            w: 1080,
            h: 1920
        }
    );
    assert_eq!(
        rect(3),
        Rect {
            x: 0,
            y: 2000,
            w: 1920,
            h: 1080
        }
    );
}

#[test]
fn a_monitor_that_is_not_readable_fails_the_whole_list() {
    let good = monitor("A", 0, 0, 100, 100, 1.0, 0);
    for (field, value) in [
        ("scale", json!(0.0)),
        ("scale", json!("x")),
        ("width", json!(0)),
        ("name", json!(1)),
        ("x", json!(1.5)),
        ("transform", json!("a")),
        ("disabled", json!("no")),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert!(
            parse_monitors(&json!([good, bad]).to_string()).is_none(),
            "{field}"
        );
    }
    assert!(parse_monitors("[]").is_some());
}

#[test]
fn only_top_and_overlay_layers_count_and_malformed_data_fails() {
    // A wallpaper on the background level covers the whole monitor; it lies
    // below every window and must not refuse a pointer point in a preview.
    let layers = json!({
        "DP-1": {"levels": {
            "0": [{"address": "0x1", "x": 0, "y": 0, "w": 3840, "h": 2160, "namespace": "wallpaper"}],
            "1": [{"x": 0, "y": 0, "w": 3840, "h": 2160}],
            "2": [{"x": 5, "y": 5, "w": 1, "h": 1}],
            "3": [{"x": 9, "y": 9, "w": 2, "h": 2}]
        }},
        "HDMI-A-1": {"levels": {"0": []}}
    });
    let list = parse_layers(&layers.to_string()).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(parse_layers("{}").unwrap(), Vec::<Rect>::new());
    for bad in [
        "[]",
        "null",
        "{\"DP-1\": {}}",
        "{\"DP-1\": {\"levels\": []}}",
        "{\"DP-1\": {\"levels\": {\"0\": [{\"x\": 1}]}}}",
        "{\"DP-1\": {\"levels\": {\"0\": 5}}}",
        "nope",
    ] {
        assert!(parse_layers(bad).is_none(), "{bad}");
    }
}

#[test]
fn the_pointer_point_is_global_and_checked() {
    let s = snap(
        &[client("0xa", (100, 200), (300, 400), 1)],
        &one_monitor(),
        "{}",
    );
    let c = s.client("0xa", "c").unwrap();
    assert_eq!(s.destination(c, 0, 0), Some((100, 200)));
    assert_eq!(s.destination(c, 299, 399), Some((399, 599)));
    // The right and bottom edges and beyond are outside: no clamping.
    for (x, y) in [(300, 0), (0, 400), (300, 400), (32_767, 32_767)] {
        assert_eq!(s.destination(c, x, y), None, "{x} {y}");
    }
}

#[test]
fn a_negative_monitor_origin_gives_a_negative_global_point() {
    let s = snap(
        &[client("0xa", (-1500, 10), (300, 300), 1)],
        &[
            monitor("L", -1920, 0, 1920, 1080, 1.0, 0),
            monitor("R", 0, 0, 1920, 1080, 1.0, 0),
        ],
        "{}",
    );
    let c = s.client("0xa", "c").unwrap();
    assert_eq!(s.destination(c, 10, 20), Some((-1490, 30)));
}

#[test]
fn a_hidden_or_unmapped_preview_has_no_point() {
    for (field, value) in [("hidden", json!(true)), ("mapped", json!(false))] {
        let mut c = client("0xa", (0, 0), (100, 100), 1);
        c[field] = value;
        let s = snap(&[c], &one_monitor(), "{}");
        let c = s.client("0xa", "c").unwrap();
        assert_eq!(s.destination(c, 1, 1), None, "{field}");
    }
}

#[test]
fn a_point_outside_every_enabled_monitor_has_no_point() {
    let s = snap(
        &[client("0xa", (1900, 900), (300, 300), 1)],
        &one_monitor(),
        "{}",
    );
    let c = s.client("0xa", "c").unwrap();
    assert!(s.destination(c, 50, 50).is_some());
    assert_eq!(s.destination(c, 150, 50), None);
    let mut off = one_monitor();
    off[0]["disabled"] = json!(true);
    let s = snap(&[client("0xa", (0, 0), (300, 300), 1)], &off, "{}");
    let c = s.client("0xa", "c").unwrap();
    assert_eq!(s.destination(c, 1, 1), None);
    assert!(s.covered(c).is_empty());
}

#[test]
fn another_mapped_client_over_the_point_refuses_it() {
    let preview = client("0xa", (0, 0), (500, 500), 1);
    // On the shown workspace: refused. On another workspace: allowed.
    // On a special workspace: refused. Hidden or unmapped: allowed.
    let mut hidden = client("0xd", (0, 0), (500, 500), 1);
    hidden["hidden"] = json!(true);
    let mut unmapped = client("0xe", (0, 0), (500, 500), 1);
    unmapped["mapped"] = json!(false);
    let cases = [
        (client("0xb", (100, 100), (50, 50), 1), None),
        (client("0xb", (100, 100), (50, 50), 4), Some(true)),
        (client("0xb", (100, 100), (50, 50), -99), None),
        (hidden, Some(true)),
        (unmapped, Some(true)),
        (client("0xb", (400, 400), (50, 50), 1), Some(true)),
    ];
    for (other, allowed) in cases {
        let s = snap(&[preview.clone(), other.clone()], &one_monitor(), "{}");
        let c = s.client("0xa", "c").unwrap();
        assert_eq!(
            s.destination(c, 110, 110).is_some(),
            allowed.unwrap_or(false),
            "{other}"
        );
    }
}

#[test]
fn any_layer_surface_over_the_point_refuses_it() {
    let layers = json!({"DP-1": {"levels": {"2": [{"x": 100, "y": 100, "w": 50, "h": 50}]}}});
    let s = snap(
        &[client("0xa", (0, 0), (500, 500), 1)],
        &one_monitor(),
        &layers.to_string(),
    );
    let c = s.client("0xa", "c").unwrap();
    assert_eq!(s.destination(c, 120, 120), None);
    assert_eq!(s.destination(c, 150, 150), Some((150, 150)));
    assert_eq!(s.destination(c, 99, 120), Some((99, 120)));
}

#[test]
fn a_window_covers_the_monitors_that_it_overlaps() {
    let s = snap(
        &[client("0xa", (1900, 0), (200, 100), 1)],
        &[
            monitor("L", 0, 0, 2000, 1000, 1.0, 0),
            monitor("R", 2000, 0, 2000, 1000, 1.0, 0),
            monitor("Far", 9000, 0, 100, 100, 1.0, 0),
        ],
        "{}",
    );
    let c = s.client("0xa", "c").unwrap();
    let names: Vec<&str> = s.covered(c).iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["L", "R"]);
    assert!(s.client("0xa", "other").is_none());
    assert!(s.client("0xzz", "c").is_none());
}

#[test]
fn the_edge_of_a_rectangle_is_outside_it() {
    let r = Rect {
        x: 0,
        y: 0,
        w: 10,
        h: 10,
    };
    assert!(r.contains(9, 9));
    assert!(!r.contains(10, 5) && !r.contains(5, 10) && !r.contains(-1, 5));
    assert!(!r.intersects(&Rect {
        x: 10,
        y: 0,
        w: 5,
        h: 5
    }));
    assert!(r.intersects(&Rect {
        x: 9,
        y: 9,
        w: 5,
        h: 5
    }));
    assert!(!r.intersects(&Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 5
    }));
}
