//! Text tests of the plugin files (R13 P4 task D, `omarchy-p4.md` §3.5). The
//! live smoke runs the QML; these tests check its text and `Model.js`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::collections::BTreeSet;
use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use super::assets::FILES;

const DIR: &str = "plugins/fleetdeck.fleet/";
const QML: [&str; 3] = ["Widget.qml", "Panel.qml", "Service.qml"];

fn text(name: &str) -> &'static str {
    let path = format!("{DIR}{name}");
    FILES
        .iter()
        .find(|a| a.path == path)
        .unwrap_or_else(|| panic!("no asset {path}"))
        .text
}

/// The text of each `Text { ... }` block of a QML file.
fn text_blocks(qml: &str) -> Vec<&str> {
    let bytes = qml.as_bytes();
    let mut blocks = Vec::new();
    let mut from = 0;
    while let Some(found) = qml[from..].find("Text {") {
        let start = from + found;
        from = start + 1;
        let before = start.checked_sub(1).map(|i| bytes[i]);
        if before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_') {
            continue;
        }
        let open = start + "Text ".len();
        let mut depth = 0usize;
        let mut end = None;
        for (i, b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        blocks.push(&qml[start..end.expect("a Text block is not closed")]);
    }
    blocks
}

/// The keys of every `strings.t("...")` and `strings.tn("...")` call.
fn looked_up_keys(qml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for call in ["strings.t(", "strings.tn("] {
        let mut from = 0;
        while let Some(found) = qml[from..].find(call) {
            let start = from + found + call.len();
            from = start;
            let rest = &qml[start..];
            assert!(
                rest.starts_with('"'),
                "a lookup key is not a literal: {call}"
            );
            let end = rest[1..].find('"').expect("a lookup key is not closed");
            keys.push(rest[1..=end].to_owned());
        }
    }
    keys
}

/// The arrays of every `command: [...]` line of a QML file.
fn command_arrays(qml: &str) -> Vec<Vec<String>> {
    let mut arrays = Vec::new();
    let mut from = 0;
    while let Some(found) = qml[from..].find("command: [") {
        let start = from + found + "command: ".len();
        from = start;
        let end = start + qml[start..].find(']').expect("a command is not closed");
        arrays.push(serde_json::from_str(&qml[start..=end]).unwrap());
    }
    arrays
}

#[test]
fn the_entry_points_of_the_manifest_are_in_the_file_list() {
    let manifest: Value = serde_json::from_str(text("manifest.json")).unwrap();
    for (_, file) in manifest["entryPoints"].as_object().unwrap() {
        text(file.as_str().unwrap());
    }
    for name in QML {
        assert!(!text(name).is_empty());
    }
}

#[test]
fn every_text_sink_is_plain_text() {
    for name in QML {
        let qml = text(name);
        for banned in ["Text.RichText", "Text.StyledText", "Text.AutoText"] {
            assert!(!qml.contains(banned), "{name}: {banned}");
        }
        for block in text_blocks(qml) {
            assert!(
                block.contains("textFormat: Text.PlainText"),
                "{name}: a Text block has no PlainText:\n{block}"
            );
        }
    }
    assert!(text_blocks(text("Widget.qml")).len() >= 1);
    assert!(text_blocks(text("Panel.qml")).len() >= 8);
}

#[test]
fn the_qml_files_import_the_commons_module_and_have_no_approval_word() {
    for name in QML {
        let qml = text(name);
        assert!(qml.contains("import qs.Commons"), "{name}");
        for word in ["approve", "Approve", "allow", "Allow"] {
            assert!(!qml.contains(word), "{name}: {word}");
        }
    }
    for name in ["Model.js", "i18n/en.json"] {
        for word in ["approve", "Approve", "allow", "Allow"] {
            assert!(!text(name).contains(word), "{name}: {word}");
        }
    }
    // The colors come from the palette, not from literals.
    for name in QML {
        let qml = text(name);
        assert!(!qml.contains("\"#"), "{name}: a color literal");
    }
    assert!(text("Widget.qml").contains("Commons.Color.urgent"));
    assert!(text("Panel.qml").contains("Commons.Color.urgent"));
}

#[test]
fn model_js_has_no_import() {
    for line in text("Model.js").lines() {
        let line = line.trim_start();
        assert!(
            !line.starts_with("import ")
                && !line.starts_with(".import")
                && !line.contains("require("),
            "{line}"
        );
    }
}

