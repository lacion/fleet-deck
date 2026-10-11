//! M2 P2-7 exit test (PLAN §13 Phase 2): a Claude session (fake-claude)
//! runs, asks for approval, is answered and interrupted from the
//! station-local CLI. The station and every CLI call are separate
//! `fleetdeck` processes; the station talks to fake-claude through a real
//! process and the Unix socket.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::Event;
use fleetdeck_proto::session::Activity;
use fleetdeck_station::store::{ReadResult, Store, StoreConfig};
use fleetdeck_testkit::{Det, FakeAgent, TempNode};

const WAIT: Duration = Duration::from_secs(20);

struct Station {
    node: TempNode,
    workspace: PathBuf,
    child: Option<Child>,
}

impl Station {
    fn start() -> Self {
        let node = TempNode::new("cli").unwrap();
        let workspace = node.workspace("demo").unwrap();
        let bins = FakeAgent::bin_dir().unwrap();
        FakeAgent::Claude.locate().unwrap();
        let mut text = node.station_toml(&bins, &[FakeAgent::Claude]);
        text.push_str("env = { FLEETDECK_FAKE_SCENARIO = \"sim\" }\n");
        node.write_station_toml(&text).unwrap();
        let child = fleetdeck(&node)
            .args(["station", "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let station = Self {
            node,
            workspace,
            child: Some(child),
        };
        let deadline = Instant::now() + WAIT;
        while !station.cli(&["station", "sessions"]).status.success() {
            assert!(Instant::now() < deadline, "the station did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        station
    }

    fn cli(&self, args: &[&str]) -> Output {
        fleetdeck(&self.node).args(args).output().unwrap()
    }

    /// Runs a CLI command that must succeed; returns its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "fleetdeck {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs a CLI command that must fail; returns its stderr.
    fn fails(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert_eq!(out.status.code(), Some(1), "fleetdeck {args:?} must fail");
        String::from_utf8(out.stderr).unwrap()
    }

    fn sessions(&self) -> Vec<SessionSummary> {
        serde_json::from_str(&self.ok(&["station", "sessions", "--json"])).unwrap()
    }

    fn wait_for(
        &self,
        what: &str,
        session: &str,
        f: impl Fn(&SessionSummary) -> bool,
    ) -> SessionSummary {
        let deadline = Instant::now() + WAIT;
        loop {
            let found = self
                .sessions()
                .into_iter()
                .find(|s| s.session_id.as_str() == session);
            if let Some(s) = found.filter(|s| f(s)) {
                return s;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn run(&self, prompt: &str) -> String {
        let cwd = self.workspace.to_str().unwrap().to_owned();
        let provider = FakeAgent::Claude.instance_id();
        let out = self.ok(&[
            "station",
            "run",
            "--provider",
            provider,
            "--cwd",
            &cwd,
            prompt,
        ]);
        out.trim().to_owned()
    }

    /// SIGTERM, then the station must stop its sessions and exit 0.
    fn stop(&mut self) {
        let mut child = self.child.take().unwrap();
        let status = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + WAIT;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the station did not stop");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(status.success(), "station exit: {status:?}");
    }

    fn db(&self) -> PathBuf {
        self.node.dirs().station_db()
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn fleetdeck(node: &TempNode) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleetdeck"));
    cmd.arg("--data-dir")
        .arg(node.data_dir())
        .env("HOME", node.home())
        .env_remove("FLEETDECK_DATA_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("RUST_LOG", "warn");
    cmd
}

async fn stored_events(db: &Path) -> Vec<Event> {
    let det = Det::new();
    let store = Store::open(db, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let ReadResult::Events { events, .. } = store.read_after(0, 10_000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            fleetdeck_proto::event::EventBody::Known(event) => Some(event),
            fleetdeck_proto::event::EventBody::Unknown { .. } => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_session_runs_asks_for_approval_is_answered_and_interrupted_from_the_cli() {
    let mut station = Station::start();
    assert_eq!(station.ok(&["station", "sessions"]).trim(), "no sessions");

    // 1. A session runs and asks for approval.
    let first = station.run("ask ls");
    let asking = station.wait_for("the approval", &first, |s| !s.open_requests.is_empty());
    assert_eq!(asking.activity, Activity::NeedsYou);
    let request = asking.open_requests[0].request_id.as_str().to_owned();
    let text = station.ok(&["station", "sessions"]);
    assert!(
        text.contains(&first) && text.contains("needs_you"),
        "{text}"
    );
    assert!(text.contains(&format!("request {request}")), "{text}");

    // 2. The CLI answers it; the tool runs and the turn ends.
    let answered = station.ok(&["station", "approve", &request]);
    assert!(answered.contains("answered"), "{answered}");
    station.wait_for("the answered turn to end", &first, |s| {
        s.open_requests.is_empty() && s.current_turn.is_none() && s.activity == Activity::Idle
    });
    // A second answer to the same request sends nothing (L3).
    station.ok(&["station", "approve", &request]);

    // 3. A second session runs a long turn; the CLI interrupts it.
    let second = station.run("wait for the interrupt");
    station.wait_for("the long turn", &second, |s| {
        s.activity == Activity::Working
    });
    let sent = station.ok(&["station", "interrupt", &second]);
    assert!(sent.contains("interrupt sent"), "{sent}");
    station.wait_for("the interrupted turn to end", &second, |s| {
        s.current_turn.is_none() && s.activity == Activity::Idle
    });

    // Rejections and errors exit 1 with a reason.
    let err = station.fails(&["station", "interrupt", &second]);
    assert!(err.contains("rejected (invalid_state)"), "{err}");
    let err = station.fails(&["station", "approve", "req-nope"]);
    assert!(err.contains("no request `req-nope`"), "{err}");
    let outside = station.node.root().to_str().unwrap().to_owned();
    let provider = FakeAgent::Claude.instance_id();
    let err = station.fails(&[
        "station",
        "run",
        "--provider",
        provider,
        "--cwd",
        &outside,
        "hi",
    ]);
    assert!(err.contains("rejected (forbidden)"), "{err}");

    // 4. SIGTERM stops both sessions; the socket goes away.
    station.stop();
    let err = station.fails(&["station", "sessions"]);
    assert!(err.contains("no station answers"), "{err}");

    // The station's log holds the evidence of each step.
    let events = stored_events(&station.db()).await;
    let resolved = events
        .iter()
        .filter(|e| matches!(e, Event::ApprovalResolved { request_id, .. } if request_id.as_str() == request))
        .count();
    assert_eq!(resolved, 1, "one answer reached the provider");
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::ApprovalResolved { by, option_id, .. }
                if *by == fleetdeck_proto::event::ResolvedBy::Local && option_id == "allow"
        )),
        "the CLI answered as `local`"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::InterruptResult {
                accepted: true,
                control_id: Some(_),
                ..
            }
        )),
        "the provider confirmed the interrupt"
    );
    let ended = events
        .iter()
        .filter(|e| matches!(e, Event::SessionEnded { .. }))
        .count();
    assert_eq!(ended, 2, "both sessions ended at the shutdown");
}

#[test]
fn the_cli_reports_a_missing_station() {
    let node = TempNode::new("idle").unwrap();
    let out = fleetdeck(&node)
        .args(["station", "sessions"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("no station answers"), "{err}");
    assert!(err.contains("fleetdeck station serve"), "{err}");
}
