//! M3 item 7-4 (PLAN §13 "Performance bars", Phase 7; DL-401): the
//! reproducible performance harness behind `make perf`.
//!
//! One deck and ten stations run as separate `fleetdeck` processes on
//! loopback (TLS links, separate data directories), with the offline
//! `fake-claude sim` agent. The harness measures:
//!
//! - station overhead: the station's resident memory (VmRSS) idle, and with
//!   50 live sessions (agent processes excluded: they are other processes);
//! - 50 sessions per station: 50 live, idle sessions on one station;
//! - 10 stations per deck: ten linked stations whose sessions reach the deck;
//! - deck read-model lag: from the station's receipt of a session start to
//!   the card in the board API `GET /api/sessions`;
//! - approval round trip: from the board's `POST /api/commands/approval.answer`
//!   to the answer line on the agent's stdin.
//!
//! The test is `#[ignore]`: `make perf` runs it in the release profile and
//! writes `target/perf/report.md`. It fails when a bar fails. Loopback is not
//! a LAN: the LAN approval round trip is an owner step (DL-401).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use common::deck::Deck;
use common::{Station, WAIT};
use serde_json::{Value, json};

const SESSIONS: usize = 50;
const STATIONS: usize = 10;
const APPROVALS: usize = 20;

fn rss_kib(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap()
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

/// The p-th percentile (nearest rank) of samples in milliseconds.
fn percentile(samples: &[f64], p: f64) -> f64 {
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    s[rank - 1]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A logged-in board client of the deck (cookie and CSRF token).
struct Board {
    addr: std::net::SocketAddr,
    cookie: String,
    csrf: String,
}

impl Board {
    fn login(deck: &Deck) -> Self {
        let (addr, token) = deck.board();
        let r = deck.https(
            addr,
            "POST",
            "/api/auth/link",
            &[],
            Some(json!({ "token": token })),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let line = r
            .head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
            .unwrap();
        let cookie = line
            .split_once(':')
            .unwrap()
            .1
            .trim()
            .split(';')
            .next()
            .unwrap();
        Self {
            addr,
            cookie: cookie.to_owned(),
            csrf: r.body["csrf"].as_str().unwrap().to_owned(),
        }
    }

    fn call(
        &self,
        deck: &Deck,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> common::deck::Reply {
        let auth = [
            ("Cookie", self.cookie.as_str()),
            ("X-CSRF-Token", self.csrf.as_str()),
        ];
        deck.https(self.addr, method, path, &auth, body)
    }

    /// The session ids of the board's cards.
    fn session_ids(&self, deck: &Deck) -> Vec<String> {
        let r = self.call(deck, "GET", "/api/sessions", None);
        assert_eq!(r.status, 200, "{}", r.body);
        let cards = r
            .body
            .as_array()
            .cloned()
            .or_else(|| r.body["sessions"].as_array().cloned());
        cards
            .unwrap_or_default()
            .iter()
            .filter_map(|c| c["summary"]["session_id"].as_str().map(str::to_owned))
            .collect()
    }
}

/// A station with room for the 50-session bar, paired by dialing the deck.
fn joined_station(deck: &Deck, name: &str) -> Station {
    let mut station = Station::named(name);
    let path = station.node.dirs().station_config();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("max_sessions = 4"));
    station
        .node
        .write_station_toml(&text.replace("max_sessions = 4", "max_sessions = 128"))
        .unwrap();
    station.spawn(None);
    let invitation = deck.ok(&["deck", "invite"]);
    station.ok_stdin(&["station", "join", "--stdin"], invitation.trim());
    station
}

/// Starts a session on `station` and returns its id and the read-model lag:
/// the time from the station's receipt to the card on the board API.
fn start_and_lag(station: &Station, deck: &Deck, board: &Board, prompt: &str) -> (String, f64) {
    let id = station.run(prompt);
    let t0 = Instant::now();
    loop {
        if board.session_ids(deck).contains(&id) {
            return (id, ms(t0.elapsed()));
        }
        assert!(t0.elapsed() < WAIT, "the deck did not show {id}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn host_line() -> String {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let cpu = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .map(|v| v.trim_start_matches([' ', '\t', ':']).to_owned())
        .unwrap_or_else(|| "unknown".into());
    let threads = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mem = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or("unknown".into(), |k| {
            format!("{:.1} GiB", k as f64 / 1024.0 / 1024.0)
        });
    let os = std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map_or("unknown".into(), |v| v.trim_matches('"').to_owned());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    format!(
        "{cpu}, {threads} threads, {mem} RAM; {os}, Linux {}",
        kernel.trim()
    )
}

struct Bar {
    name: &'static str,
    target: String,
    measured: String,
    pass: bool,
}

#[test]
#[ignore = "performance harness: run with `make perf` (release profile)"]
fn performance_bars_of_plan_section_13() {
    let started = Instant::now();
    let deck = Deck::start();
    let board = Board::login(&deck);
    let mut bars = Vec::new();
    let mut lags = Vec::new();

    // 1. Station overhead idle, then 50 live sessions on one station.
    let first = joined_station(&deck, "perf-00");
    std::thread::sleep(Duration::from_secs(1));
    let idle = rss_kib(first.pid());
    let mut ids = Vec::new();
    for n in 0..SESSIONS {
        let (id, lag) = start_and_lag(&first, &deck, &board, &format!("hello {n}"));
        ids.push(id);
        lags.push(lag);
    }
    for id in &ids {
        first.wait_for("an idle session", id, |s| s.current_turn.is_none());
    }
    std::thread::sleep(Duration::from_secs(1));
    let live = first.stdin().pids.len();
    let loaded = rss_kib(first.pid());
    let per_session = loaded.saturating_sub(idle) as f64 / SESSIONS as f64;
    bars.push(Bar {
        name: "Station overhead, idle (VmRSS)",
        target: "< 30 MB".into(),
        measured: format!("{:.1} MiB", mib(idle)),
        pass: mib(idle) < 30.0,
    });
    bars.push(Bar {
        name: "Station overhead per session (VmRSS, 50 sessions)",
        target: "< 5 MB".into(),
        measured: format!(
            "{:.2} MiB ({:.1} MiB total)",
            per_session / 1024.0,
            mib(loaded)
        ),
        pass: per_session / 1024.0 < 5.0,
    });
    bars.push(Bar {
        name: "Sessions per station",
        target: format!("{SESSIONS} live and idle"),
        measured: format!("{} started, {live} agent processes", ids.len()),
        pass: ids.len() == SESSIONS && live == SESSIONS,
    });

    // 2. Approval round trip board → agent. One session, one board follow-up
    // per sample: the Claude adapter numbers its request ids per session, and
    // the station keys them per station (DL-402), so samples on fresh
    // sessions would collide.
    let mut round_trips = Vec::new();
    let asker = first.run("ask ls 0");
    let mut seen = Vec::new();
    for n in 0..APPROVALS {
        if n > 0 {
            let body = json!({
                "station": "perf-00",
                "command_id": format!("perf-follow-{n}"),
                "command": { "op": "follow_up", "session_id": asker,
                    "input": { "text": format!("ask ls {n}") } }
            });
            let r = board.call(&deck, "POST", "/api/commands/session.follow_up", Some(body));
            assert_eq!(r.status, 200, "{}", r.body);
            assert_eq!(r.body["receipt"]["state"], "accepted", "{}", r.body);
        }
        let asking = first.wait_for("the approval", &asker, |s| {
            s.open_requests
                .iter()
                .any(|r| !seen.contains(&r.request_id.as_str().to_owned()))
        });
        let request = asking.open_requests[0].request_id.as_str().to_owned();
        seen.push(request.clone());
        let answered = |s: &Station| s.stdin().count("\"behavior\":\"allow\"");
        let before = answered(&first);
        let body = json!({
            "station": "perf-00",
            "command_id": format!("perf-answer-{n}"),
            "command": { "op": "answer", "session_id": asker, "request_id": request,
                "decision": { "option_id": "allow", "updated_input": null, "text": null } }
        });
        let t0 = Instant::now();
        let r = board.call(&deck, "POST", "/api/commands/approval.answer", Some(body));
        assert_eq!(r.status, 200, "{}", r.body);
        while answered(&first) == before {
            assert!(t0.elapsed() < WAIT, "the agent got no answer: {}", r.body);
            std::thread::sleep(Duration::from_millis(1));
        }
        round_trips.push(ms(t0.elapsed()));
        first.wait_for("the answered turn", &asker, |s| {
            s.open_requests.is_empty() && s.current_turn.is_none()
        });
    }
    let rt_p50 = percentile(&round_trips, 50.0);
    let rt_p95 = percentile(&round_trips, 95.0);
    bars.push(Bar {
        name: "Approval round trip board → agent (loopback, p95)",
        target: "< 300 ms (LAN)".into(),
        measured: format!(
            "p50 {rt_p50:.1} ms, p95 {rt_p95:.1} ms, max {:.1} ms",
            percentile(&round_trips, 100.0)
        ),
        pass: rt_p95 < 300.0,
    });

    // 3. Ten stations per deck; each starts sessions while all are linked.
    let mut stations = vec![first];
    for n in 1..STATIONS {
        stations.push(joined_station(&deck, &format!("perf-{n:02}")));
    }
    for round in 0..3 {
        for st in &stations[1..] {
            let (_, lag) = start_and_lag(st, &deck, &board, &format!("hello round {round}"));
            lags.push(lag);
        }
    }
    let views = deck.stations();
    let reachable = views.iter().filter(|v| v.reachable).count();
    bars.push(Bar {
        name: "Stations per deck",
        target: format!("{STATIONS} linked, sessions on the board"),
        measured: format!("{} paired, {reachable} reachable", views.len()),
        pass: views.len() == STATIONS && reachable == STATIONS,
    });
    let lag_p50 = percentile(&lags, 50.0);
    let lag_p95 = percentile(&lags, 95.0);
    bars.push(Bar {
        name: "Deck read-model lag (p95)",
        target: "< 500 ms".into(),
        measured: format!(
            "p50 {lag_p50:.1} ms, p95 {lag_p95:.1} ms, max {:.1} ms",
            percentile(&lags, 100.0)
        ),
        pass: lag_p95 < 500.0,
    });

    // The report.
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut report = String::new();
    writeln!(report, "# Fleet Deck v2 performance report (`make perf`)\n").unwrap();
    writeln!(report, "- Host: {}", host_line()).unwrap();
    writeln!(report, "- Build profile: {profile}; transport: loopback 127.0.0.1, TLS links, HTTPS board API (a new TLS connection per request)").unwrap();
    writeln!(report, "- Agents: `fake-claude sim` (offline); one deck and {STATIONS} stations as separate processes").unwrap();
    writeln!(
        report,
        "- Samples: read-model lag {}, approval round trip {APPROVALS}; run time {:.0} s\n",
        lags.len(),
        started.elapsed().as_secs_f64()
    )
    .unwrap();
    writeln!(report, "| Bar (PLAN §13) | Target | Measured | Result |").unwrap();
    writeln!(report, "| --- | --- | --- | --- |").unwrap();
    for b in &bars {
        let result = if b.pass { "pass" } else { "FAIL" };
        writeln!(
            report,
            "| {} | {} | {} | {result} |",
            b.name, b.target, b.measured
        )
        .unwrap();
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/perf");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.md"), &report).unwrap();
    println!("{report}");
    let failed: Vec<&str> = bars.iter().filter(|b| !b.pass).map(|b| b.name).collect();
    assert!(failed.is_empty(), "failed bars: {failed:?}\n{report}");
}
