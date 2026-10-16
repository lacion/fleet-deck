//! The `!` shell results of a host input in the Claude user message (B8).

use serde_json::Value;

use super::*;
use crate::adapter::ShellContext;
use crate::claude::tests::{config, init, input, uuid, writes};

fn result(command: &str, end: &str, stdout: &str, stderr: &str) -> ShellContext {
    ShellContext {
        command: command.to_owned(),
        end: end.to_owned(),
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    }
}

fn texts(frame: &Value) -> Vec<String> {
    frame["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["text"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_prompt_writes_two_blocks_per_result_before_the_prompt_text() {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    let mut with = input(1, "go");
    with.context = vec![
        result("make", "exit 2", "", "no rule\n"),
        result("echo <x>", "exit 0", "<x>\n", ""),
    ];
    let frames = writes(&core.prompt(&with).unwrap());
    assert_eq!(
        texts(&frames[0]),
        [
            "<bash-input>make</bash-input>",
            "<bash-stdout>(Bash completed with no output)</bash-stdout><bash-stderr>no rule\n\n[exit 2]</bash-stderr>",
            "<bash-input>echo &lt;x&gt;</bash-input>",
            "<bash-stdout>&lt;x&gt;\n</bash-stdout><bash-stderr></bash-stderr>",
            "go",
        ]
    );
    // The input keeps its own text for the run label.
    assert_eq!(frames[0]["uuid"], uuid(1));
}

#[test]
fn a_prompt_without_results_keeps_the_plain_string() {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    let frames = writes(&core.prompt(&input(1, "go")).unwrap());
    assert_eq!(frames[0]["message"]["content"], "go");
}
