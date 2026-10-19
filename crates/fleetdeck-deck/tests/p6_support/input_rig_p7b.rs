//! The fake desktop of the P7b end-to-end proofs (R13 P7b task P): the P7
//! input fakes of `input_rig`, with a fake `hyprctl` that also answers
//! `cursorpos` (the last `movecursor` point) and runs `on_focus.sh` at a
//! `focuswindow` dispatch, a fake `wlrctl` that writes its argv to `calls.log`,
//! and two ordinary client windows next to the preview (one with the sentinel
//! title `SENTINEL-TITLE-7f3a` and the class `sentinel.class`).
//!
//! `hyprctl instances -j` is the answer of the P6 fake (one instance with the
//! signature and the Wayland socket of the fake desktop environment), so the
//! compositor check of a click passes.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use fleetdeck_deck::store::{GrantClass, NewGrant};
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_station::coordinator::{Binding, BindingState, ToolClass, mint};
use fleetdeck_station::store::{NewSession, OfflineApprovalPolicy};
use serde_json::{Value, json};

use super::fake_desktop::Rig;
use super::mcp::Mcp;
use super::{NATIVE_SENTINEL, World};

/// The sentinel title and class of an ordinary window: they must be in no
/// answer, audit row, stored row or log.
pub const SENTINEL_TITLE: &str = "SENTINEL-TITLE-7f3a";
pub const SENTINEL_CLASS: &str = "sentinel.class";
/// The address, origin and size of the sentinel window.
pub const SENTINEL_ADDRESS: &str = "0x7f00a001";
pub const SENTINEL_AT: (i64, i64) = (2000, 100);
pub const SENTINEL_SIZE: (i64, i64) = (500, 300);
/// The other ordinary window.
pub const OTHER_ADDRESS: &str = "0x7f00a002";
pub const OTHER_CLASS: &str = "other.class";
pub const OTHER_TITLE: &str = "plain-title";
pub const OTHER_AT: (i64, i64) = (1200, 800);
pub const OTHER_SIZE: (i64, i64) = (400, 300);

fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The fake `hyprctl` of `input_rig`, plus `cursorpos`, a `movecursor` that
/// sets the cursor to its point and a `focuswindow` that sources
/// `on_focus.sh` when it exists. A read is logged to `reads.log`, every other
/// call to `calls.log`.
fn hyprctl_script(r: &str) -> String {
    let note = |lg: &str| {
        format!("{{ echo \"== hyprctl\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/{lg}")
    };
    format!(
        "case \"$1\" in -j|instances) {reads};; *) {log};; esac\n\
         case \"$*\" in\n\
         \"instances -j\") /bin/cat {r}/instances.json;;\n\
         \"-j monitors\") /bin/cat {r}/monitors.json;;\n\
         \"-j clients\") /bin/cat {r}/clients.json;;\n\
         \"-j layers\") /bin/cat {r}/layers.json;;\n\
         \"-j activewindow\") /bin/cat {r}/active.json;;\n\
         \"-j cursorpos\") /bin/cat {r}/cursor.json;;\n\
         \"dispatch movecursor \"*) printf '{{\"x\": %s, \"y\": %s}}' \"$3\" \"$4\" > {r}/cursor.json; echo ok;;\n\
         \"dispatch focuswindow address:\"*) [ -f {r}/on_focus.sh ] && . {r}/on_focus.sh; echo ok;;\n\
         \"dispatch closewindow address:\"*) echo \"${{3#address:}}\" >> {r}/close.req; echo ok;;\n\
         *) echo ok;;\n\
         esac",
        reads = note("reads.log"),
        log = note("calls.log"),
    )
}

/// The fake `wlrctl`: logs its argv with the other changing programs.
fn wlrctl_script(r: &str) -> String {
    format!(
        "{{ echo \"== wlrctl\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/calls.log\nexit 0"
    )
}

