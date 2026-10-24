//! The notifications of `fleetdeck desktop feed --notify` (R13 P4 task B2,
//! `omarchy-p4.md` §3.3).
//!
//! A session that starts to need you gives one desktop notification through
//! `omarchy-notification-send`. The first good summary is only a baseline.
//! A notification never answers a request, and its text holds no title and
//! no tool name: the station name, a fixed word and the callsign only.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use fleetdeck_proto::event::RequestKind;
use fleetdeck_proto::fleet_summary::{FleetSummary, NeedsYouItem};

/// The program that shows a notification.
const SEND: &str = "omarchy-notification-send";
/// The tools that the notification chain needs, in the order of the warnings.
const TOOLS: [&str; 3] = [SEND, "busctl", "jq"];
/// A new request older than this many seconds (corrected) does not notify.
const MAX_AGE_S: u32 = 60;
/// The most notifications for one summary, apart from the "more" one.
const MAX_SINGLE: usize = 5;
/// The longest description, in characters.
const MAX_DESCRIPTION: usize = 300;
/// The warning when the path of this program is not known.
const NO_EXE: &str =
    "fleetdeck: the path of this program is not known; notifications have no click action.";

type Key = (String, String, String);

fn key(item: &NeedsYouItem) -> Key {
    (
        item.station_id.clone(),
        item.session.clone(),
        item.request.clone(),
    )
}

