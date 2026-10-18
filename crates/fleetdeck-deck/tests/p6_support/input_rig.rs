//! The fake desktop of the P7 end-to-end proofs of coordinator input (R13 P7
//! task P): a fake `hyprctl` with geometry, layers, an active window and a
//! focus dispatch, and a fake `wtype` that logs its argv and writes its stdin
//! to a file. It is put into the folder of the P6 rig of target station B.
//!
//! It is modeled on the rig of the station tests
//! (`crates/fleetdeck-station/src/desktop/input/rig.rs`), which a test of
//! another crate cannot use. The shell fakes read flag files of the rig
//! folder:
//!
//! - `wtype_hold`: while the file exists, the fake `wtype` waits after it
//!   wrote `wtype.started` (the rig removes the file to let it end);
//! - the P6 gate of `hyprctl instances -j` works as before.
//!
//! The fake `wtype` writes its stdin to `wtype.stdin` only when its argv is
//! `-`, and its environment to `wtype.env`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::World;
use super::fake_desktop::{MONITOR_SENTINEL, Rig, WINDOW_SENTINEL};
use crate::common::board::{BoardClient, Http};
use crate::settings_support::{Headers, post};

/// The origin of the fake preview window, in logical coordinates.
pub const PREVIEW_AT: (i64, i64) = (100, 100);
/// The size of the fake preview window.
pub const PREVIEW_SIZE: (i64, i64) = (800, 600);

fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake `hyprctl`: the reads of the P6 fake (with its gate), plus the layers,
/// the active window and the `focuswindow` and `movecursor` dispatches. A
/// read is logged to `reads.log`, every other call to `calls.log`.
fn hyprctl_script(r: &str) -> String {
    let note = |lg: &str| {
        format!("{{ echo \"== hyprctl\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/{lg}")
    };
    format!(
        "case \"$1\" in -j|instances) {reads};; *) {log};; esac\n\
         case \"$*\" in\n\
         \"instances -j\") if [ -f {r}/gate ]; then : > {r}/entered; \
         while [ -f {r}/gate ]; do /bin/sleep 0.02; done; fi; /bin/cat {r}/instances.json;;\n\
         \"-j monitors\") /bin/cat {r}/monitors.json;;\n\
         \"-j clients\") /bin/cat {r}/clients.json;;\n\
         \"-j layers\") /bin/cat {r}/layers.json;;\n\
         \"-j activewindow\") /bin/cat {r}/active.json;;\n\
         \"dispatch closewindow address:\"*) echo \"${{3#address:}}\" >> {r}/close.req; echo ok;;\n\
         *) echo ok;;\n\
         esac",
        reads = note("reads.log"),
        log = note("calls.log"),
    )
}

/// A fake `wtype`: logs its argv, captures stdin when the argv is `-`,
/// records its environment, and waits while `wtype_hold` exists.
fn wtype_script(r: &str) -> String {
    format!(
        "{{ echo \"== wtype\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/calls.log\n\
         [ \"$1\" = \"-\" ] && /bin/cat > {r}/wtype.stdin\n\
         /usr/bin/env > {r}/wtype.env\n\
         : > {r}/wtype.started\n\
         while [ -f {r}/wtype_hold ]; do /bin/sleep 0.02; done\n\
         exit 0"
    )
}

fn monitors() -> Value {
    json!([
        {"id": 0, "name": "DP-1", "description": MONITOR_SENTINEL, "width": 3440, "height": 1440,
         "x": 0, "y": 0, "refreshRate": 144.0, "scale": 1.0, "transform": 0, "focused": true,
         "dpmsStatus": true, "activeWorkspace": {"id": 1, "name": "1"}},
        {"id": 1, "name": "HDMI-A-1", "description": "Side", "width": 1920, "height": 1080,
         "x": 3440, "y": 0, "refreshRate": 60.0, "scale": 1.0, "transform": 0, "focused": false,
         "dpmsStatus": true, "activeWorkspace": {"id": 5, "name": "5"}}
    ])
}

