//! The headless CLI call of the model help (R10, DL-167). One call is one
//! short process: the prompt goes in on stdin, the answer and the token use
//! come out of the JSON the CLI prints. The runner never sees a session; it
//! runs in a private empty directory, with the instance environment, a
//! disabled tool set, and a hard time limit.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::Driver;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The longest one call may run.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The most bytes of CLI output that the runner reads.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// One call of the runner.
#[derive(Debug, Clone)]
pub struct Call {
    pub driver: Driver,
    pub binary: PathBuf,
    pub env: ChildEnv,
    pub model: String,
    pub prompt: String,
    pub max_output_tokens: u32,
}

/// What a call returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Why a call gave no text. Each reason is a plain message for the log; a
/// caller shows nothing to the owner (R10 "Failure").
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("cannot start `{0}`: {1}")]
    Spawn(String, std::io::Error),
    #[error("the call took longer than {} s", CALL_TIMEOUT.as_secs())]
    Timeout,
    #[error("the CLI exited with {0}")]
    Exit(String),
    #[error("the CLI output is not what was expected: {0}")]
    Parse(&'static str),
    #[error("the CLI reported an error")]
    Reported,
    /// The CLI used a tool (NV-03).
    #[error("codex used a tool; model help allows none")]
    ToolUse,
    /// The CLI printed too much (NV-04).
    #[error("the provider printed more than 256 KiB")]
    TooLarge,
    #[error("private directory: {0}")]
    Dir(std::io::Error),
}

/// Runs one call. The real implementation is [`CliRunner`]; the tests use a
/// fake.
#[async_trait::async_trait]
pub trait Runner: Send + Sync + std::fmt::Debug {
    async fn run(&self, call: Call) -> Result<Run, RunError>;
}

/// The runner that starts the provider CLI.
#[derive(Debug, Default)]
pub struct CliRunner;

