//! A `fleetdeck station serve` process on a temporary node, with
//! `fake-claude sim` as its only provider instance. Every station process
//! of one `Station` shares the node (data directory, station.db, socket)
//! and one stdin log of every fake-claude process.

#![allow(dead_code, reason = "each test binary uses a part of the harness")]

pub mod deck;
pub mod fleet_git;
pub mod fleet_git_coord;
pub mod fleet_git_land;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_station::fault::FAULT_ENV;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_station::store::{Effect, ReadResult, Store, StoreConfig};
use fleetdeck_testkit::sim_claude::StdinLog;
use fleetdeck_testkit::{Det, FakeAgent, TempNode};

pub const WAIT: Duration = Duration::from_secs(20);

pub struct Station {
    pub node: TempNode,
    pub workspace: PathBuf,
    stdin_log: PathBuf,
    child: Option<Child>,
}

impl Station {
    /// Writes the node's station.toml and starts the station.
    pub fn start() -> Self {
        let mut station = Self::new();
        station.spawn(None);
        station
    }

    /// Writes the node's station.toml; starts nothing.
    pub fn new() -> Self {
        Self::named("cli")
    }

    /// A station whose station.toml name is `name`.
    pub fn named(name: &str) -> Self {
        let node = TempNode::new(name).unwrap();
        let workspace = node.workspace("demo").unwrap();
        let stdin_log = node.root().join("fake-stdin.log");
        let bins = FakeAgent::bin_dir().unwrap();
        FakeAgent::Claude.locate().unwrap();
        let mut text = node.station_toml(&bins, &[FakeAgent::Claude]);
        text.push_str(&format!(
            "env = {{ FLEETDECK_FAKE_SCENARIO = \"sim\", FLEETDECK_FAKE_STDIN_LOG = {:?} }}\n",
            stdin_log.to_str().unwrap()
        ));
        node.write_station_toml(&text).unwrap();
        Self {
            node,
            workspace,
            stdin_log,
            child: None,
        }
    }

    /// Starts `fleetdeck station serve`, armed with a fault spec if given,
    /// and waits until its socket answers.
    pub fn spawn(&mut self, fault: Option<&str>) {
        let cmd = fleetdeck(&self.node);
        self.spawn_cmd(cmd, fault);
    }

    /// [`Self::spawn`] with another `fleetdeck` binary (an installed
    /// release after a self-upgrade), with the node's arguments and
    /// environment.
    #[allow(dead_code, reason = "used by some test binaries only")]
    pub fn spawn_binary(&mut self, binary: &Path) {
        let mut cmd = fleetdeck(&self.node);
        let args: Vec<_> = cmd.get_args().map(ToOwned::to_owned).collect();
        let envs: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(ToOwned::to_owned)))
            .collect();
        cmd = Command::new(binary);
        cmd.args(args);
        for (k, v) in envs {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        self.spawn_cmd(cmd, None);
    }

    fn spawn_cmd(&mut self, mut cmd: Command, fault: Option<&str>) {
        assert!(self.child.is_none(), "a station process runs already");
        cmd.args(["station", "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(spec) = fault {
            cmd.env(FAULT_ENV, spec);
        }
        self.child = Some(cmd.spawn().unwrap());
        let deadline = Instant::now() + WAIT;
        while !self.cli(&["station", "sessions"]).status.success() {
            assert!(Instant::now() < deadline, "the station did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        fleetdeck(&self.node).args(args).output().unwrap()
    }

    /// Runs a CLI command that must succeed; returns its stdout.
    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "fleetdeck {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs a CLI command with `input` on stdin; it must succeed. Returns
    /// its stdout.
    pub fn ok_stdin(&self, args: &[&str], input: &str) -> String {
        with_stdin(&self.node, args, input)
    }

    /// Runs a CLI command that must fail; returns its stderr.
    pub fn fails(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert_eq!(out.status.code(), Some(1), "fleetdeck {args:?} must fail");
        String::from_utf8(out.stderr).unwrap()
    }

    pub fn sessions(&self) -> Vec<SessionSummary> {
        serde_json::from_str(&self.ok(&["station", "sessions", "--json"])).unwrap()
    }

    pub fn wait_for(
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

    /// `fleetdeck station run`; returns the new session id.
    pub fn run(&self, prompt: &str) -> String {
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

    /// One request on the station socket. `None` when the station closed
    /// the connection without an answer (for example, it crashed).
    pub async fn call(&self, request: &LocalRequest) -> Option<LocalResponse> {
        let path = socket_path(&self.node.dirs().station());
        let mut client = LocalClient::connect(&path).await.unwrap();
        client.call(request).await.ok()
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// Sends a signal to the station process.
    pub fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .args([&format!("-{name}"), &self.pid().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Waits until the station process exits.
    pub fn wait_exit(&mut self) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the station did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Waits for a crash: the process ended by a signal, not by an exit.
    pub fn wait_crash(&mut self) -> i32 {
        let status = self.wait_exit();
        status
            .signal()
            .unwrap_or_else(|| panic!("the station exited without a crash: {status:?}"))
    }

    /// SIGTERM, then the station must stop its sessions and exit 0.
    pub fn stop(&mut self) {
        self.signal("TERM");
        let status = self.wait_exit();
        assert!(status.success(), "station exit: {status:?}");
    }

    pub fn db(&self) -> PathBuf {
        self.node.dirs().station_db()
    }

    /// Every stdin line that a fake-claude process of this node received.
    pub fn stdin(&self) -> StdinLog {
        StdinLog::read(&self.stdin_log)
    }

    /// Waits until every fake-claude process of this node has exited (the
    /// stdin pipe of a crashed station closes; `sim` then ends).
    pub fn wait_providers_gone(&self) {
        let deadline = Instant::now() + WAIT;
        for pid in self.stdin().pids {
            while Path::new(&format!("/proc/{pid}")).exists() && !zombie(pid) {
                assert!(Instant::now() < deadline, "fake-claude {pid} still runs");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
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

fn zombie(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| {
            s.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(true)
}

/// Runs `fleetdeck <args>` on `node` with `input` on stdin; it must
/// succeed. Returns its stdout.
pub fn with_stdin(node: &TempNode, args: &[&str], input: &str) -> String {
    let out = run_stdin(node, args, input);
    assert!(
        out.status.success(),
        "fleetdeck {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Runs `fleetdeck args` with `input` as the first line of stdin.
pub fn run_stdin(node: &TempNode, args: &[&str], input: &str) -> std::process::Output {
    use std::io::Write as _;
    let mut child = fleetdeck(node)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{input}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

pub fn fleetdeck(node: &TempNode) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleetdeck"));
    cmd.arg("--data-dir")
        .arg(node.data_dir())
        .env("HOME", node.home())
        .env_remove("FLEETDECK_DATA_DIR")
        .env_remove(FAULT_ENV)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("RUST_LOG", "warn");
    cmd
}

/// Opens a stopped station's store (no recovery runs).
pub async fn open_store(db: &Path) -> Store {
    let det = Det::new();
    Store::open(db, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap()
}

/// Every known event of the store, with its session id.
pub async fn stored_events(store: &Store) -> Vec<(String, Event)> {
    let ReadResult::Events { events, .. } = store.read_after(0, 100_000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(event) => Some((e.session_id.as_str().to_owned(), event)),
            EventBody::Unknown { .. } => None,
        })
        .collect()
}

/// The effects that one command queued.
pub async fn effects_of(store: &Store, session: &str, command: &str) -> Vec<Effect> {
    store
        .session_effects(session.to_owned().into())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.command_id.as_ref().is_some_and(|c| c.as_str() == command))
        .collect()
}