/// Puts the P7b fakes into the folder of `rig` (after `input_rig::arm`):
/// `hyprctl` with the cursor, `wlrctl`, and a cursor at the origin.
pub fn arm(rig: &Rig) {
    let root = rig.dir.path();
    let r = root.display().to_string();
    let bin = root.join("bin");
    write_script(&bin, "hyprctl", &hyprctl_script(&r));
    write_script(&bin, "wlrctl", &wlrctl_script(&r));
    std::fs::write(
        root.join("cursor.json"),
        json!({"x": 0, "y": 0}).to_string(),
    )
    .unwrap();
}

fn read_json(root: &Path, name: &str) -> Value {
    let text = std::fs::read_to_string(root.join(name)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn ordinary(address: &str, class: &str, title: &str, at: (i64, i64), size: (i64, i64)) -> Value {
    json!({
        "address": address, "class": class, "title": title,
        "initialClass": class, "initialTitle": title, "mapped": true, "hidden": false,
        "at": [at.0, at.1], "size": [size.0, size.1],
        "workspace": {"id": 1}, "focusHistoryID": 1
    })
}

/// The active window entry of the ordinary window `address`.
pub fn active_of(address: &str, class: &str, title: &str) -> Value {
    json!({
        "address": address, "class": class, "title": title,
        "initialClass": class, "initialTitle": title
    })
}

/// Adds the two ordinary windows to the client list that `adopt_preview`
/// wrote (the preview stays the focused client, `focusHistoryID` 0).
pub fn add_ordinary_windows(rig: &Rig) {
    let root = rig.dir.path();
    let mut preview = read_json(root, "clients.json")[0].clone();
    preview["focusHistoryID"] = json!(0);
    let list = json!([
        preview,
        ordinary(
            SENTINEL_ADDRESS,
            SENTINEL_CLASS,
            SENTINEL_TITLE,
            SENTINEL_AT,
            SENTINEL_SIZE
        ),
        ordinary(
            OTHER_ADDRESS,
            OTHER_CLASS,
            OTHER_TITLE,
            OTHER_AT,
            OTHER_SIZE
        ),
    ]);
    std::fs::write(root.join("clients.json"), list.to_string()).unwrap();
}

/// Makes the `focuswindow` dispatch change the active window to `active`.
pub fn focus_goes_to(rig: &Rig, active: &Value) {
    let root = rig.dir.path();
    std::fs::write(root.join("after_focus.json"), active.to_string()).unwrap();
    std::fs::write(
        root.join("on_focus.sh"),
        format!(
            "/bin/cp {r}/after_focus.json {r}/active.json",
            r = root.display()
        ),
    )
    .unwrap();
}

/// The `wlrctl` calls, in order (each is its argv).
pub fn wlrctl_calls(rig: &Rig) -> Vec<Vec<String>> {
    rig.calls_of("wlrctl").into_iter().map(|c| c.args).collect()
}

/// Whether `text` is a handle: 32 lowercase hexadecimal characters.
pub fn is_handle(text: &str) -> bool {
    text.len() == 32 && text.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

fn scan(dir: &Path, needle: &str, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan(&path, needle, found);
        } else if std::fs::read(&path)
            .is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
        {
            found.push(path.display().to_string());
        }
    }
}

impl World {
    /// The `window_list` call of A for B.
    pub async fn window_list(&self) -> super::mcp::Tool {
        self.mcp
            .call("window_list", json!({"station": self.to_b()}))
            .await
    }

    /// The handle of the listed window whose rectangle starts at `at`.
    pub fn handle_at(listing: &Value, at: (i64, i64)) -> String {
        listing["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["rect"]["x"] == at.0 && w["rect"]["y"] == at.1)
            .unwrap_or_else(|| panic!("no listed window at {at:?}: {listing}"))["window_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The paths of the files of the test (the databases and logs of the deck
    /// and the stations; not the folder of the fake desktop, whose window list
    /// holds the sentinels on purpose) that hold `needle`.
    pub fn files_holding_outside_rig(&self, needle: &str) -> Vec<String> {
        let mut found = Vec::new();
        scan(self.tmp.path(), needle, &mut found);
        found
    }

    /// `(attempted_at, exposure_addrs)` of the `pointer_click` operation.
    pub async fn click_operation(&self) -> (Option<i64>, Option<String>) {
        self.deck()
            .store
            .db()
            .call(|c| {
                Ok(c.query_row(
                    "SELECT attempted_at, exposure_addrs FROM coordinator_desktop_ops
                     WHERE tool = 'pointer_click'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await
            .unwrap()
    }

    /// A second coordinator session on A (`coord-2`) with its own grant (the
    /// classes, caps, click allowance and scope of this world), binding and
    /// credential. Returns its MCP client.
    pub async fn second_caller(&self) -> Mcp {
        let id = "coord-2";
        self.a
            .store
            .create_session(NewSession {
                session_id: id.into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let (count, characters) = self.input.expect("an input world");
        let scope = if self.any_window {
            InputScope::AnyWindow
        } else {
            InputScope::OwnWindows
        };
        let mut spec = NewGrant::new(self.a_id.clone(), id.into());
        spec.classes = [
            GrantClass::Read,
            GrantClass::Mail,
            GrantClass::Desktop,
            GrantClass::Capture,
            GrantClass::Input,
        ]
        .into_iter()
        .collect();
        spec.stations = BTreeSet::from([self.a_id.clone(), self.b_id.clone()]);
        spec.allow_input = true;
        spec.allow_click = self.click;
        spec.input_scope = Some(scope);
        spec.input_cap = count;
        spec.input_character_cap = characters;
        let grant = self.deck().node.create_grant(spec).await.unwrap();
        let binding = CoordinatorBinding {
            binding_id: "b-2".to_owned(),
            grant_id: grant.grant_id.clone(),
            generation: 1,
            provider_instance: "claude".into(),
            native_id: NATIVE_SENTINEL.to_owned(),
        };
        assert!(self.deck().store.bind_grant(binding.clone()).await.unwrap());
        let mut classes: BTreeSet<ToolClass> = [
            ToolClass::Read,
            ToolClass::Mail,
            ToolClass::Desktop,
            ToolClass::Capture,
            ToolClass::Input,
            ToolClass::InputEnabled,
        ]
        .into_iter()
        .collect();
        if self.click {
            classes.insert(ToolClass::ClickEnabled);
        }
        if self.any_window {
            classes.insert(ToolClass::InputAny);
        }
        let (token, digest) = mint().unwrap();
        self.a.node.coordinator_bindings().insert(
            digest,
            Binding {
                binding_id: binding.binding_id.clone(),
                grant_id: grant.grant_id,
                session_id: id.into(),
                provider_instance: "claude".to_owned(),
                native_id: Some(NATIVE_SENTINEL.to_owned()),
                generation: 1,
                classes,
                state: BindingState::Active,
                control_skill: "skill".to_owned(),
                scope: GrantScope::Lineage,
            },
        );
        Mcp {
            url: self.a.node.coordinator_url(),
            token: token.expose().to_owned(),
        }
    }
}

/// The owner start of a coordinator session on `station` with an input grant
/// over `targets`, with the click allowance and the scope as told (the board
/// request of R13 P7b §3.1).
pub async fn start_p7b_grant(
    c: &crate::common::board::BoardClient,
    h: &crate::settings_support::Headers,
    elevation: &str,
    station: &str,
    targets: &[&str],
    (click, scope): (bool, Option<&str>),
) -> crate::common::board::Http {
    let mut coordinator = json!({
        "grant_id": "", "classes": ["desktop", "capture", "input"], "stations": targets,
        "allow_input": true, "allow_click": click, "idle_ttl_ms": 0, "expires_at": 0,
        "control_skill": {"version": "", "text": ""}});
    if let Some(scope) = scope {
        coordinator["input_scope"] = json!(scope);
    }
    let body = json!({"station": station, "command_id": "grant-p7b-1", "elevation": elevation,
        "command": {"op": "start", "prompt": null, "spec": {
            "provider_instance": "claude", "cwd": "/w", "mode": null, "model": null,
            "title": null, "coordinator": coordinator}}});
    crate::settings_support::post(c, h, "/api/commands/session.start", body).await
}