/// `^[A-Za-z0-9_-]{1,64}$`: the grammar of the ids of the summary.
pub(crate) fn id_ok(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$`: a name that never starts with `-`.
fn headline_name_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn headline(item: &NeedsYouItem) -> String {
    let name = match &item.callsign {
        Some(callsign) if headline_name_ok(callsign) => callsign.clone(),
        _ => {
            let prefix: String = item.session.chars().take(8).collect();
            if headline_name_ok(&prefix) {
                prefix
            } else {
                "session".to_owned()
            }
        }
    };
    format!("{name} needs you")
}

fn description(item: &NeedsYouItem) -> String {
    let kind = match item.kind {
        RequestKind::Command | RequestKind::FileChange | RequestKind::Tool => "Approval",
        RequestKind::Question => "Question",
    };
    let station: String = item
        .station
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | '&'))
        .collect();
    format!("{kind} on {station}")
        .chars()
        .take(MAX_DESCRIPTION)
        .collect()
}

/// The `--exec <exe> desktop open ...` tail. Without `exe` there is none.
fn exec_tail(exe: Option<&Path>, ids: Option<[&str; 3]>) -> Vec<OsString> {
    let Some(exe) = exe else {
        return Vec::new();
    };
    let mut tail: Vec<OsString> =
        vec!["--exec".into(), exe.into(), "desktop".into(), "open".into()];
    if let Some([station_id, session, request]) = ids {
        tail.push(format!("{station_id}/{session}").into());
        tail.push("--request".into());
        tail.push(request.into());
    }
    tail
}

fn argv(headline: &str, description: &str, tail: Vec<OsString>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["--app-name", "Fleet Deck", "-u", "normal"]
        .into_iter()
        .map(OsString::from)
        .collect();
    argv.push(headline.into());
    argv.push(description.into());
    argv.extend(tail);
    argv
}

/// The argv of the notification of one request (without the program name).
/// `--exec` is added only when the three ids match the grammar.
fn item_argv(item: &NeedsYouItem, exe: Option<&Path>) -> Vec<OsString> {
    let ids_ok = id_ok(&item.station_id) && id_ok(&item.session) && id_ok(&item.request);
    let tail = if ids_ok {
        exec_tail(
            exe,
            Some([
                item.station_id.as_str(),
                item.session.as_str(),
                item.request.as_str(),
            ]),
        )
    } else {
        Vec::new()
    };
    argv(&headline(item), &description(item), tail)
}

/// The argv of the "more" notification for `n` requests.
fn more_argv(n: usize, exe: Option<&Path>) -> Vec<OsString> {
    argv(
        "Fleet Deck",
        &format!("{n} more sessions need you"),
        exec_tail(exe, None),
    )
}

/// The new requests of `summary` that notify, in the order of `needs_you`,
/// and how many more than [`MAX_SINGLE`] there are. `known` is the key set
/// of the last good summary; it becomes the key set of this one. The first
/// call only sets the baseline.
fn plan<'a>(
    known: &mut Option<HashSet<Key>>,
    summary: &'a FleetSummary,
) -> (Vec<&'a NeedsYouItem>, usize) {
    let current: HashSet<Key> = summary.needs_you.iter().map(key).collect();
    let Some(before) = known.replace(current) else {
        return (Vec::new(), 0);
    };
    let mut seen = HashSet::new();
    let mut fresh: Vec<&NeedsYouItem> = summary
        .needs_you
        .iter()
        .filter(|item| {
            let key = key(item);
            !before.contains(&key) && seen.insert(key) && item.age_s <= MAX_AGE_S
        })
        .collect();
    let more = fresh.len().saturating_sub(MAX_SINGLE);
    fresh.truncate(MAX_SINGLE);
    (fresh, more)
}

/// Whether `name` is an executable file in one of the `PATH` directories.
fn on_path(name: &str, path: Option<&OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn missing_tool(tool: &str) -> String {
    format!("fleetdeck: {tool} is not installed; notifications are off.")
}

#[derive(Debug, Default)]
enum Tools {
    /// Not looked up yet: the lookup runs before the first send.
    #[default]
    Unchecked,
    Ready {
        exe: Option<PathBuf>,
    },
    Off,
}

/// The state of `--notify` for one run of the feed.
#[derive(Debug, Default)]
pub struct Notifier {
    /// The key set of the last good summary; `None` before the baseline.
    known: Option<HashSet<Key>>,
    tools: Tools,
}

impl Notifier {
    /// Looks up the tools once and the path of this program. Warns on stderr.
    fn check_tools(&mut self) {
        if !matches!(self.tools, Tools::Unchecked) {
            return;
        }
        let path = std::env::var_os("PATH");
        let mut all = true;
        for tool in TOOLS {
            if !on_path(tool, path.as_deref()) {
                eprintln!("{}", missing_tool(tool));
                all = false;
            }
        }
        self.tools = if all {
            let exe = match std::env::current_exe() {
                Ok(exe) => Some(exe),
                Err(e) => {
                    tracing::debug!(error = %e, "current_exe failed");
                    eprintln!("{NO_EXE}");
                    None
                }
            };
            Tools::Ready { exe }
        } else {
            Tools::Off
        };
    }

    /// Notifies for the requests of `summary` (corrected ages) that are new.
    pub async fn on_summary(&mut self, summary: &FleetSummary) {
        let (fresh, more) = plan(&mut self.known, summary);
        if fresh.is_empty() {
            return;
        }
        self.check_tools();
        let Tools::Ready { exe } = &self.tools else {
            return;
        };
        let exe = exe.clone();
        let mut batch: Vec<Vec<OsString>> = fresh
            .iter()
            .map(|item| item_argv(item, exe.as_deref()))
            .collect();
        if more > 0 {
            batch.push(more_argv(more, exe.as_deref()));
        }
        for args in batch {
            if !self.send(&args) {
                return;
            }
        }
    }

    /// Starts the program and does not wait for it; a task reaps it. Returns
    /// `false` when notifications are now off for the run.
    fn send(&mut self, args: &[OsString]) -> bool {
        let spawned = tokio::process::Command::new(SEND)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                tokio::spawn(async move {
                    if let Err(e) = child.wait().await {
                        tracing::debug!(error = %e, "waiting for {SEND} failed");
                    }
                });
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("{}", missing_tool(SEND));
                self.tools = Tools::Off;
                false
            }
            Err(e) => {
                eprintln!("fleetdeck: cannot start {SEND}: {e}");
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(session: &str, request: &str, age_s: u32) -> NeedsYouItem {
        NeedsYouItem {
            station: "srv".to_owned(),
            station_id: "st1".to_owned(),
            session: session.to_owned(),
            request: request.to_owned(),
            callsign: Some("lynx-12".to_owned()),
            kind: RequestKind::Tool,
            age_s,
        }
    }

    fn summary(items: Vec<NeedsYouItem>) -> FleetSummary {
        let text = r#"{"v":1,"at":"2026-10-18T12:04:00Z","board":null,"deck":"up",
 "counts":{"working":0,"idle":0,"needs_you":0,"blocked":0,"lost":0},
 "needs_you":[],"usage":null,"stations":[]}"#;
        let mut summary: FleetSummary = serde_json::from_str(text).unwrap();
        summary.needs_you = items;
        summary
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn requests(plan: &(Vec<&NeedsYouItem>, usize)) -> Vec<String> {
        plan.0.iter().map(|i| i.request.clone()).collect()
    }

    #[test]
    fn notify_first_summary_is_only_a_baseline() {
        let mut known = None;
        let first = summary(vec![item("s1", "r1", 5)]);
        assert_eq!(plan(&mut known, &first), (Vec::new(), 0));
        let second = summary(vec![item("s1", "r1", 5), item("s2", "r2", 5)]);
        assert_eq!(requests(&plan(&mut known, &second)), ["r2"]);
    }

    #[test]
    fn notify_a_key_notifies_once_and_a_returning_key_again() {
        let mut known = None;
        plan(&mut known, &summary(vec![]));
        let with = summary(vec![item("s1", "r1", 5)]);
        assert_eq!(requests(&plan(&mut known, &with)), ["r1"]);
        assert!(plan(&mut known, &with).0.is_empty(), "same key: once");
        plan(&mut known, &summary(vec![]));
        assert_eq!(
            requests(&plan(&mut known, &with)),
            ["r1"],
            "a key that left and came back notifies again"
        );
    }

    #[test]
    fn notify_a_duplicate_key_in_one_summary_notifies_once() {
        let mut known = None;
        plan(&mut known, &summary(vec![]));
        let dup = summary(vec![item("s1", "r1", 5), item("s1", "r1", 6)]);
        assert_eq!(requests(&plan(&mut known, &dup)), ["r1"]);
    }

    #[test]
    fn notify_age_rule_skips_old_requests_and_counts_them_as_seen() {
        let mut known = None;
        plan(&mut known, &summary(vec![]));
        let old = summary(vec![item("s1", "r1", 61), item("s2", "r2", 60)]);
        assert_eq!(requests(&plan(&mut known, &old)), ["r2"], "60 s notifies");
        let later = summary(vec![item("s1", "r1", 5), item("s2", "r2", 5)]);
        assert!(
            plan(&mut known, &later).0.is_empty(),
            "a skipped key is seen"
        );
    }

    #[test]
    fn notify_a_burst_of_six_gives_five_and_one_more() {
        let mut known = None;
        plan(&mut known, &summary(vec![]));
        let items: Vec<_> = (1..=6)
            .map(|n| item(&format!("s{n}"), &format!("r{n}"), 5))
            .collect();
        let batch = summary(items);
        let got = plan(&mut known, &batch);
        assert_eq!(requests(&got), ["r1", "r2", "r3", "r4", "r5"]);
        assert_eq!(got.1, 1);
    }

    #[test]
    fn notify_more_counts_only_requests_that_pass_the_age_rule() {
        let mut known = None;
        plan(&mut known, &summary(vec![]));
        let mut items: Vec<_> = (1..=8)
            .map(|n| item(&format!("s{n}"), &format!("r{n}"), 5))
            .collect();
        items[0].age_s = 500;
        items[1].age_s = 500;
        let batch = summary(items);
        let got = plan(&mut known, &batch);
        assert_eq!(got.0.len(), 5);
        assert_eq!(got.1, 1, "6 pass the age rule, 5 notify");
    }

    #[test]
    fn notify_argv_is_exact() {
        let exe = Path::new("/usr/bin/fleetdeck");
        let got = strings(&item_argv(&item("sess1", "req1", 5), Some(exe)));
        assert_eq!(
            got,
            [
                "--app-name",
                "Fleet Deck",
                "-u",
                "normal",
                "lynx-12 needs you",
                "Approval on srv",
                "--exec",
                "/usr/bin/fleetdeck",
                "desktop",
                "open",
                "st1/sess1",
                "--request",
                "req1"
            ]
        );
        assert!(!got.iter().any(|a| a == "--"));
    }

    #[test]
    fn notify_more_argv_is_exact() {
        let exe = Path::new("/usr/bin/fleetdeck");
        let got = strings(&more_argv(1, Some(exe)));
        assert_eq!(
            got,
            [
                "--app-name",
                "Fleet Deck",
                "-u",
                "normal",
                "Fleet Deck",
                "1 more sessions need you",
                "--exec",
                "/usr/bin/fleetdeck",
                "desktop",
                "open"
            ]
        );
    }

    #[test]
    fn notify_without_the_exe_path_has_no_exec_tail() {
        let got = strings(&item_argv(&item("sess1", "req1", 5), None));
        assert_eq!(got.len(), 6);
        assert!(!got.iter().any(|a| a == "--exec"));
        assert_eq!(strings(&more_argv(3, None)).len(), 6);
        assert_eq!(
            NO_EXE,
            "fleetdeck: the path of this program is not known; notifications have no click action."
        );
    }

    #[test]
    fn notify_headline_never_starts_with_a_dash() {
        let mut it = item("sessionid123", "r", 5);
        it.callsign = Some("-rf".to_owned());
        assert_eq!(headline(&it), "sessioni needs you");
        it.callsign = None;
        assert_eq!(headline(&it), "sessioni needs you");
        it.session = "-sessionid".to_owned();
        assert_eq!(headline(&it), "session needs you");
        it.session = "_abc".to_owned();
        assert_eq!(headline(&it), "session needs you");
        it.callsign = Some("a".repeat(33));
        it.session = "short".to_owned();
        assert_eq!(headline(&it), "short needs you");
        it.callsign = Some("Lynx.12_x".to_owned());
        assert_eq!(headline(&it), "Lynx.12_x needs you");
    }

    #[test]
    fn notify_description_uses_fixed_words_and_a_clean_station_name() {
        let mut it = item("s", "r", 5);
        for (kind, word) in [
            (RequestKind::Command, "Approval"),
            (RequestKind::FileChange, "Approval"),
            (RequestKind::Tool, "Approval"),
            (RequestKind::Question, "Question"),
        ] {
            it.kind = kind;
            assert_eq!(description(&it), format!("{word} on srv"));
        }
        it.station = "a<b>&c\u{7}d\n".to_owned();
        assert_eq!(description(&it), "Question on abcd");
        it.station = "x".repeat(400);
        assert_eq!(description(&it).chars().count(), 300);
    }

    #[test]
    fn notify_an_id_outside_the_grammar_gets_no_exec() {
        let exe = Path::new("/usr/bin/fleetdeck");
        for bad in ["a b", "a/b", "-x y", ""] {
            let mut it = item("sess1", "req1", 5);
            it.request = bad.to_owned();
            let got = strings(&item_argv(&it, Some(exe)));
            assert!(!got.iter().any(|a| a == "--exec"), "{bad:?}: {got:?}");
            assert_eq!(got.len(), 6);
        }
        let mut it = item("sess1", "req1", 5);
        it.station_id = "a.b".to_owned();
        assert_eq!(strings(&item_argv(&it, Some(exe))).len(), 6);
        let mut it = item("sess 1", "req1", 5);
        it.callsign = Some("-opt".to_owned());
        let got = strings(&item_argv(&it, Some(exe)));
        assert_eq!(got.len(), 6);
        assert_eq!(got[4], "session needs you");
        assert!(!got[4].starts_with('-') && !got[5].starts_with('-'));
    }

    #[test]
    fn notify_path_lookup_needs_an_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tool");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert!(!on_path("tool", Some(&path)), "no execute bit");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(on_path("tool", Some(&path)));
        assert!(!on_path("other", Some(&path)));
        assert!(!on_path("tool", None));
        assert_eq!(
            missing_tool("jq"),
            "fleetdeck: jq is not installed; notifications are off."
        );
    }
}
