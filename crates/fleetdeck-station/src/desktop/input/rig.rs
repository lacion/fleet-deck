//! The fake desktop of the input tests: the P6 fake rig, a fake `hyprctl`
//! with geometry, an active window, layers and a focus dispatch, a fake
//! `wtype` that captures its stdin and can stall or change the focus, and a
//! timeline file where the fake programs, the fake authority check and the
//! fake claim all write one marker line in call order.
//!
//! The shell fakes read flag files of the rig folder. `on_active.sh` and
//! `on_focus.sh` are sourced by the fake `hyprctl` at an `activewindow` read
//! and at a `focuswindow` dispatch: a test writes a snippet there to change
//! the fake desktop at that exact point.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, CoordinatorContext, CoordinatorRead, ProtectedRead,
};
use fleetdeck_proto::desktop::DesktopAction as A;
use serde_json::{Value, json};

use super::list::{self, ListRead};
use super::{Claimed, Claimer, Clock, Env};
use crate::desktop::authority_tests::Harness;
use crate::desktop::coordinator::{DispatchCheck, Extras, Gates, Live, check_live_input};
use crate::desktop::coordinator_tests::{DECK, FakeCheck, coordinated, ctx, extras};
use crate::desktop::events::Record;
use crate::desktop::fake::{Call, Opts, Rig};
use crate::desktop::tests::on;
use crate::desktop::{Desktop, Fail};

pub(super) const WINDOW_TITLE: &str = "Dummy page";

/// The fake `hyprctl`: the reads of the P6 fake, plus `activewindow`,
/// `layers`, and a `focuswindow` that can change the active window. A read of
/// `activewindow` is also a timeline line, because its order matters.
fn hyprctl_script(r: &str) -> String {
    format!(
        "case \"$1\" in -j|instances) {{ echo \"== hyprctl\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/reads.log;; \
         *) {{ echo \"== hyprctl\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/calls.log;; esac\n\
         case \"$*\" in\n\
         \"instances -j\") /bin/cat {r}/instances.json;;\n\
         \"-j monitors\") /bin/cat {r}/monitors.json;;\n\
         \"-j clients\") /bin/cat {r}/clients.json;;\n\
         \"-j layers\") /bin/cat {r}/layers.json;;\n\
         \"-j activewindow\") {{ echo \"== hyprctl\"; echo -j; echo activewindow; }} >> {r}/calls.log; \
         [ -f {r}/on_active.sh ] && . {r}/on_active.sh; \
         [ -f {r}/active_fail ] && exit 1; [ -f {r}/active_garbage ] && {{ echo 'not json'; exit 0; }}; \
         [ -f {r}/active_stall ] && /bin/sleep 5; /bin/cat {r}/active.json;;\n\
         \"dispatch closewindow address:\"*) echo \"${{3#address:}}\" >> {r}/close.req; echo ok;;\n\
         \"dispatch focuswindow address:\"*) [ -f {r}/on_focus.sh ] && . {r}/on_focus.sh; \
         [ -f {r}/focus_fail ] && exit 1; echo ok;;\n\
         *) echo ok;;\n\
         esac"
    )
}

/// The fake `wtype`: logs its argv, captures stdin when the argv is `-`,
/// records the locale and the environment, and obeys the flag files
/// `wtype_changes_focus`, `wtype_breaks_active`, `wtype_stall` and
/// `wtype_fail`.
fn wtype_script(r: &str) -> String {
    format!(
        "{{ echo \"== wtype\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/calls.log\n\
         [ \"$1\" = \"-\" ] && /bin/cat > {r}/wtype.stdin\n\
         echo \"$LC_ALL\" > {r}/wtype.locale\n\
         /usr/bin/env > {r}/wtype.env\n\
         [ -f {r}/wtype_changes_focus ] && /bin/cp {r}/after_wtype.json {r}/active.json\n\
         [ -f {r}/wtype_breaks_active ] && : > {r}/active_fail\n\
         if [ -f {r}/wtype_stall ]; then /bin/sleep 30 & echo $! > {r}/wtype.child; echo $$ > {r}/wtype.pid; wait; fi\n\
         [ -f {r}/wtype_fail ] && exit 3\n\
         exit 0"
    )
}

fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub(super) fn all_on() -> Live {
    Live {
        fleet_control: true,
        desktop: true,
        capture: true,
        desktop_input: true,
        desktop_click: true,
        desktop_input_any: true,
        monitors: Vec::new(),
    }
}

/// A fake deck check that writes a marker line, records its phases, can
/// answer a failure from the n-th check on (counted from 1), and runs a hook
/// with the check number.
pub(super) struct CountedCheck {
    root: PathBuf,
    pub phases: Mutex<Vec<AuthorityPhase>>,
    fail_from: Option<(usize, Fail)>,
    hook: Option<Box<dyn Fn(usize) + Send + Sync>>,
}

#[async_trait]
impl DispatchCheck for CountedCheck {
    async fn check(
        &self,
        phase: AuthorityPhase,
        _read: Option<&ProtectedRead>,
    ) -> Result<(), Fail> {
        let n = {
            let mut phases = self.phases.lock().unwrap();
            phases.push(phase);
            phases.len()
        };
        mark(&self.root, "check");
        if let Some(hook) = &self.hook {
            hook(n);
        }
        match &self.fail_from {
            Some((from, fail)) if n >= *from => Err(fail.clone()),
            _ => Ok(()),
        }
    }
}

/// A claim that writes a marker line and answers as told.
pub(super) struct FakeClaim {
    root: PathBuf,
    answer: Mutex<Result<Claimed, Fail>>,
    pub calls: AtomicUsize,
}

#[async_trait]
impl Claimer for FakeClaim {
    async fn claim(&self) -> Result<Claimed, Fail> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        mark(&self.root, "claim");
        self.answer.lock().unwrap().clone()
    }
}

pub(super) fn mark(root: &Path, what: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("calls.log"))
        .unwrap();
    writeln!(file, "== MARK\n{what}").unwrap();
}

/// What a test changes about one call.
pub(super) struct Probe {
    pub check: Arc<CountedCheck>,
    pub gates: Option<Gates>,
    pub live: Live,
    /// When set, the live settings that the protocol reads again at each
    /// check (it replaces `live` for that read); a test changes them while
    /// the call runs.
    pub live_now: Option<Arc<dyn Fn() -> Live + Send + Sync>>,
    pub alive: Arc<AtomicBool>,
    pub deck_hosts: Vec<String>,
    pub clock: Option<Clock>,
    pub claim: Arc<FakeClaim>,
    pub wtype_limit: Option<Duration>,
    pub handler_limit: Option<Duration>,
    /// The coordinator context and the link of the call.
    pub context: CoordinatorContext,
    pub extras: Extras,
    /// The connection and the clocks of the admission.
    pub harness: Arc<Harness>,
}

impl Probe {
    pub(super) fn new(fx: &Fx) -> Self {
        Self {
            check: fx.check(None, None),
            gates: None,
            live: all_on(),
            live_now: None,
            alive: Arc::new(AtomicBool::new(true)),
            deck_hosts: Vec::new(),
            clock: None,
            claim: fx.claim(Claimed::New),
            wtype_limit: None,
            handler_limit: None,
            context: ctx(),
            extras: extras(),
            harness: Arc::new(Harness::new()),
        }
    }
}

/// A fake desktop with one open preview of the caller.
pub(super) struct Fx {
    pub rig: Rig,
    pub window_id: String,
    pub record: Record,
    next: AtomicU64,
}

