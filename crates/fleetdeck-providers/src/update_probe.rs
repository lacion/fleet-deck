//! The update probe (B28, design `cli-update.md` §3.6.1): `--version`, then
//! the protocol handshake of the adapter, on one CLI. It makes no model call,
//! opens no thread and sends no prompt.
//!
//! Every child runs in its own process group. When [`check`] ends (pass, fail
//! or limit), it sends SIGKILL to the group and waits for the child. The
//! caller puts the attempt marker into `env` and runs the fence afterwards.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::event::ProviderKind;
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::claude::{self, Launch};
use crate::codex::{self, ClientInfo, rpc};

/// The request id of the Claude `initialize` request.
const CLAUDE_INIT_ID: &str = "probe-init";
/// The request id of the Codex `initialize` request.
const CODEX_INIT_ID: &str = "probe-init";
/// The request id of the Codex `account/read` request.
const CODEX_ACCOUNT_ID: &str = "probe-account";

/// The result of the update probe of one CLI (B28 §3.6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProbe {
    pub version: Option<String>,
    /// `Ok` when the CLI answered the protocol handshake with the adapter's
    /// launch arguments; `Err` with a short fixed reason.
    pub handshake: Result<(), &'static str>,
}

/// `--version`, then the handshake. No model call, no thread, no prompt.
/// `cwd` is an empty private folder. Each step is bounded by `limit`; the
/// child is killed at the end.
pub async fn check(
    kind: ProviderKind,
    program: &Path,
    env: &ChildEnv,
    cwd: &Path,
    limit: Duration,
) -> UpdateProbe {
    let version = version_of(kind, program, env, cwd, limit).await;
    let handshake = match kind {
        ProviderKind::Claude => claude_handshake(program, env, cwd, limit).await,
        ProviderKind::Codex => codex_handshake(program, env, cwd, limit).await,
        ProviderKind::Unknown => Err("no_start"),
    };
    UpdateProbe { version, handshake }
}

/// A command with its own process group, no inherited environment and no
/// stderr.
fn command(program: &Path, args: &[String], env: &ChildEnv, cwd: &Path, stdin: Stdio) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter())
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    cmd
}

/// SIGKILL to the process group whose leader is `pid`. A group that is gone
/// is fine.
fn kill_group(pid: Option<u32>) {
    let Some(pid) = pid
        .and_then(|p| i32::try_from(p).ok())
        .filter(|p| *p > 1)
        .and_then(Pid::from_raw)
    else {
        return;
    };
    let _ = kill_process_group(pid, Signal::KILL);
}

/// Kills the group of `child` and waits for the child.
async fn finish(mut child: Child) {
    kill_group(child.id());
    let _ = child.wait().await;
}

/// The version from `<program> --version`. `None` when it does not start,
/// ends too late or prints no version.
async fn version_of(
    kind: ProviderKind,
    program: &Path,
    env: &ChildEnv,
    cwd: &Path,
    limit: Duration,
) -> Option<String> {
    let args = ["--version".to_owned()];
    let child = command(program, &args, env, cwd, Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();
    let output = tokio::time::timeout(limit, child.wait_with_output()).await;
    kill_group(pid);
    let output = output.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    match kind {
        ProviderKind::Claude => claude::parse_version(&text),
        ProviderKind::Codex => codex::parse_version(&text),
        ProviderKind::Unknown => None,
    }
}

/// The Claude handshake: `initialize`, answered with the model list.
async fn claude_handshake(
    program: &Path,
    env: &ChildEnv,
    cwd: &Path,
    limit: Duration,
) -> Result<(), &'static str> {
    let args = claude::cli_args(&Launch::default(), &claude::capabilities(false));
    let mut child = command(program, &args, env, cwd, Stdio::piped())
        .spawn()
        .map_err(|_| "no_start")?;
    let result = match tokio::time::timeout(limit, claude_converse(&mut child)).await {
        Ok(result) => result,
        Err(_) => Err("no_answer"),
    };
    finish(child).await;
    result
}

async fn claude_converse(child: &mut Child) -> Result<(), &'static str> {
    let mut stdin = child.stdin.take().ok_or("no_start")?;
    let stdout = child.stdout.take().ok_or("no_start")?;
    let mut line = claude::control::initialize(CLAUDE_INIT_ID).to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|_| "no_answer")?;
    stdin.flush().await.map_err(|_| "no_answer")?;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(text)) = lines.next_line().await {
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if frame.get("type").and_then(Value::as_str) != Some("control_response") {
            continue;
        }
        let response = frame.get("response").unwrap_or(&Value::Null);
        if response.get("request_id").and_then(Value::as_str) != Some(CLAUDE_INIT_ID) {
            continue;
        }
        if response.get("subtype").and_then(Value::as_str) != Some("success") {
            return Err("error_answer");
        }
        let models = response.get("response").and_then(|r| r.get("models"));
        return if models.is_some_and(Value::is_array) {
            Ok(())
        } else {
            Err("no_models")
        };
    }
    Err("no_answer")
}

/// The Codex handshake: `initialize`, `initialized` and `account/read`.
async fn codex_handshake(
    program: &Path,
    env: &ChildEnv,
    cwd: &Path,
    limit: Duration,
) -> Result<(), &'static str> {
    let args = codex::app_server_args();
    let mut child = command(program, &args, env, cwd, Stdio::piped())
        .spawn()
        .map_err(|_| "no_start")?;
    let result = match tokio::time::timeout(limit, codex_converse(&mut child)).await {
        Ok(result) => result,
        Err(_) => Err("no_answer"),
    };
    finish(child).await;
    result
}

async fn codex_converse(child: &mut Child) -> Result<(), &'static str> {
    let mut stdin = child.stdin.take().ok_or("no_start")?;
    let stdout = child.stdout.take().ok_or("no_start")?;
    let client = ClientInfo::fleetdeck();
    let frames = [
        rpc::request(
            CODEX_INIT_ID,
            "initialize",
            json!({
                "clientInfo": {"name": client.name, "version": client.version},
                "capabilities": {"experimentalApi": true},
            }),
        ),
        rpc::notification("initialized"),
        rpc::request(
            CODEX_ACCOUNT_ID,
            "account/read",
            json!({"refreshToken": false}),
        ),
    ];
    for frame in frames {
        let mut line = frame.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|_| "no_answer")?;
    }
    stdin.flush().await.map_err(|_| "no_answer")?;
    let mut initialized = false;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(text)) = lines.next_line().await {
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let answered = |id: &str| frame.get("id").and_then(Value::as_str) == Some(id);
        if answered(CODEX_INIT_ID) {
            if frame.get("result").is_none() {
                return Err("error_answer");
            }
            initialized = true;
        } else if answered(CODEX_ACCOUNT_ID) {
            return if initialized && frame.get("result").is_some() {
                Ok(())
            } else {
                Err("error_answer")
            };
        }
    }
    Err("no_answer")
}

#[cfg(test)]
#[path = "update_probe_tests.rs"]
mod tests;
