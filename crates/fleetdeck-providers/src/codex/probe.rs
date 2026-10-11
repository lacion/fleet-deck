//! The Codex probe (L11, trap 9): the version from `codex --version` and the
//! sign-in state from `account/read` of a short `codex app-server` process
//! that opens no thread. Every unclear answer is [`AuthState::Unknown`].

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{ClientInfo, app_server_args, rpc};
use crate::adapter::AuthState;

/// The request id of `account/read` in the probe process.
const ACCOUNT_ID: &str = "probe-account";

/// The version from `codex --version` (`codex-cli 0.160.0` → `0.160.0`).
#[must_use]
pub fn parse_version(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?.trim();
    let version = line.strip_prefix("codex-cli ").unwrap_or(line).trim();
    version
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
        .then(|| version.to_owned())
}

/// The sign-in state of a `GetAccountResponse`: an account is signed in with
/// its `type` as the method; no account and `requiresOpenaiAuth: true` is
/// signed out; every other answer is unknown.
#[must_use]
pub fn parse_account(result: &Value) -> AuthState {
    match result.get("account") {
        Some(Value::Object(account)) => AuthState::SignedIn {
            method: account
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some(Value::Null) if result.get("requiresOpenaiAuth") == Some(&Value::Bool(true)) => {
            AuthState::SignedOut
        }
        _ => AuthState::Unknown,
    }
}

/// Runs `account/read` in a short app-server process. Any failure, error
/// reply or timeout is [`AuthState::Unknown`]; the process is killed at the
/// end.
pub async fn account(
    program: &Path,
    env: &ChildEnv,
    client: &ClientInfo,
    limit: Duration,
) -> AuthState {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(app_server_args())
        .env_clear()
        .envs(env.iter())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = cmd.spawn() else {
        return AuthState::Unknown;
    };
    let auth = tokio::time::timeout(limit, converse(&mut child, client))
        .await
        .ok()
        .flatten()
        .unwrap_or(AuthState::Unknown);
    let _ = child.kill().await;
    auth
}

/// Writes the handshake and `account/read`; reads until the reply.
async fn converse(child: &mut tokio::process::Child, client: &ClientInfo) -> Option<AuthState> {
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let frames = [
        rpc::request(
            "probe-init",
            "initialize",
            json!({
                "clientInfo": {"name": client.name, "version": client.version},
                "capabilities": {"experimentalApi": true},
            }),
        ),
        rpc::notification("initialized"),
        rpc::request(ACCOUNT_ID, "account/read", json!({"refreshToken": false})),
    ];
    for frame in frames {
        let mut line = frame.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).await.ok()?;
    }
    stdin.flush().await.ok()?;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if frame.get("id").and_then(Value::as_str) != Some(ACCOUNT_ID) {
            continue;
        }
        return Some(
            frame
                .get("result")
                .map_or(AuthState::Unknown, parse_account),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_of_codex_cli() {
        assert_eq!(
            parse_version("codex-cli 0.160.0\n").as_deref(),
            Some("0.160.0")
        );
        assert_eq!(parse_version("0.161.1").as_deref(), Some("0.161.1"));
        assert_eq!(parse_version("error: no such command"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn account_answers() {
        assert_eq!(
            parse_account(
                &json!({"account": {"type": "chatgpt", "email": "a@b", "planType": "plus"}, "requiresOpenaiAuth": true})
            ),
            AuthState::SignedIn {
                method: Some("chatgpt".into())
            }
        );
        assert_eq!(
            parse_account(&json!({"account": {"type": "apiKey"}, "requiresOpenaiAuth": true})),
            AuthState::SignedIn {
                method: Some("apiKey".into())
            }
        );
        assert_eq!(
            parse_account(&json!({"account": null, "requiresOpenaiAuth": true})),
            AuthState::SignedOut
        );
        // No account but no OpenAI auth needed (a custom provider): unknown.
        assert_eq!(
            parse_account(&json!({"account": null, "requiresOpenaiAuth": false})),
            AuthState::Unknown
        );
        assert_eq!(parse_account(&json!({})), AuthState::Unknown);
        assert_eq!(parse_account(&json!({"account": "x"})), AuthState::Unknown);
    }
}
