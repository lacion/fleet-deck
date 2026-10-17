//! The coordinator harness of the Fleet Git FG-10 tests on loopback (T4.6,
//! design §13.3, §16.1): a coordinator session on a real station, a fleet
//! grant over several stations, and the tools of its MCP endpoint, called over
//! HTTP as the provider would call them.
//!
//! The provider program of the rig is a wrapper script. When the station sets
//! `FLEETDECK_COORDINATOR_TOKEN` it writes the token and its arguments (with
//! the `--mcp-config` of the endpoint) next to itself, then runs the fake
//! Claude. The tests read the credential from there.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_proto::fleet_git::PushState;
use fleetdeck_proto::untrusted::fence;
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

use super::WAIT;
use super::fleet_git::{Options, Rig, StationSetup, commit_in};
use super::fleet_git_land;

/// The provider program in `dir`: it keeps the coordinator credential and
/// arguments in `dir/coord.token` and `dir/coord.args`.
#[must_use]
pub fn provider_program(dir: &Path) -> PathBuf {
    let fake = FakeAgent::Claude.locate().unwrap();
    let script = dir.join("provider.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             d=\"$(dirname \"$0\")\"\n\
             if [ -n \"$FLEETDECK_COORDINATOR_TOKEN\" ]; then\n\
             \x20 printf '%s\\n' \"$@\" > \"$d/coord.args.tmp\"\n\
             \x20 mv \"$d/coord.args.tmp\" \"$d/coord.args\"\n\
             \x20 printf '%s' \"$FLEETDECK_COORDINATOR_TOKEN\" > \"$d/coord.token.tmp\"\n\
             \x20 mv \"$d/coord.token.tmp\" \"$d/coord.token\"\n\
             fi\n\
             exec '{}' \"$@\"\n",
            fake.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// The options of a coordinator test: those of a land test (committer
/// identity, `allow_check_cmd`, the `git` wrapper), the provider program of
/// [`provider_program`] in `dir`, and `allow_fleet_control = true` on every
/// named station.
#[must_use]
pub fn options(dir: &Path, stations: &[&str]) -> Options {
    let mut options = fleet_git_land::options(stations);
    options.provider = Some(provider_program(dir));
    for name in stations {
        let setup: &mut StationSetup = options.setup.get_mut(*name).unwrap();
        setup.toml.push_str("allow_fleet_control = true\n");
    }
    options
}

/// The tool answer of one `tools/call`.
#[derive(Debug)]
pub struct ToolAnswer {
    pub is_error: bool,
    pub text: String,
}

impl ToolAnswer {
    /// The JSON of a successful answer.
    #[must_use]
    pub fn json(&self) -> Value {
        assert!(!self.is_error, "a tool error: {}", self.text);
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("{e}: {}", self.text))
    }
}

/// A started coordinator: its endpoint, its credential and its grant.
pub struct Coord {
    port: u16,
    token: String,
    pub grant_id: String,
    pub session_id: String,
    next: std::cell::Cell<u64>,
}

/// The one-time code of `fleetdeck open --elevate`, turned into an
/// elevation of the board login of the rig.
fn elevation(rig: &Rig) -> String {
    let out = rig.deck.ok(&["open", "--elevate"]);
    let code = out
        .strip_prefix("Elevation code: ")
        .and_then(|r| r.split(' ').next())
        .unwrap_or_else(|| panic!("unexpected output: {out:?}"))
        .to_owned();
    let r = rig.board("POST", "/api/auth/elevate", Some(json!({ "code": code })));
    assert_eq!(r.status, 200, "{}", r.body);
    r.body["elevation"].as_str().unwrap().to_owned()
}

/// Waits until `file` exists; returns its text.
fn wait_text(file: &Path) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(file) {
            return text;
        }
        assert!(Instant::now() < deadline, "no file {}", file.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Coord {
    /// Turns the deck setting `fleet_control` on, starts a coordinator session
    /// on station `i` (a fleet grant with `classes`, the station home as its
    /// folder), and waits until the deck bound the grant. `dir` is the
    /// folder of [`provider_program`].
    pub fn start(rig: &Rig, dir: &Path, i: usize, classes: &[&str]) -> Self {
        let elevation = elevation(rig);
        // The deck switch of fleet control (a live setting).
        let view = rig.board("GET", "/api/settings/deck", None);
        assert_eq!(view.status, 200, "{}", view.body);
        let saved = rig.board(
            "POST",
            "/api/settings/deck",
            Some(json!({
                "changes": { "fleet_control": true },
                "revision": view.body["revision"],
                "elevation": elevation,
            })),
        );
        assert_eq!(saved.status, 200, "{}", saved.body);

        let s = &rig.stations[i];
        let r = rig.board(
            "POST",
            "/api/commands/session.start",
            Some(json!({
                "station": s.name,
                "command_id": "coord-start-1",
                "elevation": elevation,
                "command": {
                    "op": "start",
                    "prompt": { "text": "coordinate" },
                    "spec": {
                        "provider_instance": FakeAgent::Claude.instance_id(),
                        "cwd": s.home,
                        "coordinator": {
                            "grant_id": "",
                            "classes": classes,
                            "scope": "fleet",
                            "idle_ttl_ms": 0,
                            "expires_at": 0,
                            "control_skill": { "version": "", "text": "" },
                        },
                    },
                },
            })),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["receipt"]["state"], "accepted", "{}", r.body);

        let token = wait_text(&dir.join("coord.token"));
        let args = wait_text(&dir.join("coord.args"));
        let config = args
            .lines()
            .skip_while(|a| *a != "--mcp-config")
            .nth(1)
            .unwrap_or_else(|| panic!("no --mcp-config: {args}"));
        let config: Value = serde_json::from_str(config).unwrap();
        let url = config["mcpServers"]["fleetdeck"]["url"].as_str().unwrap();
        let port = url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|r| r.strip_suffix("/mcp"))
            .unwrap_or_else(|| panic!("the endpoint: {url}"))
            .parse()
            .unwrap();

        // The grant of this station that the deck bound.
        let station = rig.station_id(i);
        let deadline = Instant::now() + WAIT;
        let grant = loop {
            let r = rig.board("GET", "/api/coordinator/grants", None);
            assert_eq!(r.status, 200, "{}", r.body);
            let found = r.body.as_array().unwrap().iter().find(|g| {
                g["station_id"] == station && g["state"] == "active" && !g["generation"].is_null()
            });
            if let Some(g) = found {
                break g.clone();
            }
            assert!(
                Instant::now() < deadline,
                "the grant is not bound: {}",
                r.body
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        Self {
            port,
            token,
            grant_id: grant["grant_id"].as_str().unwrap().to_owned(),
            session_id: grant["session_id"].as_str().unwrap().to_owned(),
            next: std::cell::Cell::new(0),
        }
    }

    /// Sets the stations of the grant to the stations `which` of the rig. The
    /// product has no route for it (a known gap): the test stops the deck,
    /// edits `deck.db`, and starts the deck again. The edit has two parts: the
    /// `stations` of the grant, and the link scope `coordinate` of each of
    /// those stations (a target station asks `coordinator.authority_check`
    /// with it; the deck gives it only to the station of the coordinator).
    pub fn widen(&self, rig: &mut Rig, which: &[usize]) {
        let ids: Vec<String> = which.iter().map(|i| rig.station_id(*i)).collect();
        let grant = self.grant_id.clone();
        rig.restart_deck_with(|db| {
            let conn = rusqlite::Connection::open(db).unwrap();
            let changed = conn
                .execute(
                    "UPDATE tokens SET stations = ?1 WHERE grant_id = ?2",
                    rusqlite::params![serde_json::to_string(&ids).unwrap(), grant],
                )
                .unwrap();
            assert_eq!(changed, 1, "one grant row");
            for id in &ids {
                let text: String = conn
                    .query_row(
                        "SELECT scopes FROM stations WHERE station_id = ?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap();
                let mut scopes: Vec<String> = serde_json::from_str(&text).unwrap();
                if !scopes.iter().any(|s| s == "coordinate") {
                    scopes.push("coordinate".to_owned());
                }
                conn.execute(
                    "UPDATE stations SET scopes = ?2 WHERE station_id = ?1",
                    rusqlite::params![id, serde_json::to_string(&scopes).unwrap()],
                )
                .unwrap();
            }
        });
    }

    /// One JSON-RPC message to the endpoint over plain HTTP; returns the
    /// status and the JSON body.
    fn post(&self, message: &Value) -> (u16, Value) {
        let body = message.to_string();
        let mut tcp = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        tcp.set_read_timeout(Some(WAIT)).unwrap();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n\
             Authorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.port,
            self.token,
            body.len()
        );
        tcp.write_all(request.as_bytes()).unwrap();
        let mut raw = String::new();
        tcp.read_to_string(&mut raw).unwrap();
        let (head, text) = raw.split_once("\r\n\r\n").expect("an HTTP response");
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(text).unwrap_or(Value::Null))
    }

    fn request(&self, method: &str, params: Value) -> Value {
        let id = self.next.get() + 1;
        self.next.set(id);
        let (status, body) = self.post(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        assert_eq!(status, 200, "{body}");
        body["result"].clone()
    }

    /// The names of the tools that the grant lists.
    #[must_use]
    pub fn tools(&self) -> Vec<String> {
        let result = self.request("tools/list", json!({}));
        result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `tools/call name arguments`.
    #[must_use]
    pub fn call(&self, name: &str, arguments: Value) -> ToolAnswer {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        ToolAnswer {
            is_error: result["isError"].as_bool().unwrap(),
            text: result["content"][0]["text"].as_str().unwrap().to_owned(),
        }
    }
}

/// A synced session on station `i` in the folder `cwd` (a home of `project`)
/// whose first turn is over.
pub fn idle_in(rig: &Rig, i: usize, cwd: &Path, project: &str) -> fleetdeck_proto::SessionSummary {
    let s = rig.start_in(
        i,
        cwd,
        json!({ "kind": "worktree" }),
        json!({ "project_id": project }),
    );
    let id = s.session_id.as_str().to_owned();
    rig.wait_summary("the first turn", i, &id, |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    })
}

/// Waits until the deck lists every one of `sessions` (a coordinator op
/// needs the session in the deck's read model).
pub fn wait_deck_knows(rig: &Rig, sessions: &[&fleetdeck_proto::SessionSummary]) {
    let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    rig.deck.wait_for("the deck lists the sessions", |cards| {
        ids.iter()
            .all(|id| cards.iter().any(|c| c.summary.session_id.as_str() == *id))
    });
}

/// An idle session on station `i` that committed `file` in its worktree and
/// ran one more turn, so that `push = on_turn` published the commit. Returns
/// the session and the commit.
pub fn pushed_session(
    rig: &Rig,
    i: usize,
    cwd: &Path,
    project: &str,
    file: &str,
) -> (fleetdeck_proto::SessionSummary, String) {
    let s = idle_in(rig, i, cwd, project);
    let id = s.session_id.as_str().to_owned();
    let oid = commit_in(
        &PathBuf::from(s.worktree.clone().unwrap()),
        file,
        file,
        file,
    );
    rig.prompt(i, &id, file);
    let want = oid.clone();
    let s = rig.wait_summary("the push", i, &id, move |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.as_deref() == Some(&want))
    });
    (s, oid)
}

/// A project with a synced home on each of the stations `0..n`: the home of
/// station 0 is the rig home, the others are clones that "add home" makes.
/// Returns the project id and the folder of each home.
pub fn project_with_homes(rig: &Rig, n: usize, settings: Value) -> (String, Vec<PathBuf>) {
    let project = rig.project(0, "App", settings);
    let mut folders = vec![rig.stations[0].home.clone()];
    for i in 1..n {
        let r = rig.add_home(&project, i);
        assert_eq!(r.status, 202, "{}", r.body);
        let home = rig.wait_home_on(&project, i, "ready");
        folders.push(PathBuf::from(home["path"].as_str().unwrap()));
    }
    (project, folders)
}

/// The Git text of a branch row as the coordinator gets it: the `subject`
/// inside the untrusted envelope of the project and the branch name.
#[must_use]
pub fn fenced_subject(project: &str, name: &str, subject: &str) -> String {
    fence(&format!("git:{project}/{name}"), subject)
}