#[test]
fn every_looked_up_key_exists_and_every_key_is_used() {
    let catalog: Value = serde_json::from_str(text("i18n/en.json")).unwrap();
    let catalog = catalog.as_object().unwrap();
    assert!(
        catalog.values().all(Value::is_string),
        "a flat catalog of strings"
    );
    let mut used = BTreeSet::new();
    for name in QML {
        let qml = text(name);
        for key in looked_up_keys(qml) {
            assert!(key.starts_with("fleetdeck."), "{name}: {key}");
            assert!(catalog.contains_key(&key), "{name}: missing key {key}");
            used.insert(key);
        }
        // A QML file with text declares its own lookup.
        if qml.contains("strings.t(") {
            assert!(
                qml.contains(
                    "Commons.Translations { id: strings; directory: Qt.resolvedUrl(\"i18n\") }"
                ),
                "{name}"
            );
        }
    }
    for key in catalog.keys() {
        assert!(used.contains(key), "unused key {key}");
    }
}

#[test]
fn the_process_commands_are_the_feeds_and_there_is_no_screensaver_process() {
    assert_eq!(
        command_arrays(text("Service.qml")),
        vec![vec!["fleetdeck", "desktop", "feed", "--notify"]]
    );
    let service = text("Service.qml");
    assert!(!service.contains("screensaver"), "the station runs it now");
    assert_eq!(service.matches("Process {").count(), 1);
    assert_eq!(service.matches("Timer {").count(), 1);
    assert_eq!(
        command_arrays(text("Panel.qml")),
        vec![vec!["fleetdeck", "desktop", "feed"]]
    );
    assert!(command_arrays(text("Widget.qml")).is_empty());
}

#[test]
fn the_click_and_key_commands_are_argv_arrays_of_the_annex() {
    let widget = text("Widget.qml");
    for argv in [
        r#"Quickshell.execDetached(["fleetdeck", "desktop", "open"])"#,
        r#"Quickshell.execDetached(["fleetdeck", "desktop", "wall", "show"])"#,
        r#"Quickshell.execDetached(["omarchy-shell", "shell", "toggle", "fleetdeck.fleet", "{}"])"#,
    ] {
        assert!(widget.contains(argv), "{argv}");
    }
    let panel = text("Panel.qml");
    for argv in [
        r#"Quickshell.execDetached(["fleetdeck", "desktop", "open", item.station_id + "/" + item.session, "--request", item.request])"#,
        r#"Quickshell.execDetached(["fleetdeck", "desktop", "wall", "next"])"#,
    ] {
        assert!(panel.contains(argv), "{argv}");
    }
    // Every start of a command goes through an argv array, never a shell.
    for name in QML {
        let qml = text(name);
        assert!(!qml.contains("bash"), "{name}");
        assert!(!qml.contains("sh -c"), "{name}");
    }
}

