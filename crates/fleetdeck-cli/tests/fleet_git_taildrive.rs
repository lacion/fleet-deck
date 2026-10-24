//! Fleet Git FG-12 on loopback (T2.5, design §15.8, owner acceptance A2): the
//! Taildrive artifact share. Each station has a fake `tailscale` program (a
//! script below its HOME, set with `tailscale_bin`) that prints a fixed
//! `status --json` and the `drive list` table of `shares.tsv`, and logs every
//! argv. The provider is a script that writes its environment to
//! `$HOME/provider-env.txt`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::WAIT;
use common::fleet_git::{Options, Rig, StationSetup};
use fleetdeck_core::fleet_git_digest::home_folder_name;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::Event;
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

/// The fake `tailscale`: `status --json` prints `status.json`; `drive list`
/// prints `drive.raw` as it is, else the table of `shares.tsv` (name, path
/// and user, separated by tabs); another call fails.
const TAILSCALE: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/argv.log"
case "$1" in
status) cat "$d/status.json" ;;
drive)
  if [ -f "$d/drive.raw" ]; then cat "$d/drive.raw"; exit 0; fi
  [ -f "$d/shares.tsv" ] || exit 1
  awk -F'\t' '
    BEGIN { a = 4; b = 4; c = 2 }
    { n[NR] = $1; p[NR] = $2; u[NR] = $3
      if (length($1) > a) a = length($1)
      if (length($2) > b) b = length($2)
      if (length($3) > c) c = length($3) }
    END {
      printf "%-*s    %-*s    as\n", a, "name", b, "path"
      da = sprintf("%*s", a, ""); gsub(/ /, "-", da)
      db = sprintf("%*s", b, ""); gsub(/ /, "-", db)
      dc = sprintf("%*s", c, ""); gsub(/ /, "-", dc)
      printf "%s    %s    %s\n", da, db, dc
      for (i = 1; i <= NR; i++) printf "%-*s    %-*s    %s\n", a, n[i], b, p[i], u[i]
    }' "$d/shares.tsv" ;;
*) exit 2 ;;
esac
"#;

