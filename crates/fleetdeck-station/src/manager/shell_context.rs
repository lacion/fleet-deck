//! The input record of a prompt, a follow-up or a steer, with the `!` shell
//! results that it carries to the model (B8, DL-196).
//!
//! The one helper [`record`] builds every such record (`input_effect` and
//! the resume-first path both call it). It resolves `input.shell_results`
//! against the attachable rows of the session, gives each named result an
//! equal share of the byte budget (at most [`RESULT_BUDGET`] each), cuts
//! the results to their share, copies them into the record and marks their
//! rows as carried, all in the decision transaction. So the context is part
//! of the input: durable in the effect, replayed with it, delivered once.
//!
//! No named result is deferred (B8): a result that does not fit its share
//! is cut, never left out. An input that names more than [`MAX_RESULTS`]
//! results carries the first ones; the rest wait, attachable, for the next
//! input.

use fleetdeck_proto::command::UserInput;
use fleetdeck_proto::event::RunOrigin;
use fleetdeck_proto::{InputId, SessionId};
use fleetdeck_providers::ShellContext;
use fleetdeck_providers::claude::control::shell_blocks;

use crate::store::shell_runs::{ShellRow, ShellState};
use crate::store::{DecideError, Decision, InputRecord};

/// The most bytes of one result (its two serialized, escaped blocks).
pub const RESULT_BUDGET: usize = 32 * 1024;
/// The most bytes of all results of one input.
pub const INPUT_BUDGET: usize = 64 * 1024;
/// The most results of one input (B8); each gets at least 1 KiB.
pub const MAX_RESULTS: usize = 64;

/// The `end` text of a settled row.
fn end_of(row: &ShellRow) -> String {
    match (row.state, row.exit_code) {
        (ShellState::Done, _) => "exit 0".to_owned(),
        (ShellState::Failed, Some(code)) => format!("exit {code}"),
        (ShellState::Failed, None) => "failed".to_owned(),
        (ShellState::Cancelled, _) => "cancelled".to_owned(),
        (ShellState::Timeout, _) => "timed out".to_owned(),
        _ => "outcome unknown: the station restarted during the command".to_owned(),
    }
}

/// The serialized, escaped size of the blocks of one result.
fn size(result: &ShellContext) -> usize {
    serde_json::to_string(&shell_blocks(result)).map_or(usize::MAX, |s| s.len())
}

/// `text` with its first `cut` characters replaced by the notice.
fn cut_front(text: &str, cut: usize) -> String {
    if cut == 0 {
        return text.to_owned();
    }
    let rest: String = text.chars().skip(cut).collect();
    format!("[… {cut} characters omitted]\n{rest}")
}