/// The names of a JavaScript runtime on `PATH`.
fn runtime() -> Option<&'static str> {
    ["node", "bun"].into_iter().find(|name| {
        Command::new(name)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Runs `parse` of `Model.js` on each line with a JavaScript runtime.
fn run_parse(runtime: &str, lines: &[String]) -> Vec<Value> {
    let script = r#"
const fs = require("fs");
const parse = new Function(process.env.MODEL_SRC + "\nreturn parse;")();
const lines = JSON.parse(fs.readFileSync(0, "utf8"));
process.stdout.write(JSON.stringify(lines.map((line) => parse(line))));
"#;
    let mut child = Command::new(runtime)
        .arg("-e")
        .arg(script)
        .env("MODEL_SRC", text("Model.js"))
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(lines).unwrap().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn unknown() -> Value {
    json!({
        "v": 1, "deck": "unknown",
        "counts": {"working": 0, "idle": 0, "needs_you": 0, "blocked": 0, "lost": 0},
        "needs_you": [], "usage": null, "stations": []
    })
}

fn item(session: &str, kind: &str) -> Value {
    json!({"station": "srv", "station_id": "st1", "session": session, "request": "r1",
           "callsign": "lynx-12", "kind": kind, "age_s": 95})
}

/// A valid line of exactly `size` bytes (spaces after the JSON).
fn padded(size: usize) -> String {
    let mut line = r#"{"v":1,"deck":"up"}"#.to_owned();
    while line.len() < size {
        line.push(' ');
    }
    line
}

#[test]
fn model_js_parses_bounds_and_rejects_with_a_javascript_runtime() {
    let Some(runtime) = runtime() else {
        println!("skipped: no node or bun on PATH");
        return;
    };
    let annex = r#"{"v":1,"at":"2026-10-18T12:04:00Z","board":"https://deck.example.ts.net:7420","deck":"up",
 "counts":{"working":7,"idle":3,"needs_you":2,"blocked":1,"lost":0},
 "needs_you":[{"station":"srv","station_id":"st1","session":"s1","request":"r1","callsign":"lynx-12",
               "kind":"tool","age_s":95}],
 "usage":{"label":"claude · 5 h","pct":63,"resets_in_s":4320},
 "stations":[{"name":"srv","up":true,"cpu_load_pct":41},{"name":"laptop","up":true,"cpu_load_pct":null}]}"#;
    let mut many = json!({"v": 1, "deck": "up", "needs_you": [], "stations": []});
    for i in 0..12 {
        many["needs_you"]
            .as_array_mut()
            .unwrap()
            .push(item(&format!("s{i}"), "command"));
    }
    for i in 0..40 {
        many["stations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": format!("n{i}"), "up": true, "cpu_load_pct": null}));
    }
    let dirty = json!({"v": 1, "deck": "sideways",
        "counts": {"working": -1, "idle": 1.5, "needs_you": "3", "blocked": 2, "lost": 0},
        "needs_you": [{"station": "a\u{202e}b\u{7}c", "station_id": "st1", "session": "s1",
                       "request": "r1", "callsign": "\u{200f}", "kind": "question", "age_s": -4}],
        "usage": {"label": "x".repeat(100), "pct": 250, "resets_in_s": -1},
        "stations": [{"name": "n".repeat(100), "up": "yes", "cpu_load_pct": 1.5}]});
    let lines = vec![
        annex.to_owned(),
        "not json".to_owned(),
        padded(16 * 1024),
        padded(16 * 1024 + 1),
        json!({"v": 1, "deck": "up", "needs_you": [
            item("bad id!", "tool"), item("s2", "approval"), item("s3", "tool")]})
        .to_string(),
        many.to_string(),
        dirty.to_string(),
        r#"{"v":2,"deck":"up"}"#.to_owned(),
        "[1]".to_owned(),
    ];
    let got = run_parse(runtime, &lines);

    assert_eq!(got[0]["deck"], "up");
    assert_eq!(got[0]["counts"]["working"], 7);
    assert_eq!(got[0]["counts"]["needs_you"], 2);
    assert_eq!(
        got[0]["needs_you"],
        json!([{"station": "srv", "station_id": "st1", "session": "s1", "request": "r1",
                "callsign": "lynx-12", "kind": "tool", "age_s": 95}])
    );
    assert_eq!(
        got[0]["usage"],
        json!({"label": "claude · 5 h", "pct": 63, "resets_in_s": 4320})
    );
    assert_eq!(
        got[0]["stations"],
        json!([{"name": "srv", "up": true, "cpu_load_pct": 41},
               {"name": "laptop", "up": true, "cpu_load_pct": null}])
    );

    assert_eq!(got[1], unknown());
    assert_eq!(got[2]["deck"], "up", "16 KiB exactly is the largest line");
    assert_eq!(got[3], unknown());

    let kept = got[4]["needs_you"].as_array().unwrap();
    assert_eq!(kept.len(), 1, "a bad id and a bad kind drop the item");
    assert_eq!(kept[0]["session"], "s3");

    assert_eq!(got[5]["needs_you"].as_array().unwrap().len(), 10);
    assert_eq!(got[5]["stations"].as_array().unwrap().len(), 32);

    assert_eq!(got[6]["deck"], "unknown");
    assert_eq!(
        got[6]["counts"],
        json!({"working": 0, "idle": 0, "needs_you": 0, "blocked": 2, "lost": 0})
    );
    assert_eq!(got[6]["needs_you"][0]["station"], "abc");
    assert_eq!(got[6]["needs_you"][0]["callsign"], Value::Null);
    assert_eq!(got[6]["needs_you"][0]["age_s"], 0);
    assert_eq!(got[6]["usage"]["label"].as_str().unwrap().len(), 64);
    assert_eq!(got[6]["usage"]["pct"], 100);
    assert_eq!(got[6]["usage"]["resets_in_s"], Value::Null);
    assert_eq!(got[6]["stations"][0]["name"].as_str().unwrap().len(), 64);
    assert_eq!(got[6]["stations"][0]["up"], false);
    assert_eq!(got[6]["stations"][0]["cpu_load_pct"], Value::Null);

    assert_eq!(got[7], unknown());
    assert_eq!(got[8], unknown());
}

#[test]
fn the_widget_draws_the_board_mark() {
    let brand = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/src/ui/brand.ts"
    ))
    .expect("read brand.ts");
    let after = &brand[brand.find("MARK_PATH =").expect("MARK_PATH") + "MARK_PATH =".len()..];
    let open = after.find('\'').expect("opening quote") + 1;
    let close = open + after[open..].find('\'').expect("closing quote");
    let path = &after[open..close];
    let qml = text("Widget.qml");
    assert!(qml.contains(&format!("PathSvg {{ path: \"{path}\" }}")));
    for needed in [
        "import QtQuick.Shapes",
        "fillColor: root.tint",
        "fillRule: ShapePath.OddEvenFill",
    ] {
        assert!(qml.contains(needed), "{needed}");
    }
    assert!(!qml.contains("\\uf0e8"));
}
