//! `fleetdeck desktop crash --last` (R13 P4 task H, `omarchy-p4.md` §3.9):
//! starts one supervised session that diagnoses the newest crash that
//! `systemd-coredump` recorded.
//!
//! 1. `coredumpctl list --json=short --no-pager` gives the crashes; the
//!    entry with the largest `time` is the newest.
//! 2. The facts (process, PID, binary, signal, time) lose control and
//!    bidirectional characters and are cut at 200 characters.
//! 3. The fixed prompt of `bin/omarchy-agent-crash` is filled in.
//! 4. The session starts through the station local socket, as `station run`
//!    does: the first provider instance, the canonical `$HOME`, no mode.
//! 5. The session id is printed, and one notification is sent. A failed
//!    notification is a warning.
//!
//! Every exit-1 text is printed on stderr as `fleetdeck: <text>`.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::bail;
use fleetdeck_core::ChildEnv;
use fleetdeck_core::config::StationConfig;
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_proto::command::{ReceiptState, RejectCode, SessionCommand, SessionSpec, UserInput};
use fleetdeck_station::desktop::detect::omarchy_root;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use serde_json::Value;

use crate::omarchy::{omarchy_env, tool};

/// The time limit of `coredumpctl`.
const LIMIT: Duration = Duration::from_secs(10);
/// The longest text of one fact, in characters.
const MAX_TEXT: usize = 200;
/// The program that shows a notification.
const SEND: &str = "omarchy-notification-send";

const NO_COREDUMPCTL: &str = "coredumpctl is not available.";
const NO_CRASH: &str = "No crash was found.";
const NO_OMARCHY: &str = "Omarchy was not found on this machine.";
const NO_HOME: &str = "HOME is not set or is not a directory.";
const HOME_NOT_ALLOWED: &str = "Add your home directory to allowed_roots to diagnose a crash.";
const NO_PROVIDER: &str = "No provider is configured on this station.";
const NO_STATION: &str = "The local station is not running.";
const NOT_SENT: &str = "fleetdeck: the notification was not sent.";

/// The facts of one crash, already cleaned.
#[derive(Debug, PartialEq, Eq)]
struct Facts {
    comm: String,
    pid: String,
    exe: String,
    sig: String,
    time: String,
}

/// Removes control and bidirectional characters, then cuts at
/// [`MAX_TEXT`] characters.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
                )
        })
        .take(MAX_TEXT)
        .collect()
}

/// A cleaned text, or `unknown` when nothing is left.
fn or_unknown(text: &str) -> String {
    let text = clean(text);
    if text.is_empty() {
        "unknown".to_owned()
    } else {
        text
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC` for a time in microseconds since the epoch.
fn format_time(micros: u64) -> String {
    let secs = micros / 1_000_000;
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let rest = secs % 86_400;
    // The civil date of a day number (days since 1970-01-01).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// A JSON integer or a text of digits, as digits.
fn digits(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Number(n) => n.as_u64().map(|n| n.to_string()),
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            Some(s.clone())
        }
        _ => None,
    }
}

/// A JSON number or text, as text.
fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// The facts of the newest entry of the `coredumpctl` JSON.
///
/// # Errors
/// The text is not a JSON array of objects (`coredumpctl is not
/// available.`), no entry has a numeric `time`, or the `pid` of the newest
/// entry is not digits (`No crash was found.`).
fn facts(json: &str) -> anyhow::Result<Facts> {
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(json) else {
        bail!(NO_COREDUMPCTL);
    };
    if !entries.iter().all(Value::is_object) {
        bail!(NO_COREDUMPCTL);
    }
    let newest = entries
        .iter()
        .filter_map(|entry| Some((entry.get("time")?.as_u64()?, entry)))
        .max_by_key(|(time, _)| *time);
    let Some((time, entry)) = newest else {
        bail!(NO_CRASH);
    };
    let Some(pid) = digits(entry.get("pid")) else {
        bail!(NO_CRASH);
    };
    let exe_raw = entry.get("exe").and_then(Value::as_str).unwrap_or_default();
    let comm = match entry.get("comm").and_then(Value::as_str).map(clean) {
        Some(comm) if !comm.is_empty() => comm,
        _ => or_unknown(exe_raw.rsplit('/').next().unwrap_or_default()),
    };
    Ok(Facts {
        comm,
        pid,
        exe: or_unknown(exe_raw),
        sig: or_unknown(&text(entry.get("sig")).unwrap_or_default()),
        time: format_time(time),
    })
}

/// The prompt of `bin/omarchy-agent-crash` with the values filled in.
fn prompt(facts: &Facts, omarchy: &Path) -> String {
    format!(
        "A process crashed on this Omarchy machine and I want to know why.\n\
         \n\
         What systemd-coredump recorded:\n\
         \x20 process:  {}\n\
         \x20 PID:      {}\n\
         \x20 binary:   {}\n\
         \x20 signal:   {}\n\
         \x20 time:     {}\n\
         \n\
         Use the diagnose-crash skill: it covers how to investigate, what to report, and\n\
         when a crash is worth reporting upstream to Omarchy. If your harness has no skill\n\
         mechanism, read the skill files directly and follow them instead:\n\
         \n\
         \x20 {}/default/agents/skills/diagnose-crash/SKILL.md\n",
        facts.comm,
        facts.pid,
        facts.exe,
        facts.sig,
        facts.time,
        omarchy.display()
    )
}

/// Runs `coredumpctl` on the `PATH` of `env`.
async fn coredumpctl(env: &ChildEnv, limit: Duration) -> anyhow::Result<String> {
    let args = ["list", "--json=short", "--no-pager"];
    match tool(env, "coredumpctl", &args, limit).await {
        Ok(out) if out.code == 0 => Ok(out.stdout),
        Ok(_) | Err(_) => bail!(NO_COREDUMPCTL),
    }
}

/// The canonical `$HOME` as text.
fn canonical_home(env: &Env) -> anyhow::Result<String> {
    let Some(home) = &env.home else {
        bail!(NO_HOME);
    };
    match std::fs::canonicalize(home).map(|p| p.to_str().map(str::to_owned)) {
        Ok(Some(home)) => Ok(home),
        _ => bail!(NO_HOME),
    }
}

/// The serde name of a unit enum value (`forbidden`).
fn word<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => "unknown".to_owned(),
    }
}

