//! The status projection of a card (PLAN §7.2): [`card_status`] maps a
//! session projection and the state of its station link to a column.
//!
//! Order of the rules:
//! 1. `lost`: the station restarted and the process is gone.
//! 2. `ended`: the session ended. An ended card stays ended while the link
//!    is down, because nothing can change it.
//! 3. `unreachable`: the station link is down; the card keeps its last
//!    known projection for the badge.
//! 4. `needs_you`: an open approval or question.
//! 5. `queued`: the session is starting and no run started yet.
//! 6. `verifying`: a running shell tool matches the test-runner pattern;
//!    `working`: a run or background work runs (DL-049).
//! 7. `idle`.

use crate::event::{SessionState, ToolKind};
use crate::session::{Activity, CardStatus, SessionSummary};

/// The test runners of v0's `TEST_RUNNER_RE`
/// (`\b(pytest|jest|vitest|go test|cargo test|npm (run )?test)\b`).
const TEST_RUNNERS: [&str; 7] = [
    "pytest",
    "jest",
    "vitest",
    "go test",
    "cargo test",
    "npm test",
    "npm run test",
];

/// The column of a card. `reachable` is the state of the station link.
#[must_use]
pub fn card_status(summary: &SessionSummary, reachable: bool) -> CardStatus {
    if summary.state == SessionState::Lost {
        return CardStatus::Lost;
    }
    if summary.activity == Activity::Ended || summary.state == SessionState::Exited {
        return CardStatus::Ended;
    }
    if !reachable {
        return CardStatus::Unreachable;
    }
    match summary.activity {
        Activity::NeedsYou => CardStatus::NeedsYou,
        _ if summary.state == SessionState::Starting && summary.current_run.is_none() => {
            CardStatus::Queued
        }
        Activity::Working | Activity::Background => {
            let testing = summary
                .running_tools
                .iter()
                .any(|t| t.tool_kind == ToolKind::Shell && is_test_command(&t.input_summary));
            if testing {
                CardStatus::Verifying
            } else {
                CardStatus::Working
            }
        }
        Activity::Idle | Activity::Ended => CardStatus::Idle,
    }
}

/// Whether a shell command runs tests: one of [`TEST_RUNNERS`] at word
/// boundaries, as v0's regular expression matches it.
#[must_use]
pub fn is_test_command(command: &str) -> bool {
    TEST_RUNNERS
        .iter()
        .any(|runner| contains_word(command, runner))
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + needle.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SessionState;
    use crate::ids::{NodeId, RunId, SessionId};
    use crate::session::RunningTool;

    fn summary(state: SessionState, activity: Activity) -> SessionSummary {
        let mut s = SessionSummary::new(SessionId::from("s"));
        s.state = state;
        s.activity = activity;
        s
    }

    fn shell(command: &str) -> RunningTool {
        RunningTool {
            node_id: NodeId::from("n"),
            tool_kind: ToolKind::Shell,
            title: "Bash".to_owned(),
            input_summary: command.to_owned(),
        }
    }

    #[test]
    fn test_runners_match_at_word_boundaries_only() {
        for yes in [
            "cargo test -p x",
            "cd web && npm run test",
            "npm test",
            "uv run pytest -q",
            "go test ./...",
            "npx vitest run",
            "jest",
        ] {
            assert!(is_test_command(yes), "{yes}");
        }
        for no in [
            "cargo testing",
            "pytester",
            "majestic",
            "npm run tests",
            "cargo build",
            "",
        ] {
            assert!(!is_test_command(no), "{no}");
        }
    }

    #[test]
    fn each_rule_maps_to_its_column_in_order() {
        use Activity as A;
        use CardStatus as C;
        use SessionState as S;
        let mut running = summary(S::Running, A::Working);
        running.current_run = Some(RunId::from("r"));
        let mut testing = running.clone();
        testing.running_tools = vec![shell("ls"), shell("cargo test")];
        let mut test_not_shell = running.clone();
        test_not_shell.running_tools = vec![RunningTool {
            tool_kind: ToolKind::Other,
            ..shell("cargo test")
        }];
        let cases = [
            (summary(S::Lost, A::Ended), true, C::Lost),
            (summary(S::Lost, A::Ended), false, C::Lost),
            (summary(S::Exited, A::Ended), true, C::Ended),
            (summary(S::Exited, A::Ended), false, C::Ended),
            (running.clone(), false, C::Unreachable),
            (summary(S::Running, A::NeedsYou), true, C::NeedsYou),
            (summary(S::Starting, A::Idle), true, C::Queued),
            (summary(S::Starting, A::NeedsYou), true, C::NeedsYou),
            (running, true, C::Working),
            (testing, true, C::Verifying),
            (test_not_shell, true, C::Working),
            (summary(S::Running, A::Background), true, C::Working),
            (summary(S::Running, A::Idle), true, C::Idle),
            (summary(S::Stopping, A::Idle), true, C::Idle),
        ];
        for (s, reachable, want) in cases {
            assert_eq!(
                card_status(&s, reachable),
                want,
                "{s:?} reachable={reachable}"
            );
        }
    }
}