/// Cuts the streams of `result` from the front until its blocks fit `max`:
/// stdout first (down to its notice), then stderr. When that is not enough,
/// the command is cut from its end and gets a `…` (B8).
fn fit(mut result: ShellContext, max: usize) -> ShellContext {
    if size(&result) <= max {
        return result;
    }
    let original = result.clone();
    for stdout_stream in [true, false] {
        let text = if stdout_stream {
            &original.stdout
        } else {
            &original.stderr
        };
        let total = text.chars().count();
        // The least cut that fits; the whole stream when none does.
        let (mut low, mut high) = (0, total);
        while low < high {
            let mid = low + (high - low) / 2;
            let mut trial = result.clone();
            *(if stdout_stream {
                &mut trial.stdout
            } else {
                &mut trial.stderr
            }) = cut_front(text, mid);
            if size(&trial) <= max {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        *(if stdout_stream {
            &mut result.stdout
        } else {
            &mut result.stderr
        }) = cut_front(text, low);
        if size(&result) <= max {
            return result;
        }
    }
    // The longest prefix of the command that fits, then `…`.
    let command = result.command.clone();
    let total = command.chars().count();
    let with_prefix = |keep: usize| {
        let mut cut: String = command.chars().take(keep).collect();
        cut.push('…');
        cut
    };
    let (mut low, mut high) = (0, total);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let mut trial = result.clone();
        trial.command = with_prefix(mid);
        if size(&trial) <= max {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    result.command = with_prefix(low);
    result
}

/// The rows that `named` picks, in order: an id that is unknown, not
/// attachable or repeated is dropped; the ones past [`MAX_RESULTS`] wait (B8).
fn select<'a>(named: &[String], attachable: &'a [ShellRow]) -> Vec<&'a ShellRow> {
    let mut rows: Vec<&ShellRow> = Vec::new();
    for id in named {
        if rows.len() == MAX_RESULTS {
            break;
        }
        if let Some(row) = attachable.iter().find(|r| r.effect_id == *id)
            && !rows.iter().any(|r| r.effect_id == *id)
        {
            rows.push(row);
        }
    }
    rows
}

/// Builds the input record of `input` for `session_id`: a new input id and
/// the shell results that the input names and the session can carry.
///
/// # Errors
/// A store error.
pub(super) fn record(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    origin: RunOrigin,
    input: UserInput,
) -> Result<InputRecord, DecideError> {
    let input_id: InputId = d.new_id();
    let mut shell_context = Vec::new();
    if !input.shell_results.is_empty() {
        let attachable = d.shell_attachable(session_id)?;
        let rows = select(&input.shell_results, &attachable);
        // Equal shares: every kept result is carried (B8).
        let share = RESULT_BUDGET.min(INPUT_BUDGET / rows.len().max(1));
        let mut kept: Vec<String> = Vec::new();
        for row in rows {
            shell_context.push(fit(
                ShellContext {
                    command: row.command.clone(),
                    end: end_of(row),
                    stdout: row.stdout_tail.clone().unwrap_or_default(),
                    stderr: row.stderr_tail.clone().unwrap_or_default(),
                },
                share,
            ));
            kept.push(row.effect_id.clone());
        }
        d.shell_include(&kept, input_id.as_str())?;
    }
    Ok(InputRecord {
        input_id,
        origin,
        input,
        shell_context,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(stdout: &str, stderr: &str) -> ShellContext {
        ShellContext {
            command: "cmd".to_owned(),
            end: "exit 0".to_owned(),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn a_result_within_the_budget_is_not_cut() {
        let small = result(&"a".repeat(1000), "err");
        assert_eq!(fit(small.clone(), RESULT_BUDGET), small);
    }

    #[test]
    fn stdout_is_cut_from_the_front_first_with_a_notice() {
        let big = result(&format!("{}END", "<".repeat(30_000)), "keep");
        let cut = fit(big, RESULT_BUDGET);
        assert!(cut.stdout.starts_with("[… "), "{}", &cut.stdout[..30]);
        assert!(cut.stdout.ends_with("END"), "the end of the stream stays");
        assert_eq!(cut.stderr, "keep");
        assert!(size(&cut) <= RESULT_BUDGET);
        // The notice counts the cut characters.
        let n: usize = cut.stdout["[… ".len()..]
            .split_once(' ')
            .unwrap()
            .0
            .parse()
            .unwrap();
        let kept = cut
            .stdout
            .split_once("omitted]\n")
            .unwrap()
            .1
            .chars()
            .count();
        assert_eq!(n + kept, 30_003);
    }

    #[test]
    fn stderr_is_cut_after_stdout_is_down_to_its_notice() {
        let big = result(&"<".repeat(30_000), &"&".repeat(30_000));
        let cut = fit(big, RESULT_BUDGET);
        assert!(size(&cut) <= RESULT_BUDGET);
        assert!(cut.stdout.starts_with("[… 30000 characters omitted]"));
        assert!(cut.stderr.starts_with("[… "));
    }

    #[test]
    fn a_long_command_is_cut_from_its_end_to_fit() {
        let long = ShellContext {
            command: "c".repeat(4000),
            end: "exit 0".to_owned(),
            stdout: String::new(),
            stderr: String::new(),
        };
        let cut = fit(long, 1024);
        assert!(cut.command.ends_with('…'), "{}", cut.command.len());
        assert!(cut.command.chars().count() < 4000);
        assert!(size(&cut) <= 1024);
        // The longest prefix that fits: one more character would not.
        let mut more = cut.clone();
        more.command = format!("{}c…", cut.command.trim_end_matches('…'));
        assert!(size(&more) > 1024);
    }

    #[test]
    fn three_results_of_30_kib_get_equal_shares() {
        let share = INPUT_BUDGET / 3;
        let total: usize = (0..3)
            .map(|_| {
                let cut = fit(result(&"a".repeat(30 * 1024), ""), share);
                assert!(cut.stdout.starts_with("[… "));
                assert!(size(&cut) <= share);
                size(&cut)
            })
            .sum();
        assert!(total <= INPUT_BUDGET);
    }

    fn row_with(id: &str) -> ShellRow {
        ShellRow {
            effect_id: id.to_owned(),
            session_id: "s".into(),
            provider: "claude".to_owned(),
            worktree_root: "/w".to_owned(),
            command: "c".to_owned(),
            state: ShellState::Done,
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            cancel_requested: false,
            advance_pending: false,
            generation: None,
            included_in: None,
            created_at: 0,
            ended_at: None,
        }
    }

    #[test]
    fn at_most_64_named_results_are_selected_and_the_rest_wait() {
        let attachable: Vec<ShellRow> = (0..65).map(|i| row_with(&format!("e{i}"))).collect();
        let mut named: Vec<String> = attachable.iter().map(|r| r.effect_id.clone()).collect();
        // An unknown id and a repeated id are dropped before the count.
        named.insert(1, "nope".to_owned());
        named.insert(2, "e0".to_owned());
        let rows = select(&named, &attachable);
        assert_eq!(rows.len(), MAX_RESULTS);
        assert_eq!(rows[0].effect_id, "e0");
        assert_eq!(rows[63].effect_id, "e63");
        assert!(rows.iter().all(|r| r.effect_id != "e64"));
    }

    #[test]
    fn the_end_text_follows_the_state() {
        let mut row = ShellRow {
            effect_id: "e".to_owned(),
            session_id: "s".into(),
            provider: "claude".to_owned(),
            worktree_root: "/w".to_owned(),
            command: "c".to_owned(),
            state: ShellState::Failed,
            exit_code: Some(7),
            stdout_tail: None,
            stderr_tail: None,
            cancel_requested: false,
            advance_pending: false,
            generation: None,
            included_in: None,
            created_at: 0,
            ended_at: None,
        };
        assert_eq!(end_of(&row), "exit 7");
        row.exit_code = None;
        assert_eq!(end_of(&row), "failed");
        row.state = ShellState::Done;
        assert_eq!(end_of(&row), "exit 0");
        row.state = ShellState::Cancelled;
        assert_eq!(end_of(&row), "cancelled");
        row.state = ShellState::Timeout;
        assert_eq!(end_of(&row), "timed out");
        row.state = ShellState::Unknown;
        assert_eq!(
            end_of(&row),
            "outcome unknown: the station restarted during the command"
        );
    }
}