impl Fx {
    pub(super) async fn new() -> Self {
        let rig = Rig::new(Opts::default());
        let root = rig.dir.path().to_owned();
        let r = root.display().to_string();
        let bin = root.join("bin");
        write_script(&bin, "hyprctl", &hyprctl_script(&r));
        write_script(&bin, "wtype", &wtype_script(&r));
        // The preview opens through the P6 path.
        let h = Harness::new();
        let action = A::BrowserOpen {
            url: "http://localhost:3000/".to_owned(),
            monitor: None,
            session: None,
        };
        let admitted = coordinated(
            &h,
            &action,
            1,
            &ctx(),
            FakeCheck::ok(),
            Arc::new(|| Ok(())),
            extras(),
        );
        let window_id = rig
            .desktop
            .act(&on, action, admitted)
            .await
            .unwrap()
            .window_id
            .unwrap();
        let (_, record) = rig.desktop.events.preview(&window_id).unwrap();
        let fx = Self {
            rig,
            window_id,
            record,
            next: AtomicU64::new(100),
        };
        fx.write("monitors.json", &default_monitors());
        fx.write(
            "layers.json",
            &json!({"DP-1": {"levels": {"0": [], "1": []}}}),
        );
        fx.set_clients(&[fx.preview_client(), other_client()]);
        fx.set_active(&fx.preview_active(WINDOW_TITLE));
        fx.write("after_wtype.json", &foreign_active());
        fx.clear_logs();
        fx
    }

    pub(super) fn root(&self) -> PathBuf {
        self.rig.dir.path().to_owned()
    }

    pub(super) fn desktop(&self) -> &Arc<Desktop> {
        &self.rig.desktop
    }

    pub(super) fn write(&self, name: &str, value: &Value) {
        std::fs::write(self.root().join(name), value.to_string()).unwrap();
    }

    pub(super) fn write_text(&self, name: &str, text: &str) {
        std::fs::write(self.root().join(name), text).unwrap();
    }

    pub(super) fn flag(&self, name: &str, on: bool) {
        self.rig.flag(name, on);
    }

    pub(super) fn clear_logs(&self) {
        for name in ["calls.log", "reads.log"] {
            let _ = std::fs::remove_file(self.root().join(name));
        }
    }

    pub(super) fn preview_client(&self) -> Value {
        json!({
            "address": self.record.address, "class": self.record.class,
            "title": WINDOW_TITLE, "initialClass": self.record.class,
            "initialTitle": WINDOW_TITLE, "mapped": true, "hidden": false,
            "at": [100, 100], "size": [800, 600], "workspace": {"id": 1}
        })
    }

    pub(super) fn preview_active(&self, title: &str) -> Value {
        json!({
            "address": self.record.address, "class": self.record.class,
            "title": title, "initialClass": self.record.class, "initialTitle": title
        })
    }

    pub(super) fn set_clients(&self, clients: &[Value]) {
        self.write("clients.json", &Value::Array(clients.to_vec()));
    }

    pub(super) fn set_active(&self, active: &Value) {
        self.write("active.json", active);
    }

    /// Makes the focus dispatch change the active window to `active`.
    pub(super) fn focus_goes_to(&self, active: &Value) {
        self.write("after_focus.json", active);
        self.write_text(
            "on_focus.sh",
            &format!(
                "/bin/cp {}/after_focus.json {}/active.json",
                self.root().display(),
                self.root().display()
            ),
        );
    }

    /// Makes `wtype` change the active window to `active` while it runs.
    pub(super) fn wtype_changes_focus_to(&self, active: &Value) {
        self.write("after_wtype.json", active);
        self.flag("wtype_changes_focus", true);
    }

    /// The program calls and the markers, one line each, in call order.
    pub(super) fn timeline(&self) -> Vec<String> {
        self.rig
            .calls()
            .into_iter()
            .map(|call| match call.program.as_str() {
                "MARK" => call.args.join(" "),
                "hyprctl" => format!("hyprctl {}", call.args.join(" ")),
                other => other.to_owned(),
            })
            .collect()
    }

    pub(super) fn calls_of(&self, program: &str) -> Vec<Call> {
        self.rig.calls_of(program)
    }

    pub(super) fn focus_line(&self) -> String {
        format!(
            "hyprctl dispatch focuswindow address:{}",
            self.record.address
        )
    }