/// The start request of the session: the first provider instance, `cwd`,
/// no mode, the prompt.
fn start_request(
    config: &StationConfig,
    cwd: String,
    prompt: String,
) -> anyhow::Result<LocalRequest> {
    let Some(instance) = config.provider_instances.first() else {
        bail!(NO_PROVIDER);
    };
    Ok(LocalRequest::Command {
        command_id: None,
        command: SessionCommand::Start {
            spec: SessionSpec {
                effort: None,
                provider_instance: instance.id.clone(),
                cwd,
                mode: None,
                model: None,
                title: None,
                coordinator: None,
                launch: None,
                extra_env: None,
            },
            prompt: Some(UserInput::new(prompt)),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        },
    })
}

/// Sends the start request to the local station; returns the session id.
async fn start(dirs: &NodeDirs, request: &LocalRequest) -> anyhow::Result<String> {
    let path = socket_path(&dirs.station());
    let Ok(mut client) = LocalClient::connect(&path).await else {
        bail!(NO_STATION);
    };
    let Ok(answer) = client.call(request).await else {
        bail!(NO_STATION);
    };
    match answer {
        LocalResponse::Receipt { receipt } if receipt.state == ReceiptState::Rejected => {
            let (code, message) = receipt.reject.map_or_else(
                || (word(&RejectCode::Unknown), String::new()),
                |r| (word(&r.code), r.message),
            );
            bail!("{code}: {message}")
        }
        LocalResponse::Receipt { receipt } => match receipt.result.and_then(|r| r.session_id) {
            Some(session) => Ok(session.to_string()),
            None => bail!("The station accepted the start but sent no session id."),
        },
        LocalResponse::Error { message } => bail!("The station refused the request: {message}"),
        _ => bail!("The station gave an answer that this program does not know."),
    }
}

/// The argv of the notification (without the program name).
fn notify_argv(exe: &Path) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["--app-name", "Fleet Deck", "-u", "normal"]
        .into_iter()
        .map(OsString::from)
        .collect();
    argv.push("Fleet Deck".into());
    argv.push("A crash diagnosis session started".into());
    argv.extend(["--exec".into(), exe.into(), "desktop".into(), "open".into()]);
    argv
}

/// Starts the notification detached (its own process group, no standard
/// streams). A failure is a warning on stderr.
fn notify() {
    let sent = std::env::current_exe().ok().and_then(|exe| {
        Command::new(SEND)
            .args(notify_argv(&exe))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok()
    });
    if sent.is_none() {
        eprintln!("{NOT_SENT}");
    }
}