/// The provider: it writes its environment (not for a probe) and runs the
/// fake Claude.
fn provider(dir: &Path) -> PathBuf {
    let fake = FakeAgent::Claude.locate().unwrap();
    let script = dir.join("provider.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1\" in auth|--version) ;; *) env > \"$HOME/provider-env.txt\" ;; esac\nexec '{}' \"$@\"\n",
            fake.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn status(machine: &str) -> String {
    format!(
        r#"{{"BackendState":"Running","Self":{{"HostName":"{machine}","DNSName":"{machine}.tail1234.ts.net.","UserID":1,"TailscaleIPs":["100.64.0.10"]}},"CurrentTailnet":{{"Name":"tail1234.ts.net","MagicDNSSuffix":"tail1234.ts.net","MagicDNSEnabled":true}},"Peer":{{}},"User":{{"1":{{"LoginName":"owner@example.com"}}}}}}"#
    )
}

/// A station with the fake `tailscale` of `machine`. `shares` are the lines
/// `name<TAB>path` of `shares.tsv` (`{HOME}` is the station HOME); `extra`
/// are more files.
fn station(machine: &str, shares: &[(&str, &str)], toml: &str) -> StationSetup {
    let tsv: String = shares
        .iter()
        .map(|(name, path)| format!("{name}\t{path}\towner\n"))
        .collect();
    let mut files = vec![
        ("ts/tailscale".to_owned(), TAILSCALE.as_bytes().to_vec()),
        ("ts/status.json".to_owned(), status(machine).into_bytes()),
        ("share/out/note.txt".to_owned(), b"note\n".to_vec()),
        (".claude/settings.json".to_owned(), b"{}\n".to_vec()),
        (".ssh/known_hosts".to_owned(), b"\n".to_vec()),
        ("mnt/art/.keep".to_owned(), b"\n".to_vec()),
    ];
    if !shares.is_empty() {
        files.push(("ts/shares.tsv".to_owned(), tsv.into_bytes()));
    }
    StationSetup {
        toml: format!("tailscale_bin = \"{{HOME}}/ts/tailscale\"\n{toml}"),
        files,
        executables: vec!["ts/tailscale".to_owned()],
        ..StationSetup::default()
    }
}

fn rig_of(dir: &Path, stations: &[(&str, StationSetup)]) -> Rig {
    let names: Vec<&str> = stations.iter().map(|(n, _)| *n).collect();
    Rig::with(
        &names,
        Options {
            provider: Some(provider(dir)),
            setup: stations
                .iter()
                .map(|(n, s)| ((*n).to_owned(), s.clone()))
                .collect::<BTreeMap<_, _>>(),
            ..Options::default()
        },
    )
}

fn home_of(rig: &Rig, i: usize) -> PathBuf {
    rig.stations[i].station.node.home().canonicalize().unwrap()
}

/// The Taildrive report of station `i` as JSON, once `f` holds for it.
fn wait_report(rig: &Rig, i: usize, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    let info = rig.wait_info(what, i, |info| {
        info.taildrive
            .as_ref()
            .is_some_and(|t| f(&serde_json::to_value(t).unwrap()))
    });
    serde_json::to_value(info.taildrive.unwrap()).unwrap()
}

/// Waits until the report of station `i` lists a share `name` that reads
/// `exposure` (`clear`, `exposed`, ...).
fn wait_share(rig: &Rig, i: usize, name: &str, exposure: &str) -> Value {
    let report = wait_report(rig, i, &format!("the share {name} to be {exposure}"), |t| {
        t["shares"].as_array().is_some_and(|s| {
            s.iter()
                .any(|s| s["name"] == name && s["exposure_state"] == exposure)
        })
    });
    report["shares"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap()
        .clone()
}

/// The `argv.log` lines of the fake `tailscale` of station `i`.
fn argv(rig: &Rig, i: usize) -> Vec<String> {
    std::fs::read_to_string(home_of(rig, i).join("ts/argv.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn write_ts(rig: &Rig, i: usize, name: &str, text: &str) {
    std::fs::write(home_of(rig, i).join("ts").join(name), text).unwrap();
}

/// Saves the artifact share of the project (sync stays on).
fn save_share(rig: &Rig, project: &str, share: &str) -> common::deck::Reply {
    rig.save_git(project, json!({ "sync": true, "artifact_share": share }))
}

/// The environment lines that the provider of station `i` saw at its next
/// session start; the old dump is removed first.
fn clear_env(rig: &Rig, i: usize) {
    let _ = std::fs::remove_file(home_of(rig, i).join("provider-env.txt"));
}

fn provider_env(rig: &Rig, i: usize) -> Vec<String> {
    let file = home_of(rig, i).join("provider-env.txt");
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(&file)
            && !text.is_empty()
        {
            return text.lines().map(str::to_owned).collect();
        }
        assert!(Instant::now() < deadline, "the provider did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn var<'a>(env: &'a [String], name: &str) -> Option<&'a str> {
    env.iter().find_map(|l| l.strip_prefix(&format!("{name}=")))
}

/// A started session once its artifact folder is ready.
fn start_with_artifacts(rig: &Rig, i: usize) -> SessionSummary {
    let id = rig.start(i).session_id.as_str().to_owned();
    rig.wait_summary("the artifact folder", i, &id, |s| s.artifacts_dir.is_some())
}

#[test]
fn shares_are_reported_and_only_status_and_drive_list_run() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig_of(
        dir.path(),
        &[(
            "alpha",
            station("alpha-m", &[("out", "{HOME}/share/out")], ""),
        )],
    );
    let report = wait_report(&rig, 0, "the report", |t| t["state"] == "ok");
    assert_eq!(report["machine"], "alpha-m");
    let shares = report["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 1, "{report}");
    assert_eq!(shares[0]["name"], "out");
    assert_eq!(
        shares[0]["path"],
        format!("{}/share/out", home_of(&rig, 0).display())
    );
    assert_eq!(shares[0]["path_state"], "ok");
    // The only calls: `status --json` and `drive list`.
    let log = argv(&rig, 0);
    assert!(log.iter().any(|l| l == "status --json"), "{log:?}");
    assert!(log.iter().any(|l| l == "drive list"), "{log:?}");
    for line in &log {
        assert!(
            line == "status --json" || line == "drive list",
            "an unexpected tailscale call: {line}"
        );
    }
}

#[test]
fn unreadable_output_gives_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mut setup = station("alpha-m", &[], "");
    setup
        .files
        .push(("ts/drive.raw".to_owned(), b"not a table\n".to_vec()));
    let rig = rig_of(dir.path(), &[("alpha", setup)]);
    let report = wait_report(&rig, 0, "the report", |t| t["state"] == "unknown");
    assert_eq!(report["reason"], "Fleet Deck cannot read the share list");
    assert_eq!(report["shares"].as_array().unwrap().len(), 0);
}

#[test]
fn a_missing_program_gives_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let mut setup = station("alpha-m", &[], "");
    setup.toml = "tailscale_bin = \"{HOME}/ts/absent\"\n".to_owned();
    let rig = rig_of(dir.path(), &[("alpha", setup)]);
    let report = wait_report(&rig, 0, "the report", |t| t["state"] == "unavailable");
    assert_eq!(report["reason"], "tailscale is not installed");
}

#[test]
fn shares_over_a_home_the_claude_home_and_ssh_are_exposed_and_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut rig = rig_of(dir.path(), &[("alpha", station("alpha-m", &[], ""))]);
    let id = rig.project(0, "App", json!({}));
    let home = home_of(&rig, 0);
    let cases = [
        (
            "proj",
            home.join("projects/app"),
            "project_home",
            "a project home",
        ),
        (
            "claude",
            home.join(".claude"),
            "claude_home",
            "the Claude home",
        ),
        ("ssh", home.join(".ssh"), "ssh", "~/.ssh"),
    ];
    for (name, path, kind, what) in cases {
        // A restart makes a report that knows the homes of the stored
        // snapshot, and the deck asks `station.info` at the new link.
        write_ts(
            &rig,
            0,
            "shares.tsv",
            &format!("{name}\t{}\towner\n", path.display()),
        );
        rig.restart_station(0, &[]);
        let share = wait_share(&rig, 0, name, "exposed");
        // (`~/.ssh` is also a member-hidden path, so a kind can come with it.)
        assert!(
            share["exposed"].as_array().unwrap().contains(&json!(kind)),
            "{share}"
        );
        // The deck refuses it as the artifact share of the project.
        let r = save_share(&rig, &id, &format!("alpha-m/{name}"));
        assert_eq!(r.status, 400, "{}", r.body);
        assert_eq!(r.body["error"], "share_exposed");
        assert_eq!(r.body["field"], "artifact_share");
        let message = r.body["message"].as_str().unwrap();
        assert!(
            message.starts_with("This share exposes ") && message.contains(what),
            "{message}"
        );
        assert!(message.ends_with(". Pick another share."), "{message}");
    }
    assert!(rig.project_of(&id)["git"]["artifact_share"].is_null());
}

/// A rig with one station `alpha-m` and a synced project `App` whose share
/// `out` is clear in the report that the deck holds.
fn local_rig(dir: &Path, toml: &str, shares: &[(&str, &str)]) -> (Rig, String) {
    let mut rig = rig_of(dir, &[("alpha", station("alpha-m", shares, toml))]);
    let id = rig.project(0, "App", json!({}));
    rig.restart_station(0, &[]);
    (rig, id)
}

fn mode_of(path: &str) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_local_share_sets_the_variable_and_makes_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (rig, id) = local_rig(dir.path(), "", &[("out", "{HOME}/share/out")]);
    wait_share(&rig, 0, "out", "clear");
    let r = save_share(&rig, &id, "alpha-m/out");
    assert_eq!(r.status, 200, "{}", r.body);
    clear_env(&rig, 0);
    let s = start_with_artifacts(&rig, 0);
    let want = format!(
        "{}/share/out/{}/{}/",
        home_of(&rig, 0).display(),
        home_folder_name("App"),
        s.callsign.clone().unwrap()
    );
    let dir = s.artifacts_dir.clone().unwrap();
    assert_eq!(dir.path.as_deref(), Some(want.as_str()));
    assert!(dir.url.is_none());
    assert_eq!(mode_of(&want), 0o700, "the session folder");
    let env = provider_env(&rig, 0);
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS"), Some(want.as_str()));
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS_URL"), None);
}

#[test]
fn a_configured_mount_sets_the_variable_and_makes_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let toml = "taildrive_mounts = { \"nas1/art\" = \"{HOME}/mnt/art\" }\n";
    let (rig, id) = local_rig(dir.path(), toml, &[]);
    // The machine `nas1` is no station: only the mount checks apply.
    let r = save_share(&rig, &id, "nas1/art");
    assert_eq!(r.status, 200, "{}", r.body);
    clear_env(&rig, 0);
    let s = start_with_artifacts(&rig, 0);
    let want = format!(
        "{}/mnt/art/{}/{}/",
        home_of(&rig, 0).display(),
        home_folder_name("App"),
        s.callsign.clone().unwrap()
    );
    assert_eq!(
        s.artifacts_dir.unwrap().path.as_deref(),
        Some(want.as_str())
    );
    assert_eq!(mode_of(&want), 0o700);
    assert_eq!(
        var(&provider_env(&rig, 0), "FLEETDECK_ARTIFACTS"),
        Some(want.as_str())
    );
}

#[test]
fn without_a_local_path_the_url_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let (rig, id) = local_rig(dir.path(), "", &[]);
    let r = save_share(&rig, &id, "nas1/out");
    assert_eq!(r.status, 200, "{}", r.body);
    clear_env(&rig, 0);
    let s = start_with_artifacts(&rig, 0);
    let want = format!(
        "http://100.100.100.100:8080/tail1234.ts.net/nas1/out/{}/{}/",
        home_folder_name("App"),
        s.callsign.clone().unwrap()
    );
    let shown = s.artifacts_dir.unwrap();
    assert_eq!(shown.url.as_deref(), Some(want.as_str()));
    assert!(shown.path.is_none());
    let env = provider_env(&rig, 0);
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS_URL"), Some(want.as_str()));
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS"), None);
}

