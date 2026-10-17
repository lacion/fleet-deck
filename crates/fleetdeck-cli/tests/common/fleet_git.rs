//! The Fleet Git loopback rig (T1.7, design §16.1, owner acceptance A2): one
//! deck, several stations with separate data directories, and one bare
//! repository `remote.git` as the remote of every home. Each station.toml
//! sets `allow_local_remotes = true` (design §16.1, M13), and the deck learns
//! it from `station.info`. The project `remote_url` is `file://<canonical
//! temp dir>/remote.git`, and the temp dir is an allowed root of every
//! station, so the confined fetch and push can write there.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{Event, SessionState};
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

use super::deck::{Deck, Reply};
use super::{Station, WAIT, fleetdeck, open_store, stored_events};

/// What a test changes in the rig.
#[derive(Default)]
pub struct Options {
    /// A provider program (a script that runs the fake Claude) instead of the
    /// fake Claude itself.
    pub provider: Option<PathBuf>,
    /// More environment variables of every station process.
    pub station_env: Vec<(String, String)>,
    /// The setup of single stations, by station name.
    pub setup: BTreeMap<String, StationSetup>,
    /// Programs for the PATH of every station process (T3.5): the name and
    /// the text of a script. `{FX}` stands for the folder `fx` below the
    /// temp dir of the rig (an allowed root, so a confined `git` can write
    /// there). The folder `fx/bin` goes first on the PATH.
    pub scripts: Vec<(String, String)>,
}

/// What differs for one station of the rig.
#[derive(Clone)]
pub struct StationSetup {
    /// Top-level `station.toml` lines (`traits = [...]`, `tailscale_bin = ...`).
    /// `{HOME}` and `{ROOT}` stand for the canonical HOME of the station and
    /// the temp dir of the rig, here and in `files`.
    pub toml: String,
    /// More environment variables of this station process.
    pub env: Vec<(String, String)>,
    /// Files below the HOME of the station, written before its first start:
    /// the path relative to HOME and the content.
    pub files: Vec<(String, Vec<u8>)>,
    /// The files (relative to HOME) that get mode 0755.
    pub executables: Vec<String>,
    /// `allow_local_remotes = true` in `station.toml` (the rig default).
    pub allow_local_remotes: bool,
}

impl Default for StationSetup {
    fn default() -> Self {
        Self {
            toml: String::new(),
            env: Vec::new(),
            files: Vec::new(),
            executables: Vec::new(),
            allow_local_remotes: true,
        }
    }
}

/// One station of the rig, and its home: a clone of `remote.git`.
pub struct RigStation {
    pub name: String,
    pub station: Station,
    pub home: PathBuf,
}

pub struct Rig {
    pub deck: Deck,
    /// The canonical temp dir: `remote.git` and the seed clone are below it.
    pub root: PathBuf,
    pub remote: PathBuf,
    pub stations: Vec<RigStation>,
    board: std::net::SocketAddr,
    cookie: String,
    csrf: String,
    /// The station.toml text options of `with`, for `add`.
    options: Options,
    /// `<root>/fx/bin` when `Options.scripts` is not empty.
    fx_bin: Option<PathBuf>,
    commands: std::cell::Cell<u32>,
    _tmp: tempfile::TempDir,
}

