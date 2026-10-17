//! The Fleet Git loopback rig (T1.7, design §16.1, owner acceptance A2): one
//! deck, several stations with separate data directories, and one bare
//! repository `remote.git` as the remote of every home. The deck and every
//! station process get `FLEETDECK_ALLOW_LOCAL_REMOTES=1` (phase 1: T1.3 and
//! T1.5 read it until T2.1 and T2.2 add the station reports and the
//! station.toml key). The project `remote_url` is `file://<canonical temp
//! dir>/remote.git`, and the temp dir is an allowed root of every station, so
//! the confined fetch and push can write there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{Event, SessionState};
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

use super::deck::{Deck, Reply};
use super::{Station, WAIT, fleetdeck, open_store, stored_events};

/// The variable that lets the deck and the stations accept a local remote.
pub const ALLOW_LOCAL: (&str, &str) = ("FLEETDECK_ALLOW_LOCAL_REMOTES", "1");

/// What a test changes in the rig.
#[derive(Default)]
pub struct Options {
    /// A provider program (a script that runs the fake Claude) instead of the
    /// fake Claude itself.
    pub provider: Option<PathBuf>,
    /// More environment variables of every station process.
    pub station_env: Vec<(String, String)>,
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

        let deck = Deck::start_with_env(&[ALLOW_LOCAL]);
        let (board, token) = deck.board();
        let login = deck.https(
            board,
            "POST",
            "/api/auth/link",
            &[],
            Some(json!({ "token": token })),
        );
        assert_eq!(login.status, 200, "{}", login.body);
        let (cookie, csrf) = (
            cookie_of(&login.head),
            login.body["csrf"].as_str().unwrap().to_owned(),
        );
        let mut rig = Self {
            deck,
            root,
            remote,
            stations: Vec::new(),
            board,
            cookie,
            csrf,
            commands: std::cell::Cell::new(0),
            _tmp: tmp,
        };
        for name in names {
            rig.add_station(name, &options);
        }
        rig
    }

    fn add_station(&mut self, name: &str, options: &Options) {
        let mut station = Station::named(name);
        let path = station.node.dirs().station_config();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text = text.replace(
            "allowed_roots = [\"~/projects\"]",
            &format!("allowed_roots = [\"~/projects\", {:?}]", self.root),
        );
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
        cmd.env(ALLOW_LOCAL.0, ALLOW_LOCAL.1);
        cmd.envs(options.station_env.iter().map(|(k, v)| (k, v)));
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
            if seen.len() == self.stations.len() && seen.iter().all(|s| s.reachable) {
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
        let id = self.create_project(i, name);
        let mut settings = settings;
        settings["sync"] = json!(true);
        let r = self.save_git(&id, settings);
        assert_eq!(r.status, 200, "{}", r.body);
        self.wait_home(&id, "ready");
        id
    }

    /// `POST /api/projects` for the home of station `i`, with no Git settings.
    pub fn create_project(&self, i: usize, name: &str) -> String {
        let s = &self.stations[i];
        let body = json!({
            "name": name, "station": s.name, "path": s.home,
            "trackers": []
        });
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

    /// Moves `remote.git` away; the homes still point to the old path.
    pub fn remove_remote(&self) -> PathBuf {
        let gone = self.root.join("remote.gone");
        std::fs::rename(&self.remote, &gone).unwrap();
        gone
    }
}