/// The `artifacts.unavailable` events of the stopped station `i`: the codes.
async fn unavailable_codes(rig: &mut Rig, i: usize) -> Vec<String> {
    rig.events_after_stop(i)
        .await
        .into_iter()
        .filter_map(|(_, e)| match e {
            Event::ArtifactsUnavailable { code, .. } => Some(code),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_missing_mount_emits_unavailable_and_the_session_still_starts() {
    let dir = tempfile::tempdir().unwrap();
    let toml = "taildrive_mounts = { \"nas1/art\" = \"{HOME}/mnt/gone\" }\n";
    let (mut rig, id) = local_rig(dir.path(), toml, &[]);
    let r = save_share(&rig, &id, "nas1/art");
    assert_eq!(r.status, 200, "{}", r.body);
    clear_env(&rig, 0);
    let started = rig.start(0);
    let sid = started.session_id.as_str().to_owned();
    // The session runs a turn, with no artifact variable.
    rig.wait_summary("the first turn", 0, &sid, |s| {
        s.current_turn.is_none() && s.usage.input_tokens.is_some()
    });
    let env = provider_env(&rig, 0);
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS"), None);
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS_URL"), None);
    assert!(rig.summary(0, &sid).artifacts_dir.is_none());
    assert_eq!(unavailable_codes(&mut rig, 0).await, ["mount_missing"]);
}

/// Two stations: `alpha` has the project and no local path; `bravo` exports
/// the machine `bravo-m` with the share `out`.
fn remote_rig(dir: &Path) -> (Rig, String) {
    let mut rig = rig_of(
        dir,
        &[
            ("alpha", station("alpha-m", &[], "")),
            (
                "bravo",
                station("bravo-m", &[("out", "{HOME}/share/out")], ""),
            ),
        ],
    );
    let id = rig.project(0, "App", json!({}));
    // The restart makes bravo report the share with the stored snapshot.
    rig.restart_station(1, &[]);
    wait_share(&rig, 1, "out", "clear");
    (rig, id)
}

#[tokio::test]
async fn the_remote_authorization_through_the_deck_gives_ok_and_exposed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut rig, id) = remote_rig(dir.path());
    let r = save_share(&rig, &id, "bravo-m/out");
    assert_eq!(r.status, 200, "{}", r.body);

    // The exporting station says `ok`: the session gets the address.
    clear_env(&rig, 0);
    let s = start_with_artifacts(&rig, 0);
    let want = format!(
        "http://100.100.100.100:8080/tail1234.ts.net/bravo-m/out/{}/{}/",
        home_folder_name("App"),
        s.callsign.clone().unwrap()
    );
    assert_eq!(s.artifacts_dir.unwrap().url.as_deref(), Some(want.as_str()));
    assert_eq!(
        var(&provider_env(&rig, 0), "FLEETDECK_ARTIFACTS_URL"),
        Some(want.as_str())
    );

    // A protected file in the share (a member-hidden `.env`): after bravo
    // makes its report again, it says `exposed`, and the next session of
    // alpha starts without the variables.
    std::fs::write(home_of(&rig, 1).join("share/out/.env"), "SECRET=1\n").unwrap();
    rig.restart_station(1, &[]);
    let share = wait_share(&rig, 1, "out", "exposed");
    assert_eq!(share["exposed"], json!(["member_deny"]), "{share}");
    clear_env(&rig, 0);
    let second = rig.start(0);
    let sid = second.session_id.as_str().to_owned();
    rig.wait_summary("the first turn", 0, &sid, |s| {
        s.current_turn.is_none() && s.usage.input_tokens.is_some()
    });
    let env = provider_env(&rig, 0);
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS_URL"), None);
    assert_eq!(var(&env, "FLEETDECK_ARTIFACTS"), None);
    assert!(rig.summary(0, &sid).artifacts_dir.is_none());
    assert_eq!(unavailable_codes(&mut rig, 0).await, ["share_exposed"]);
}