/// Runs `git args` in `dir` with a fixed identity; it must succeed. Returns
/// its trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Runs `git args` in `dir` and returns the whole output.
pub fn git_out(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args([
            "-c",
            "user.name=rig",
            "-c",
            "user.email=rig@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

/// Commits `file` with `content` in the work tree `dir`; returns the commit.
pub fn commit_in(dir: &Path, file: &str, content: &str, message: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Logs in to the board of `deck`: its address, the cookie and the CSRF token.
fn login(deck: &Deck) -> (std::net::SocketAddr, String, String) {
    let (board, token) = deck.board();
    let login = deck.https(
        board,
        "POST",
        "/api/auth/link",
        &[],
        Some(json!({ "token": token })),
    );
    assert_eq!(login.status, 200, "{}", login.body);
    (
        board,
        cookie_of(&login.head),
        login.body["csrf"].as_str().unwrap().to_owned(),
    )
}

fn cookie_of(head: &str) -> String {
    let line = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .unwrap();
    line.split_once(':')
        .unwrap()
        .1
        .trim()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

impl Rig {
    /// A rig with one station for each name.
    pub fn new(names: &[&str]) -> Self {
        Self::with(names, Options::default())
    }

    pub fn with(names: &[&str], options: Options) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("fleetdeck-fg-")
            .tempdir()
            .unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let remote = root.join("remote.git");
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                remote.to_str().unwrap(),
            ],
        );
        git(&seed, &["init", "-q", "-b", "main"]);
        commit_in(&seed, "README.md", "one\n", "first");
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-q", "origin", "main"]);

        let deck = Deck::start();
        let (board, cookie, csrf) = login(&deck);
        let mut rig = Self {
            deck,
            root,
            remote,
            stations: Vec::new(),
            board,
            cookie,
            csrf,
            options: Options::default(),
            fx_bin: None,
            commands: std::cell::Cell::new(0),
            _tmp: tmp,
        };
        if !options.scripts.is_empty() {
            rig.fx_bin = Some(rig.write_scripts(&options.scripts));
        }
        for name in names {
            let setup = options.setup.get(*name).cloned().unwrap_or_default();
            rig.add_with(name, &options, &setup);
        }
        rig.options = options;
        rig
    }

    /// Adds one more station (and its home clone) with `setup`.
    pub fn add(&mut self, name: &str, setup: &StationSetup) {
        let options = Options {
            provider: self.options.provider.clone(),
            station_env: self.options.station_env.clone(),
            setup: BTreeMap::new(),
            scripts: Vec::new(),
        };
        self.add_with(name, &options, setup);
    }

    /// Writes the scripts of `Options.scripts` to `<root>/fx/bin`; returns it.
    fn write_scripts(&self, scripts: &[(String, String)]) -> PathBuf {
        let fx = self.root.join("fx");
        let bin = fx.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for (name, text) in scripts {
            let file = bin.join(name);
            std::fs::write(&file, text.replace("{FX}", fx.to_str().unwrap())).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    /// `<fx_bin>:$PATH`, when the rig has scripts.
    fn path_with_scripts(&self) -> Option<String> {
        self.fx_bin.as_ref().map(|bin| {
            let old = std::env::var("PATH").unwrap_or_default();
            format!("{}:{old}", bin.display())
        })
    }

    fn add_with(&mut self, name: &str, options: &Options, setup: &StationSetup) {
        let mut station = Station::named(name);
        let path = station.node.dirs().station_config();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text = text.replace(
            "allowed_roots = [\"~/projects\"]",
            &format!("allowed_roots = [\"~/projects\", {:?}]", self.root),
        );
        let home_dir = station.node.home().canonicalize().unwrap();
        let fill = |text: &str| {
            text.replace("{HOME}", home_dir.to_str().unwrap())
                .replace("{ROOT}", self.root.to_str().unwrap())
        };
        let mut extra = fill(&setup.toml);
        if setup.allow_local_remotes {
            extra.push_str("allow_local_remotes = true\n");
        }
        assert!(text.contains("listen = []\n"), "the listen line: {text}");
        text = text.replace("listen = []\n", &format!("listen = []\n{extra}"));
        for (rel, content) in &setup.files {
            let file = station.node.home().join(rel);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            match std::str::from_utf8(content) {
                Ok(text) => std::fs::write(&file, fill(text)).unwrap(),
                Err(_) => std::fs::write(&file, content).unwrap(),
            }
            if setup.executables.contains(rel) {
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        if let Some(program) = &options.provider {
            let fake = FakeAgent::bin_dir().unwrap().join("fake-claude");
            let old = format!("binary = \"{}\"", fake.display());
            assert!(text.contains(&old), "the provider line: {text}");
            text = text.replace(&old, &format!("binary = \"{}\"", program.display()));
        }
        station.node.write_station_toml(&text).unwrap();
        let home = station.node.home().join("projects").join("app");
        git(
            &self.root,
            &["clone", "-q", &self.remote_url(), home.to_str().unwrap()],
        );
        let home = home.canonicalize().unwrap();
        let mut cmd = fleetdeck(&station.node);
        if let Some(path) = self.path_with_scripts() {
            cmd.env("PATH", path);
        }
        cmd.envs(options.station_env.iter().map(|(k, v)| (k, v)));
        cmd.envs(setup.env.iter().map(|(k, v)| (k, v)));
        station.spawn_cmd(cmd, None);
        let invitation = self.deck.ok(&["deck", "invite"]);
        station.ok_stdin(&["station", "join", "--stdin"], invitation.trim());
        self.stations.push(RigStation {
            name: name.to_owned(),
            station,
            home,
        });
        self.wait_reachable();
    }

    fn wait_reachable(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let seen = self.deck.stations();
            // The deck needs `station.info.fleet_git` (with the local-remote
            // flag) before a `file://` remote can be saved.
            let known = seen
                .iter()
                .all(|s| s.reachable && s.info.as_ref().is_some_and(|i| i.fleet_git.is_some()));
            if seen.len() == self.stations.len() && known {
                return;
            }
            assert!(Instant::now() < deadline, "stations not linked: {seen:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `file://<canonical temp dir>/remote.git`.
    pub fn remote_url(&self) -> String {
        format!("file://{}", self.remote.display())
    }

    /// One request to the board as the logged-in owner.
    pub fn board(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let auth = [
            ("Cookie", self.cookie.as_str()),
            ("X-CSRF-Token", self.csrf.as_str()),
        ];
        self.deck.https(self.board, method, path, &auth, body)
    }

    fn command_id(&self) -> String {
        let n = self.commands.get() + 1;
        self.commands.set(n);
        format!("rig-{n}")
    }

    /// Creates a project for the home of station `i`, saves `settings` with
    /// `sync = true` and the rig remote, and waits until the station verified
    /// the home. Returns the project id.
    pub fn project(&self, i: usize, name: &str, settings: Value) -> String {
        self.project_with(i, name, settings, Value::Null)
    }

    /// [`Self::project`] with the forge binding `forge` (`null`: none).
    pub fn project_with(&self, i: usize, name: &str, settings: Value, forge: Value) -> String {
        let id = self.create_project_with(i, name, forge);
        let mut settings = settings;
        settings["sync"] = json!(true);
        let r = self.save_git(&id, settings);
        assert_eq!(r.status, 200, "{}", r.body);
        self.wait_home(&id, "ready");
        id
    }

    /// `POST /api/projects` for the home of station `i`, with no Git settings.
    pub fn create_project(&self, i: usize, name: &str) -> String {
        self.create_project_with(i, name, Value::Null)
    }

    /// [`Self::create_project`] with the forge binding `forge`.
    pub fn create_project_with(&self, i: usize, name: &str, forge: Value) -> String {
        let s = &self.stations[i];
        let mut body = json!({
            "name": name, "station": s.name, "path": s.home,
            "trackers": []
        });
        if !forge.is_null() {
            body["forge"] = forge;
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let r = self.board("POST", "/api/projects", Some(body.clone()));
            if r.status == 200 {
                return r.body["project_id"].as_str().unwrap().to_owned();
            }
            let waiting = r.status == 404 || r.body["error"] == "station_unreachable";
            assert!(waiting && Instant::now() < deadline, "{}", r.body);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `PUT /api/projects/{id}/git` at the stored revision, with the rig
    /// remote. Returns the reply as it is.
    pub fn save_git(&self, project: &str, settings: Value) -> Reply {
        let revision = self.project_of(project)["git_revision"].as_u64().unwrap();
        self.board(
            "PUT",
            &format!("/api/projects/{project}/git"),
            Some(json!({
                "settings": settings,
                "revision": revision,
                "remote_url": self.remote_url(),
            })),
        )
    }

    /// The project as `GET /api/projects` lists it.
    pub fn project_of(&self, project: &str) -> Value {
        let r = self.board("GET", "/api/projects", None);
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["project_id"] == project)
            .unwrap_or_else(|| panic!("no project {project}: {}", r.body))
            .clone()
    }

    /// Waits until the first home of the project reads `state`.
    pub fn wait_home(&self, project: &str, state: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let p = self.project_of(project);
            if p["homes"][0]["state"] == state {
                return;
            }
            assert!(Instant::now() < deadline, "the home is not {state}: {p}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A worktree start through the board in the home of station `i`;
    /// returns the reply. `extra` is merged into the command.
    pub fn start_reply(&self, i: usize, workspace: Value, extra: Value) -> Reply {
        self.start_at(i, &self.stations[i].home, workspace, extra)
    }

    /// [`Self::start_reply`] in the folder `cwd`.
    pub fn start_at(&self, i: usize, cwd: &Path, workspace: Value, extra: Value) -> Reply {
        let s = &self.stations[i];
        let mut command = json!({
            "op": "start",
            "spec": {
                "provider_instance": FakeAgent::Claude.instance_id(),
                "cwd": cwd,
            },
            "prompt": { "text": "hello" },
        });
        if !workspace.is_null() {
            command["workspace"] = workspace;
        }
        if let Value::Object(extra) = extra {
            for (k, v) in extra {
                command[k] = v;
            }
        }
        self.board(
            "POST",
            "/api/commands/session.start",
            Some(json!({
                "station": s.name,
                "command_id": self.command_id(),
                "command": command,
            })),
        )
    }

    /// A worktree start that must be accepted; returns the new session once
    /// the station lists it.
    pub fn start(&self, i: usize) -> SessionSummary {
        self.start_with(i, json!({ "kind": "worktree" }), Value::Null)
    }

    /// [`Self::start`] with a workspace request and extra command fields.
    pub fn start_with(&self, i: usize, workspace: Value, extra: Value) -> SessionSummary {
        self.start_in(i, &self.stations[i].home, workspace, extra)
    }

    /// [`Self::start_with`] in the folder `cwd`.
    pub fn start_in(&self, i: usize, cwd: &Path, workspace: Value, extra: Value) -> SessionSummary {
        let station = &self.stations[i].station;
        let before: Vec<_> = station
            .sessions()
            .into_iter()
            .map(|s| s.session_id.as_str().to_owned())
            .collect();
        let r = self.start_at(i, cwd, workspace, extra);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["receipt"]["state"], "accepted", "{}", r.body);
        let deadline = Instant::now() + WAIT;
        loop {
            let new = station
                .sessions()
                .into_iter()
                .find(|s| !before.contains(&s.session_id.as_str().to_owned()));
            if let Some(s) = new {
                return s;
            }
            assert!(Instant::now() < deadline, "the station has no new session");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The current summary of a session of station `i`.
    pub fn summary(&self, i: usize, session: &str) -> SessionSummary {
        self.stations[i]
            .station
            .sessions()
            .into_iter()
            .find(|s| s.session_id.as_str() == session)
            .unwrap_or_else(|| panic!("no session {session}"))
    }

    /// Waits until `f` holds for the summary of the session.
    pub fn wait_summary(
        &self,
        what: &str,
        i: usize,
        session: &str,
        f: impl Fn(&SessionSummary) -> bool,
    ) -> SessionSummary {
        self.stations[i].station.wait_for(what, session, f)
    }

    /// A follow-up prompt through the board (one finished turn).
    pub fn prompt(&self, i: usize, session: &str, text: &str) {
        let s = &self.stations[i];
        let r = self.board(
            "POST",
            "/api/commands/session.follow_up",
            Some(json!({
                "station": s.name,
                "command_id": self.command_id(),
                "command": {
                    "op": "follow_up",
                    "session_id": session,
                    "input": { "text": text },
                },
            })),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["receipt"]["state"], "accepted", "{}", r.body);
    }

    /// Stops a session of station `i` through the board and waits until it
    /// is `exited`.
    pub fn stop_session(&self, i: usize, session: &str) {
        let s = &self.stations[i];
        let r = self.board(
            "POST",
            "/api/commands/session.stop",
            Some(json!({
                "station": s.name,
                "command_id": self.command_id(),
                "command": { "op": "stop", "session_id": session, "how": "graceful" },
            })),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["receipt"]["state"], "accepted", "{}", r.body);
        self.wait_summary("the session ended", i, session, |s| {
            s.state == SessionState::Exited
        });
    }

    /// Stops station `i` (SIGTERM) and returns every event of its store, with
    /// its session id. The station stays down.
    pub async fn events_after_stop(&mut self, i: usize) -> Vec<(String, Event)> {
        self.stations[i].station.stop();
        let store = open_store(&self.stations[i].station.db()).await;
        stored_events(&store).await
    }

    /// Every ref of `remote.git` with its commit.
    pub fn bare_refs(&self) -> BTreeMap<String, String> {
        git(
            &self.remote,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(r, o)| (r.to_owned(), o.to_owned()))
        .collect()
    }

    /// A new commit on `main` of the remote (through the seed clone); returns
    /// its id.
    pub fn advance_remote(&self, file: &str, message: &str) -> String {
        let seed = self.root.join("seed");
        let oid = commit_in(&seed, file, message, message);
        git(&seed, &["push", "-q", "origin", "main"]);
        oid
    }

    /// The deck's view of station `i`.
    fn view(&self, i: usize) -> Option<fleetdeck_proto::board::StationView> {
        let name = &self.stations[i].name;
        self.deck.stations().into_iter().find(|s| &s.name == name)
    }

    /// The deck's id of station `i`.
    pub fn station_id(&self, i: usize) -> String {
        let view = self.view(i).expect("the deck lists the station");
        view.station_id.as_str().to_owned()
    }

    /// The `station.info` that the deck last got from station `i`.
    pub fn station_info(&self, i: usize) -> fleetdeck_proto::link::StationInfo {
        self.view(i).and_then(|s| s.info).expect("a station.info")
    }

    /// Polls until `f` holds for the `station.info` of station `i`.
    pub fn wait_info(
        &self,
        what: &str,
        i: usize,
        f: impl Fn(&fleetdeck_proto::link::StationInfo) -> bool,
    ) -> fleetdeck_proto::link::StationInfo {
        let deadline = Instant::now() + WAIT;
        loop {
            let info = self.view(i).filter(|s| s.reachable).and_then(|s| s.info);
            if let Some(info) = info.filter(|info| f(info)) {
                return info;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The home of the project on station `i`, as the project lists it.
    pub fn home_on(&self, project: &str, i: usize) -> Option<Value> {
        let id = self.station_id(i);
        self.project_of(project)["homes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["station_id"] == id)
            .cloned()
    }

    /// Waits until the home of the project on station `i` reads `state`.
    pub fn wait_home_on(&self, project: &str, i: usize, state: &str) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(h) = self.home_on(project, i).filter(|h| h["state"] == state) {
                return h;
            }
            assert!(
                Instant::now() < deadline,
                "the home on {i} is not {state}: {}",
                self.project_of(project)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `POST /api/projects/{id}/homes` for station `i`.
    pub fn add_home(&self, project: &str, i: usize) -> Reply {
        let body = json!({ "station": self.station_id(i), "command_id": self.command_id() });
        self.board(
            "POST",
            &format!("/api/projects/{project}/homes"),
            Some(body),
        )
    }

    /// `POST /api/projects/{id}/homes/{station}/retry` for station `i`.
    pub fn retry_home(&self, project: &str, i: usize) -> Reply {
        let path = format!("/api/projects/{project}/homes/{}/retry", self.station_id(i));
        let body = json!({ "command_id": self.command_id() });
        self.board("POST", &path, Some(body))
    }

    /// `DELETE /api/projects/{id}/homes/{station}` for station `i`.
    pub fn remove_home(&self, project: &str, i: usize) -> Reply {
        let path = format!("/api/projects/{project}/homes/{}", self.station_id(i));
        self.board("DELETE", &path, None)
    }

    /// Stops station `i` (SIGTERM) and starts it again with `env`.
    pub fn restart_station(&mut self, i: usize, env: &[(&str, &str)]) {
        self.stations[i].station.stop();
        self.start_station(i, env);
    }

    /// SIGKILL for station `i`; it stays down.
    pub fn kill_station(&mut self, i: usize) {
        self.stations[i].station.signal("KILL");
        self.stations[i].station.wait_crash();
    }

    /// Starts the stopped station `i` with `env` and waits until the deck got
    /// a `station.info` that is newer than before.
    pub fn start_station(&mut self, i: usize, env: &[(&str, &str)]) {
        let before = self.view(i).and_then(|s| s.info_at);
        let mut cmd = fleetdeck(&self.stations[i].station.node);
        if let Some(path) = self.path_with_scripts() {
            cmd.env("PATH", path);
        }
        cmd.envs(self.options.station_env.iter().map(|(k, v)| (k, v)));
        cmd.envs(env.iter().copied());
        self.stations[i].station.spawn_cmd(cmd, None);
        let deadline = Instant::now() + WAIT;
        loop {
            if self
                .view(i)
                .is_some_and(|s| s.reachable && s.info_at > before)
            {
                return;
            }
            assert!(Instant::now() < deadline, "station {i} is not linked again");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Stops the deck, lets `swap` replace `deck.db` (the deck is down), and
    /// starts the deck again. The board login is made again. The stations
    /// dial the deck again by themselves.
    pub fn restart_deck_with(&mut self, swap: impl FnOnce(&Path)) {
        self.deck.stop();
        swap(&self.deck.node.dirs().deck_db());
        self.deck.spawn();
        let (board, cookie, csrf) = login(&self.deck);
        (self.board, self.cookie, self.csrf) = (board, cookie, csrf);
        self.wait_reachable();
    }

    /// Moves `remote.git` away; the homes still point to the old path.
    pub fn remove_remote(&self) -> PathBuf {
        let gone = self.root.join("remote.gone");
        std::fs::rename(&self.remote, &gone).unwrap();
        gone
    }
}