fn put(root: &Path, name: &str, value: &Value) {
    std::fs::write(root.join(name), value.to_string()).unwrap();
}

/// Puts the input fakes into the folder of `rig`: `hyprctl` and `wtype`, the
/// monitors with their origins, and no layer surface.
pub fn arm(rig: &Rig) {
    let root = rig.dir.path();
    let r = root.display().to_string();
    let bin = root.join("bin");
    write_script(&bin, "hyprctl", &hyprctl_script(&r));
    write_script(&bin, "wtype", &wtype_script(&r));
    put(root, "monitors.json", &monitors());
    put(
        root,
        "layers.json",
        &json!({"DP-1": {"levels": {"0": [], "1": []}}}),
    );
}

fn read_json(root: &Path, name: &str) -> Value {
    let text = std::fs::read_to_string(root.join(name)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// The full client entry of a preview with `title`.
fn full_client(address: &str, class: &str, title: &str) -> Value {
    json!({
        "address": address, "class": class, "title": title,
        "initialClass": class, "initialTitle": title, "mapped": true, "hidden": false,
        "at": [PREVIEW_AT.0, PREVIEW_AT.1], "size": [PREVIEW_SIZE.0, PREVIEW_SIZE.1],
        "workspace": {"id": 1}
    })
}

fn full_active(address: &str, class: &str, title: &str) -> Value {
    json!({
        "address": address, "class": class, "title": title,
        "initialClass": class, "initialTitle": title
    })
}

/// Completes the client entry that the P6 fake announced for the newest
/// preview (address, class, title only) with the geometry that the input
/// station reads, and makes that preview the active window. Returns the
/// address and the class of the preview.
pub fn adopt_preview(rig: &Rig) -> (String, String) {
    let root = rig.dir.path();
    let list = read_json(root, "clients.json");
    let last = list
        .as_array()
        .and_then(|a| a.last())
        .expect("a preview client")
        .clone();
    let address = last["address"].as_str().unwrap().to_owned();
    let class = last["class"].as_str().unwrap().to_owned();
    put(
        root,
        "clients.json",
        &json!([full_client(&address, &class, WINDOW_SENTINEL)]),
    );
    put(
        root,
        "active.json",
        &full_active(&address, &class, WINDOW_SENTINEL),
    );
    (address, class)
}

/// Gives the preview the title `title`, in the client list and in the active
/// window (what a page that shows the board looks like).
pub fn set_title(rig: &Rig, title: &str) {
    let root = rig.dir.path();
    let (address, class) = (
        read_json(root, "clients.json")[0]["address"]
            .as_str()
            .unwrap()
            .to_owned(),
        read_json(root, "clients.json")[0]["class"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    put(
        root,
        "clients.json",
        &json!([full_client(&address, &class, title)]),
    );
    put(root, "active.json", &full_active(&address, &class, title));
}

/// Removes the logs of the changing programs (the baseline of a test). The
/// event task of the P6 fake remembers the windows that it announced.
pub fn clear_calls(rig: &Rig) {
    let _ = std::fs::remove_file(rig.dir.path().join("calls.log"));
}

/// Makes the fake `wtype` wait (`true`) or go on (`false`).
pub fn hold_wtype(rig: &Rig, on: bool) {
    let path = rig.dir.path().join("wtype_hold");
    if on {
        std::fs::write(path, "").unwrap();
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// Waits until the fake `wtype` started (the file `wtype.started`).
pub async fn wtype_started(rig: &Rig) {
    let path = rig.dir.path().join("wtype.started");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the fake wtype did not start"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The text that the fake `wtype` read on its stdin, if it ran with `-`.
pub fn wtype_stdin(rig: &Rig) -> Option<String> {
    std::fs::read_to_string(rig.dir.path().join("wtype.stdin")).ok()
}

/// The `hyprctl dispatch focuswindow` calls, in order.
pub fn focus_calls(rig: &Rig) -> Vec<Vec<String>> {
    dispatches(rig, "focuswindow")
}

/// The `hyprctl dispatch movecursor` calls, in order.
pub fn move_calls(rig: &Rig) -> Vec<Vec<String>> {
    dispatches(rig, "movecursor")
}

fn dispatches(rig: &Rig, what: &str) -> Vec<Vec<String>> {
    rig.calls_of("hyprctl")
        .into_iter()
        .filter(|c| c.args.get(1).is_some_and(|a| a == what))
        .map(|c| c.args)
        .collect()
}

/// The paths (as text) of every file below `dir` that holds `needle`, but the
/// files named in `skip`.
fn files_with(dir: &Path, needle: &str, skip: &[&str], found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_with(&path, needle, skip, found);
            continue;
        }
        if skip
            .iter()
            .any(|s| path.file_name().is_some_and(|n| n == *s))
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
            found.push(path.display().to_string());
        }
    }
}

impl World {
    /// `preview_open` of A for B, then the geometry of the preview on the
    /// fake desktop, then an empty call log. Returns the `window_id`.
    pub async fn open_preview(&self, id: &str) -> String {
        let url =
            json!({"request_id": id, "station": self.to_b(), "url": "http://localhost:3000/"});
        let window = self.mcp.call("preview_open", url).await.ok()["window_id"]
            .as_str()
            .unwrap()
            .to_owned();
        adopt_preview(&self.b.rig);
        clear_calls(&self.b.rig);
        window
    }

    /// The files of the whole test (the databases and logs of the deck and the
    /// stations, the folder of the fake desktop) that hold `needle`, but the
    /// stdin file of the fake `wtype`. Empty when the text is nowhere.
    pub fn files_holding(&self, needle: &str) -> Vec<String> {
        let mut found = Vec::new();
        files_with(self.tmp.path(), needle, &["wtype.stdin"], &mut found);
        files_with(self.b.rig.dir.path(), needle, &["wtype.stdin"], &mut found);
        found
    }

    /// The grant of the world as the deck stores it.
    pub async fn grant(&self) -> fleetdeck_deck::store::Grant {
        self.deck()
            .store
            .grant(self.grant_id.clone())
            .await
            .unwrap()
            .expect("the grant")
    }

    /// An input call of A for B.
    pub fn key_args(&self, id: &str, window: &str, chord: &str) -> Value {
        json!({"request_id": id, "station": self.to_b(), "window_id": window, "chord": chord})
    }

    pub fn text_args(&self, id: &str, window: &str, text: &str) -> Value {
        json!({"request_id": id, "station": self.to_b(), "window_id": window, "text": text})
    }

    pub fn move_args(&self, id: &str, window: &str, x: u32, y: u32) -> Value {
        json!({"request_id": id, "station": self.to_b(), "window_id": window, "x": x, "y": y})
    }
}

/// The owner start of a coordinator session on `station` with an input grant
/// over `targets` (the classes `desktop`, `capture`, `input` and
/// `allow_input`), as the board sends it with an elevation.
pub async fn start_input_grant(
    c: &BoardClient,
    h: &Headers,
    elevation: &str,
    station: &str,
    targets: &[&str],
) -> Http {
    let body = json!({"station": station, "command_id": "grant-input-1", "elevation": elevation,
        "command": {"op": "start", "prompt": null, "spec": {
            "provider_instance": "claude", "cwd": "/w", "mode": null, "model": null,
            "title": null, "coordinator": {
                "grant_id": "", "classes": ["desktop", "capture", "input"], "stations": targets,
                "allow_input": true, "idle_ttl_ms": 0, "expires_at": 0,
                "control_skill": {"version": "", "text": ""}}}}});
    post(c, h, "/api/commands/session.start", body).await
}