#[async_trait::async_trait]
impl Runner for CliRunner {
    async fn run(&self, call: Call) -> Result<Run, RunError> {
        let dir = PrivateDir::create().map_err(RunError::Dir)?;
        let mut command = match call.driver {
            Driver::Claude => claude_command(&call),
            Driver::Codex => codex_command(&call),
        };
        command
            .current_dir(dir.path())
            .env_clear()
            .envs(call.env.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let name = call.binary.display().to_string();
        let mut child = command.spawn().map_err(|e| RunError::Spawn(name, e))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let prompt = call.prompt.clone();
        let feed = async move {
            if let Some(mut stdin) = stdin {
                // A CLI that exits early closes the pipe; the exit status
                // tells the rest.
                let _ = stdin.write_all(prompt.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
        };
        // The read stops one byte after the limit, so a CLI cannot fill the
        // memory of the station (NV-04).
        let read = async move {
            let mut bytes = Vec::new();
            if let Some(stdout) = stdout {
                stdout
                    .take(MAX_OUTPUT_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .await?;
            }
            Ok::<_, std::io::Error>(bytes)
        };
        let finished = tokio::time::timeout(CALL_TIMEOUT, async {
            // The feed runs beside the read, but only the read decides: a
            // CLI that prints too much and ignores its stdin must not hold
            // the call until the time limit.
            tokio::pin!(feed, read);
            let mut fed = false;
            let bytes = loop {
                tokio::select! {
                    done = &mut read => break done?,
                    () = &mut feed, if !fed => fed = true,
                }
            };
            if bytes.len() > MAX_OUTPUT_BYTES {
                let _ = child.kill().await;
                return Ok(Err(RunError::TooLarge));
            }
            let status = child.wait().await?;
            Ok::<_, std::io::Error>(Ok((status, bytes)))
        })
        .await
        .map_err(|_| RunError::Timeout)?;
        let (status, bytes) =
            finished.map_err(|e| RunError::Spawn(call.binary.display().to_string(), e))??;
        if !status.success() {
            // The JSON `result` of a failed Claude call names the reason
            // (for example a failed login); keep its first line, cut.
            let reason = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v.get("result").and_then(Value::as_str).map(str::to_owned))
                .and_then(|r| {
                    r.lines()
                        .next()
                        .map(|l| l.chars().take(160).collect::<String>())
                })
                .filter(|r| !r.trim().is_empty());
            return Err(RunError::Exit(match reason {
                Some(reason) => format!("{status} ({reason})"),
                None => status.to_string(),
            }));
        }
        let stdout = String::from_utf8_lossy(&bytes);
        match call.driver {
            Driver::Claude => parse_claude(&stdout),
            Driver::Codex => parse_codex(&stdout),
        }
    }
}

/// `claude -p` with one turn, no tools and JSON output. The context flags
/// keep the preamble small: a default call sends about 20000 input tokens of
/// settings, memory and tool text that a title never needs.
///
/// The user settings stay on (`--setting-sources user`): without them the
/// CLI cannot refresh its OAuth login and every call fails with "OAuth
/// session expired". `disableAllHooks` keeps the user's hooks (for example a
/// SessionStart hook) from running for a model-help call. A call costs about
/// 6000 input tokens.
fn claude_command(call: &Call) -> Command {
    let mut command = Command::new(&call.binary);
    command
        .arg("-p")
        .args(["--model", &call.model])
        .args(["--output-format", "json"])
        .args(["--max-turns", "1"])
        .args(["--tools", ""])
        .args(["--setting-sources", "user"])
        .args(["--settings", r#"{"disableAllHooks":true}"#])
        .arg("--disable-slash-commands")
        .args(["--system-prompt", crate::assist::prompts::SYSTEM]);
    command
}

/// `codex exec` with a read-only sandbox and JSONL output; the prompt is
/// read from stdin (`-`).
///
/// Model help allows no tools (NV-03). Codex has no switch for that, so the
/// `-c` flags turn the Codex tools off one by one, and [`parse_codex`]
/// refuses a run that used one anyway. A feature name that this Codex does
/// not know is ignored.
fn codex_command(call: &Call) -> Command {
    let mut command = Command::new(&call.binary);
    command
        .arg("exec")
        .args(["--model", &call.model])
        .arg("--json")
        .arg("--skip-git-repo-check")
        .args(["--sandbox", "read-only"]);
    for flag in CODEX_TOOLS_OFF {
        command.args(["-c", flag]);
    }
    command.arg("-");
    command
}

/// The `-c` overrides that turn the Codex tools off (NV-03).
const CODEX_TOOLS_OFF: [&str; 10] = [
    "features.shell_tool=false",
    "features.unified_exec=false",
    "features.apply_patch_freeform=false",
    "features.view_image=false",
    "features.apps=false",
    "features.plugins=false",
    "features.browser_use=false",
    "features.computer_use=false",
    "features.hooks=false",
    r#"web_search="disabled""#,
];

/// The environment of a Claude call: the instance environment plus the
/// output limit of the call.
#[must_use]
pub fn claude_env(base: &ChildEnv, max_output_tokens: u32) -> ChildEnv {
    let mut env = base.clone();
    // A name and a number: `set` cannot refuse them.
    let _ = env.set(
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        max_output_tokens.to_string(),
    );
    env
}

fn count(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

/// The JSON of `claude -p --output-format json`: `result` is the text, and
/// `usage` holds the tokens. The input count includes the cache reads and
/// writes, because they are tokens that the call spent.
pub fn parse_claude(stdout: &str) -> Result<Run, RunError> {
    let value: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| RunError::Parse("claude printed no JSON"))?;
    if value.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(RunError::Reported);
    }
    let text = value
        .get("result")
        .and_then(Value::as_str)
        .ok_or(RunError::Parse("claude gave no `result`"))?;
    let usage = value.get("usage").unwrap_or(&Value::Null);
    Ok(Run {
        text: text.to_owned(),
        input_tokens: count(usage.get("input_tokens"))
            + count(usage.get("cache_creation_input_tokens"))
            + count(usage.get("cache_read_input_tokens")),
        output_tokens: count(usage.get("output_tokens")),
    })
}

/// The JSONL of `codex exec --json`: the last `agent_message` item is the
/// text, and `turn.completed` holds the tokens (the cached part of the
/// input is already inside `input_tokens`). An item of any type other than
/// `agent_message` and `reasoning` is a tool use: the run fails and its text
/// is never returned (NV-03).
pub fn parse_codex(stdout: &str) -> Result<Run, RunError> {
    let mut text = None;
    let mut usage = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some(kind @ ("item.started" | "item.completed")) => {
                let item = value.get("item").unwrap_or(&Value::Null);
                let item_type = item.get("type").and_then(Value::as_str);
                if !matches!(item_type, Some("agent_message" | "reasoning")) {
                    return Err(RunError::ToolUse);
                }
                if kind == "item.completed"
                    && item_type == Some("agent_message")
                    && let Some(t) = item.get("text").and_then(Value::as_str)
                {
                    text = Some(t.to_owned());
                }
            }
            Some("turn.completed") => usage = value.get("usage").cloned(),
            Some("turn.failed" | "error") => return Err(RunError::Reported),
            _ => {}
        }
    }
    let text = text.ok_or(RunError::Parse("codex gave no agent message"))?;
    let usage = usage.ok_or(RunError::Parse("codex gave no token use"))?;
    Ok(Run {
        text,
        input_tokens: count(usage.get("input_tokens")),
        output_tokens: count(usage.get("output_tokens"))
            + count(usage.get("reasoning_output_tokens")),
    })
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// An empty directory with mode 0700 that goes away with the value.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> std::io::Result<Self> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!(
            "fleetdeck-assist-{}-{}-{stamp}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            tracing::debug!(error = %e, "assist directory");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_json_gives_the_text_and_all_tokens() {
        let out = r#"{"type":"result","is_error":false,"result":"Fix the login redirect",
            "usage":{"input_tokens":2,"cache_creation_input_tokens":1935,
            "cache_read_input_tokens":10,"output_tokens":41}}"#;
        let run = parse_claude(out).unwrap();
        assert_eq!(run.text, "Fix the login redirect");
        assert_eq!((run.input_tokens, run.output_tokens), (1947, 41));
    }