/// `fleetdeck desktop crash --last`.
pub async fn run(dirs: &NodeDirs) -> anyhow::Result<()> {
    let omarchy_env = omarchy_env();
    let list = coredumpctl(&omarchy_env, LIMIT).await?;
    let facts = facts(&list)?;
    let Some((root, _)) = omarchy_root(&omarchy_env) else {
        bail!(NO_OMARCHY);
    };
    let prompt = prompt(&facts, &root);
    let env = Env::from_process();
    let home = canonical_home(&env)?;
    let config = StationConfig::load_with_store(
        &dirs.station_config(),
        &env,
        &dirs.station().join("secrets"),
    )?;
    if !config.is_allowed(Path::new(&home)) {
        bail!(HOME_NOT_ALLOWED);
    }
    let request = start_request(&config, home, prompt)?;
    let session = start(dirs, &request).await?;
    println!("{session}");
    notify();
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn entry(time: u64, pid: &str, exe: &str) -> Value {
        serde_json::json!({"time": time, "pid": pid.parse::<u64>().unwrap_or(0), "sig": 11, "exe": exe})
    }

    fn list(entries: &[Value]) -> String {
        Value::Array(entries.to_vec()).to_string()
    }

    fn text_of(result: anyhow::Result<Facts>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn crash_picks_the_entry_with_the_largest_time() {
        let json = list(&[
            entry(1_700_000_000_000_000, "1", "/usr/bin/a"),
            entry(1_800_000_000_000_000, "2", "/usr/bin/b"),
            entry(1_750_000_000_000_000, "3", "/usr/bin/c"),
        ]);
        let got = facts(&json).unwrap();
        assert_eq!(got.pid, "2");
        assert_eq!(got.exe, "/usr/bin/b");
        assert_eq!(got.comm, "b");
        assert_eq!(got.sig, "11");
    }

    #[test]
    fn crash_time_format_for_a_known_value() {
        // 2026-10-18 04:57:09 UTC.
        assert_eq!(
            format_time(1_792_299_429_000_000),
            "2026-10-18 04:57:09 UTC"
        );
        assert_eq!(format_time(0), "1970-01-01 00:00:00 UTC");
        // 2000-02-29 23:59:59 UTC, with microseconds that are dropped.
        assert_eq!(format_time(951_868_799_999_999), "2000-02-29 23:59:59 UTC");
        assert_eq!(
            format_time(1_709_164_800_000_000),
            "2024-02-29 00:00:00 UTC"
        );
    }

    #[test]
    fn crash_an_empty_list_and_invalid_json_have_their_own_texts() {
        assert_eq!(text_of(facts("[]")), "No crash was found.");
        assert_eq!(text_of(facts(r#"[{"pid":1}]"#)), "No crash was found.");
        for bad in ["", "not json", "{}", "[1]", r#"[{"time":1},3]"#] {
            assert_eq!(
                text_of(facts(bad)),
                "coredumpctl is not available.",
                "{bad}"
            );
        }
    }

    #[test]
    fn crash_a_pid_that_is_not_digits_refuses_the_newest_entry() {
        let json = r#"[{"time":2,"pid":"12x","exe":"/a"},{"time":1,"pid":5,"exe":"/b"}]"#;
        assert_eq!(text_of(facts(json)), "No crash was found.");
        let json = r#"[{"time":2,"pid":"","exe":"/a"}]"#;
        assert_eq!(text_of(facts(json)), "No crash was found.");
        let json = r#"[{"time":2,"pid":-4,"exe":"/a"}]"#;
        assert_eq!(text_of(facts(json)), "No crash was found.");
        let json = r#"[{"time":2,"pid":"77","exe":"/a"}]"#;
        assert_eq!(facts(json).unwrap().pid, "77");
    }

    #[test]
    fn crash_missing_fields_print_unknown_and_comm_falls_back_to_the_exe() {
        let got = facts(r#"[{"time":1,"pid":4}]"#).unwrap();
        assert_eq!(
            (got.comm.as_str(), got.exe.as_str(), got.sig.as_str()),
            ("unknown", "unknown", "unknown")
        );
        let got = facts(r#"[{"time":1,"pid":4,"exe":null,"sig":"","comm":"foo"}]"#).unwrap();
        assert_eq!(
            (got.comm.as_str(), got.exe.as_str(), got.sig.as_str()),
            ("foo", "unknown", "unknown")
        );
        let got = facts(r#"[{"time":1,"pid":4,"exe":"/usr/bin/x","sig":"SIGSEGV"}]"#).unwrap();
        assert_eq!((got.comm.as_str(), got.sig.as_str()), ("x", "SIGSEGV"));
    }

    #[test]
    fn crash_a_hostile_exe_loses_control_and_bidi_characters_and_is_cut() {
        let hostile = format!(
            "/usr/bin/ev\u{1b}[31mil\n\r\u{0}\u{202e}gnp\u{2066}x\u{2069}\u{200f}\u{061c}{}",
            "z".repeat(500)
        );
        let json = list(&[entry(5, "9", &hostile)]);
        let got = facts(&json).unwrap();
        assert_eq!(got.exe.chars().count(), MAX_TEXT);
        assert!(
            got.exe.starts_with("/usr/bin/ev[31milgnpxzzz"),
            "{}",
            got.exe
        );
        assert!(!got.exe.chars().any(char::is_control));
        let bidi = ['\u{202e}', '\u{2066}', '\u{2069}', '\u{200f}', '\u{061c}'];
        assert!(!got.exe.contains(bidi));
        let prompt = prompt(&got, Path::new("/usr/share/omarchy"));
        let process = prompt.lines().find(|l| l.contains("process:")).unwrap();
        assert_eq!(
            process.chars().count(),
            "  process:  ".len() + got.comm.chars().count()
        );
    }

    #[test]
    fn crash_the_prompt_text_is_the_fixed_text_with_the_values() {
        let got = facts(
            r#"[{"time":1792299429000000,"pid":4242,"sig":11,"exe":"/usr/bin/foo","comm":"foo"}]"#,
        )
        .unwrap();
        let expected = "A process crashed on this Omarchy machine and I want to know why.\n\
\n\
What systemd-coredump recorded:\n\
\x20 process:  foo\n\
\x20 PID:      4242\n\
\x20 binary:   /usr/bin/foo\n\
\x20 signal:   11\n\
\x20 time:     2026-10-18 04:57:09 UTC\n\
\n\
Use the diagnose-crash skill: it covers how to investigate, what to report, and\n\
when a crash is worth reporting upstream to Omarchy. If your harness has no skill\n\
mechanism, read the skill files directly and follow them instead:\n\
\n\
\x20 /opt/omarchy/default/agents/skills/diagnose-crash/SKILL.md\n";
        assert_eq!(prompt(&got, Path::new("/opt/omarchy")), expected);
    }

    #[test]
    fn crash_the_notification_argv_is_fixed() {
        let argv: Vec<String> = notify_argv(Path::new("/bin/fleetdeck"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            [
                "--app-name",
                "Fleet Deck",
                "-u",
                "normal",
                "Fleet Deck",
                "A crash diagnosis session started",
                "--exec",
                "/bin/fleetdeck",
                "desktop",
                "open"
            ]
        );
    }

    fn fake_tool(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn env_with(dir: &Path) -> ChildEnv {
        ChildEnv::scrubbed([(OsString::from("PATH"), dir.as_os_str().to_owned())])
    }

    #[tokio::test]
    async fn crash_coredumpctl_output_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "coredumpctl", "echo '[]'");
        let out = coredumpctl(&env_with(dir.path()), LIMIT).await.unwrap();
        assert_eq!(out.trim(), "[]");
    }

    #[tokio::test]
    async fn crash_a_missing_tool_a_failure_and_too_much_output_are_not_available() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_with(dir.path());
        let missing = coredumpctl(&env, LIMIT).await.unwrap_err();
        assert_eq!(missing.to_string(), "coredumpctl is not available.");
        fake_tool(dir.path(), "coredumpctl", "echo '[]'; exit 3");
        let failed = coredumpctl(&env, LIMIT).await.unwrap_err();
        assert_eq!(failed.to_string(), "coredumpctl is not available.");
        fake_tool(
            dir.path(),
            "coredumpctl",
            "head -c 1100000 /dev/zero | tr '\\0' 'a'",
        );
        let big = coredumpctl(&env, LIMIT).await.unwrap_err();
        assert_eq!(big.to_string(), "coredumpctl is not available.");
    }

    #[tokio::test]
    async fn crash_a_stalled_tool_is_stopped_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        fake_tool(dir.path(), "coredumpctl", "exec sleep 30");
        let started = std::time::Instant::now();
        let err = coredumpctl(&env_with(dir.path()), Duration::from_millis(300))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "coredumpctl is not available.");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn crash_the_start_request_has_the_first_instance_the_home_and_no_mode() {
        let config = StationConfig::parse("", &Env::from_process()).unwrap();
        let request = start_request(&config, "/home/ana".to_owned(), "P".to_owned()).unwrap();
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["op"], "command");
        let command = &json["command"];
        assert_eq!(command["op"], "start");
        assert_eq!(command["spec"]["provider_instance"], "claude");
        assert_eq!(command["spec"]["cwd"], "/home/ana");
        assert_eq!(command["spec"]["mode"], Value::Null);
        assert_eq!(command["prompt"]["text"], "P");
    }

    #[test]
    fn crash_without_a_provider_instance_the_start_is_refused() {
        let mut config = StationConfig::parse("", &Env::from_process()).unwrap();
        config.provider_instances.clear();
        let err = start_request(&config, "/home/ana".to_owned(), "P".to_owned()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "No provider is configured on this station."
        );
    }

    #[test]
    fn crash_the_home_must_exist() {
        let env = Env {
            home: Some("/nonexistent-fleetdeck-home".into()),
            xdg_config_home: None,
            xdg_data_home: None,
        };
        assert_eq!(canonical_home(&env).unwrap_err().to_string(), NO_HOME);
        let none = Env {
            home: None,
            xdg_config_home: None,
            xdg_data_home: None,
        };
        assert_eq!(canonical_home(&none).unwrap_err().to_string(), NO_HOME);
    }
}
