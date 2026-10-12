//! M3 item 7-6: every `fleetdeck ...` command that the new README, the
//! operations guide and the cutover guide name is a real subcommand of this
//! build, and every `--flag` that they give is accepted by it.

use std::path::PathBuf;
use std::process::Command;

const DOCS: &[&str] = &["README.md", "docs/v2/operations.md", "docs/v2/cutover.md"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The inline-code spans of `text` that start with `fleetdeck `.
fn commands(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("fleetdeck ") && !span.contains('\n'))
        .map(str::to_owned)
        .collect()
}

/// The subcommand path (`["station", "upgrade", "prepare"]`) and the long
/// flags of one documented command.
fn split(command: &str) -> (Vec<String>, Vec<String>) {
    let mut path = Vec::new();
    let mut flags = Vec::new();
    let mut in_path = true;
    for word in command.split_whitespace().skip(1) {
        let word = word.trim_matches(|c| c == '[' || c == ']');
        if let Some(flag) = word.strip_prefix("--") {
            in_path = false;
            flags.push(flag.split('=').next().unwrap_or(flag).to_owned());
        } else if in_path && word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            path.push(word.to_owned());
        } else {
            in_path = false;
        }
    }
    (path, flags)
}

#[test]
fn every_documented_fleetdeck_command_and_flag_exists() {
    let mut checked = 0;
    for doc in DOCS {
        let text = std::fs::read_to_string(root().join(doc)).unwrap();
        for command in commands(&text) {
            let (path, flags) = split(&command);
            let out = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
                .args(&path)
                .arg("--help")
                .output()
                .unwrap();
            let help = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "{doc}: `{command}` is not a command of this build: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            for flag in flags {
                assert!(
                    help.contains(&format!("--{flag}")),
                    "{doc}: `{command}` names --{flag}, which `fleetdeck {}` does not accept",
                    path.join(" ")
                );
            }
            checked += 1;
        }
    }
    assert!(checked >= 20, "only {checked} documented commands found");
}