    /// A fake authority check that writes a marker line, passes, and can fail
    /// from the n-th check or run a hook.
    pub(super) fn check(
        &self,
        fail_from: Option<(usize, Fail)>,
        hook: Option<Box<dyn Fn(usize) + Send + Sync>>,
    ) -> Arc<CountedCheck> {
        Arc::new(CountedCheck {
            root: self.root(),
            phases: Mutex::new(Vec::new()),
            fail_from,
            hook,
        })
    }

    pub(super) fn claim(&self, answer: Claimed) -> Arc<FakeClaim> {
        Arc::new(FakeClaim {
            root: self.root(),
            answer: Mutex::new(Ok(answer)),
            calls: AtomicUsize::new(0),
        })
    }

    pub(super) fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root().join(name)).unwrap_or_default()
    }

    /// One input call with a new operation number and the claim of `probe`.
    pub(super) async fn run(&self, action: &A, probe: &Probe) -> Result<(), Fail> {
        self.run_with(action, probe, probe.claim.as_ref()).await
    }

    /// One input call with another claim.
    pub(super) async fn run_with(
        &self,
        action: &A,
        probe: &Probe,
        claim: &dyn Claimer,
    ) -> Result<(), Fail> {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let live = probe.live.clone();
        let gates: Gates = probe.gates.clone().unwrap_or_else(|| {
            let (live, gated) = (live.clone(), action.clone());
            Arc::new(move || check_live_input(&live, &gated))
        });
        let admitted = coordinated(
            &probe.harness,
            action,
            n,
            &probe.context,
            probe.check.clone(),
            gates,
            probe.extras.clone(),
        );
        let alive = Arc::clone(&probe.alive);
        let read_live: Arc<dyn Fn() -> Live + Send + Sync> = probe
            .live_now
            .clone()
            .unwrap_or_else(|| Arc::new(move || live.clone()));
        let mut env = Env::new(
            read_live,
            Arc::new(move || alive.load(Ordering::SeqCst)),
            probe.deck_hosts.clone(),
        );
        if let Some(clock) = &probe.clock {
            env.clock = Arc::clone(clock);
        }
        if let Some(limit) = probe.wtype_limit {
            env.wtype_limit = limit;
        }
        if let Some(limit) = probe.handler_limit {
            env.handler_limit = limit;
        }
        self.desktop()
            .input(&on, action.clone(), admitted, &env, claim)
            .await
    }

    pub(super) fn key(&self, chord: &str) -> A {
        A::InputKeyPress {
            window_id: self.window_id.clone(),
            chord: chord.to_owned(),
        }
    }

    pub(super) fn text(&self, text: &str) -> A {
        A::InputTypeText {
            window_id: self.window_id.clone(),
            text: text.to_owned(),
        }
    }

    pub(super) fn pointer(&self, x: u32, y: u32) -> A {
        A::InputPointerMove {
            window_id: self.window_id.clone(),
            x,
            y,
        }
    }

    // ---------------------------------------------- ordinary windows (P7b)

    /// One `window_list` read with the live settings `live` and the deck
    /// check `check`; `context` is the coordinator of the read.
    pub(super) async fn list(
        &self,
        live: &(dyn Fn() -> Live + Send + Sync),
        check: &dyn DispatchCheck,
        context: Option<CoordinatorContext>,
    ) -> Result<Value, Fail> {
        self.list_with(live, check, context, &[]).await
    }

    /// As [`Fx::list`], with the lower case deck hosts of the barrier.
    pub(super) async fn list_with(
        &self,
        live: &(dyn Fn() -> Live + Send + Sync),
        check: &dyn DispatchCheck,
        context: Option<CoordinatorContext>,
        deck_hosts: &[String],
    ) -> Result<Value, Fail> {
        let source = ListRead::new(self.desktop(), DECK, "target", live);
        let req = CoordinatorRead {
            read_id: "0".repeat(32),
            read: ProtectedRead::InputWindows,
            read_hash: "0".repeat(64),
            context,
        };
        list::read(&source, deck_hosts, check, &req).await
    }

    /// A listing that the fake deck allows, for the context of the probe.
    pub(super) async fn listing(&self) -> Value {
        let live = all_on();
        self.list(&move || live.clone(), &*FakeCheck::ok(), Some(ctx()))
            .await
            .unwrap()
    }

    /// The handle of the listed window at `(x, y)` of `listing`.
    pub(super) fn handle_at(listing: &Value, x: i64, y: i64) -> String {
        listing["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["rect"]["x"] == x && w["rect"]["y"] == y)
            .unwrap_or_else(|| panic!("no listed window at {x},{y}: {listing}"))["window_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The handle of the ordinary window `other_client()` (at 2000,100).
    pub(super) async fn other_handle(&self) -> String {
        Self::handle_at(&self.listing().await, 2000, 100)
    }

    /// A mapped window on workspace `ws`.
    pub(super) fn window(address: &str, at: (i64, i64), size: (i64, i64), ws: i64) -> Value {
        json!({
            "address": address, "class": "kitty", "title": "plain",
            "initialClass": "kitty", "initialTitle": "plain", "mapped": true,
            "hidden": false, "at": [at.0, at.1], "size": [size.0, size.1],
            "workspace": {"id": ws}, "focusHistoryID": 3
        })
    }

    pub(super) fn input_any(&self, window_id: &str, chord: &str) -> A {
        A::InputAnyKeyPress {
            window_id: window_id.to_owned(),
            chord: chord.to_owned(),
        }
    }
}

pub(super) fn foreign_active() -> Value {
    json!({
        "address": "0xbeef", "class": "alacritty",
        "title": "shell", "initialClass": "alacritty", "initialTitle": "shell"
    })
}

pub(super) fn other_client() -> Value {
    json!({
        "address": "0xbeef", "class": "alacritty", "title": "shell",
        "initialClass": "alacritty", "initialTitle": "shell", "mapped": true,
        "hidden": false, "at": [2000, 100], "size": [500, 300], "workspace": {"id": 1}
    })
}

fn default_monitors() -> Value {
    json!([
        {"id": 0, "name": "DP-1", "description": "Ultrawide", "width": 3440, "height": 1440,
         "x": 0, "y": 0, "refreshRate": 144.0, "scale": 1.0, "transform": 0, "focused": true,
         "dpmsStatus": true, "activeWorkspace": {"id": 1, "name": "1"}},
        {"id": 1, "name": "HDMI-A-1", "description": "Side", "width": 1920, "height": 1080,
         "x": 3440, "y": 0, "refreshRate": 60.0, "scale": 1.0, "transform": 0, "focused": false,
         "dpmsStatus": true, "activeWorkspace": {"id": 5, "name": "5"}}
    ])
}

/// A clock that reads `offsets_ms[n]` (from a fixed start) at its n-th read
/// and stays at the last one afterwards.
pub(super) fn scripted_clock(offsets_ms: &[u64]) -> Clock {
    let base = Instant::now();
    let offsets = offsets_ms.to_vec();
    let reads = Arc::new(AtomicUsize::new(0));
    Arc::new(move || {
        let n = reads.fetch_add(1, Ordering::SeqCst).min(offsets.len() - 1);
        base + Duration::from_millis(offsets[n])
    })
}

// ------------------------------------------------ helpers of the refusal tests

pub(super) fn pair(e: &Fail) -> (&'static str, String) {
    (e.code, e.message.clone())
}

pub(super) fn calls(fx: &Fx) -> usize {
    fx.rig.calls().len()
}

/// A run that must fail; `Err` is the failure.
pub(super) async fn refused(fx: &Fx, probe: &Probe, action: &A) -> Fail {
    fx.run(action, probe).await.unwrap_err()
}

/// A refusal that ran no program and wrote no claim.
pub(super) async fn nothing(fx: &Fx, probe: &Probe, action: &A, code: &str) {
    let e = refused(fx, probe, action).await;
    assert_eq!(e.code, code, "{e:?}");
    assert_eq!(calls(fx), 0, "{:?}", fx.timeline());
    assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
}