    #[test]
    fn a_claude_error_or_a_missing_result_is_an_error() {
        assert!(matches!(
            parse_claude(r#"{"is_error":true,"result":"x"}"#),
            Err(RunError::Reported)
        ));
        assert!(matches!(
            parse_claude(r#"{"usage":{}}"#),
            Err(RunError::Parse(_))
        ));
        assert!(matches!(parse_claude("not json"), Err(RunError::Parse(_))));
    }

    #[test]
    fn codex_jsonl_gives_the_last_message_and_the_tokens() {
        let out = r#"{"type":"thread.started","thread_id":"t"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"first"}}
{"type":"item.completed","item":{"id":"item_1","type":"reasoning","text":"skip"}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"ok"}}
{"type":"turn.completed","usage":{"input_tokens":14206,"cached_input_tokens":12288,"output_tokens":5,"reasoning_output_tokens":2}}
"#;
        let run = parse_codex(out).unwrap();
        assert_eq!(run.text, "ok");
        assert_eq!((run.input_tokens, run.output_tokens), (14206, 7));
    }

    #[test]
    fn codex_output_without_a_message_or_usage_is_an_error() {
        assert!(matches!(
            parse_codex(r#"{"type":"turn.completed","usage":{}}"#),
            Err(RunError::Parse(_))
        ));
        assert!(matches!(
            parse_codex(r#"{"type":"item.completed","item":{"type":"agent_message","text":"x"}}"#),
            Err(RunError::Parse(_))
        ));
        assert!(matches!(
            parse_codex(r#"{"type":"turn.failed","error":{"message":"x"}}"#),
            Err(RunError::Reported)
        ));
    }

    #[test]
    fn a_codex_tool_item_fails_the_run_and_gives_no_text() {
        let out = r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"cat x"}}
{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"cat x"}}
{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"secret"}}
{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}
"#;
        assert!(matches!(parse_codex(out), Err(RunError::ToolUse)));
        for kind in ["file_change", "mcp_tool_call", "web_search", "todo_list"] {
            let out = format!(
                r#"{{"type":"item.completed","item":{{"type":"{kind}"}}}}
{{"type":"item.completed","item":{{"type":"agent_message","text":"x"}}}}
{{"type":"turn.completed","usage":{{}}}}"#
            );
            assert!(
                matches!(parse_codex(&out), Err(RunError::ToolUse)),
                "{kind}"
            );
        }
        // A tool that only started is a tool use too.
        let out = r#"{"type":"item.started","item":{"type":"web_search"}}"#;
        assert!(matches!(parse_codex(out), Err(RunError::ToolUse)));
    }

    fn call_of(driver: Driver, binary: PathBuf) -> Call {
        Call {
            driver,
            binary,
            env: ChildEnv::empty(),
            model: "m".to_owned(),
            prompt: "hello".to_owned(),
            max_output_tokens: 10,
        }
    }

    #[test]
    fn the_codex_command_turns_the_tools_off_before_the_prompt_marker() {
        let command = codex_command(&call_of(Driver::Codex, PathBuf::from("codex")));
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.last().map(String::as_str), Some("-"));
        for flag in [
            "features.shell_tool=false",
            "features.unified_exec=false",
            "features.apply_patch_freeform=false",
            "features.view_image=false",
            "features.apps=false",
            "features.plugins=false",
            "features.browser_use=false",
            "features.computer_use=false",
            "features.hooks=false",
            r#"web_search="disabled""#,
        ] {
            let at = args.iter().position(|a| a == flag).expect(flag);
            assert_eq!(args[at - 1], "-c", "{flag}");
        }
    }

    /// A fake provider CLI: a shell script that runs `body`.
    #[cfg(unix)]
    fn fake_cli(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-cli");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cli_that_prints_more_than_the_limit_is_cut_and_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = fake_cli(tmp.path(), "head -c 307200 /dev/zero | tr '\\0' 'a'");
        let err = CliRunner
            .run(call_of(Driver::Claude, cli))
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::TooLarge), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cli_that_prints_small_json_gives_the_text() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = fake_cli(
            tmp.path(),
            r#"cat > /dev/null
printf '%s' '{"is_error":false,"result":"A title","usage":{"input_tokens":3,"output_tokens":4}}'"#,
        );
        let run = CliRunner.run(call_of(Driver::Claude, cli)).await.unwrap();
        assert_eq!(run.text, "A title");
        assert_eq!((run.input_tokens, run.output_tokens), (3, 4));
    }

    #[test]
    fn the_claude_call_limits_its_output_by_environment() {
        let env = claude_env(&ChildEnv::empty(), 123);
        assert_eq!(
            env.get("CLAUDE_CODE_MAX_OUTPUT_TOKENS")
                .and_then(|v| v.to_str()),
            Some("123")
        );
    }
}
