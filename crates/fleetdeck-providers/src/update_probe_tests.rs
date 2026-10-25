//! Tests of the update probe. The fake CLIs are `#!/bin/sh` scripts.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::event::ProviderKind;

use super::*;

const LIMIT: Duration = Duration::from_secs(10);

fn env() -> ChildEnv {
    let mut env = ChildEnv::empty();
    env.set("PATH", "/usr/bin:/bin")
        .expect("PATH is a valid name");
    env
}

/// Writes an executable script and returns its path.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// A Claude that prints its version and answers `initialize` with `reply`.
fn claude_script(dir: &Path, reply: &str) -> PathBuf {
    script(
        dir,
        "claude",
        &format!(
            "if [ \"$1\" = \"--version\" ]; then echo '2.1.300 (Claude Code)'; exit 0; fi\n\
             read line\n\
             echo '{reply}'\n\
             cat >/dev/null"
        ),
    )
}

async fn probe(kind: ProviderKind, program: &Path, dir: &Path, limit: Duration) -> UpdateProbe {
    check(kind, program, &env(), dir, limit).await
}

const MODELS_OK: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"probe-init","response":{"models":[{"value":"default"}]}}}"#;

#[tokio::test]
async fn a_claude_that_answers_initialize_with_models_passes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let program = claude_script(dir.path(), MODELS_OK);
    let result = probe(ProviderKind::Claude, &program, dir.path(), LIMIT).await;
    assert_eq!(result.version.as_deref(), Some("2.1.300"));
    assert_eq!(result.handshake, Ok(()));
}

#[tokio::test]
async fn a_claude_error_answer_fails_with_error_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reply = r#"{"type":"control_response","response":{"subtype":"error","request_id":"probe-init","error":"no"}}"#;
    let program = claude_script(dir.path(), reply);
    let result = probe(ProviderKind::Claude, &program, dir.path(), LIMIT).await;
    assert_eq!(result.handshake, Err("error_answer"));
}

#[tokio::test]
async fn a_claude_answer_without_models_fails_with_no_models() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reply = r#"{"type":"control_response","response":{"subtype":"success","request_id":"probe-init","response":{}}}"#;
    let program = claude_script(dir.path(), reply);
    let result = probe(ProviderKind::Claude, &program, dir.path(), LIMIT).await;
    assert_eq!(result.handshake, Err("no_models"));
}

#[tokio::test]
async fn a_silent_claude_fails_with_no_answer_within_the_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let program = script(
        dir.path(),
        "claude",
        "if [ \"$1\" = \"--version\" ]; then echo '2.1.300'; exit 0; fi\nsleep 30",
    );
    let started = std::time::Instant::now();
    let result = probe(
        ProviderKind::Claude,
        &program,
        dir.path(),
        Duration::from_millis(300),
    )
    .await;
    assert_eq!(result.handshake, Err("no_answer"));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_missing_program_fails_with_no_start_and_no_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let program = dir.path().join("absent");
    let result = probe(ProviderKind::Claude, &program, dir.path(), LIMIT).await;
    assert_eq!(result.version, None);
    assert_eq!(result.handshake, Err("no_start"));
}

#[tokio::test]
async fn the_claude_probe_passes_the_adapter_launch_arguments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let argv = dir.path().join("argv");
    let program = script(
        dir.path(),
        "claude",
        &format!(
            "if [ \"$1\" = \"--version\" ]; then echo '2.1.300'; exit 0; fi\n\
             for a in \"$@\"; do echo \"$a\"; done > '{}'\n\
             read line\n\
             echo '{MODELS_OK}'\n\
             cat >/dev/null",
            argv.display()
        ),
    );
    let result = probe(ProviderKind::Claude, &program, dir.path(), LIMIT).await;
    assert_eq!(result.handshake, Ok(()));
    let written = std::fs::read_to_string(&argv).expect("argv file");
    let got: Vec<&str> = written.lines().collect();
    let want = claude::cli_args(&Launch::default(), &claude::capabilities(false));
    assert_eq!(got, want.iter().map(String::as_str).collect::<Vec<_>>());
}

fn codex_script(dir: &Path, init: &str, account: &str) -> PathBuf {
    script(
        dir,
        "codex",
        &format!(
            "if [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.161.0'; exit 0; fi\n\
             read a\nread b\nread c\n\
             echo '{init}'\n\
             echo '{account}'\n\
             cat >/dev/null"
        ),
    )
}

#[tokio::test]
async fn a_codex_that_answers_initialize_and_account_read_passes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let program = codex_script(
        dir.path(),
        r#"{"id":"probe-init","result":{"userAgent":"codex"}}"#,
        r#"{"id":"probe-account","result":{"account":null,"requiresOpenaiAuth":true}}"#,
    );
    let result = probe(ProviderKind::Codex, &program, dir.path(), LIMIT).await;
    assert_eq!(result.version.as_deref(), Some("0.161.0"));
    assert_eq!(result.handshake, Ok(()));
}

#[tokio::test]
async fn a_codex_error_on_initialize_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let program = codex_script(
        dir.path(),
        r#"{"id":"probe-init","error":{"code":-32600,"message":"no"}}"#,
        r#"{"id":"probe-account","result":{}}"#,
    );
    let result = probe(ProviderKind::Codex, &program, dir.path(), LIMIT).await;
    assert_eq!(result.handshake, Err("error_answer"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_background_child_of_the_probe_is_killed_with_its_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("pid");
    let program = script(
        dir.path(),
        "claude",
        &format!(
            "if [ \"$1\" = \"--version\" ]; then echo '2.1.300'; exit 0; fi\n\
             sleep 60 &\n\
             echo $! > '{}'\n\
             read line\n\
             echo '{MODELS_OK}'\n\
             cat >/dev/null",
            pid_file.display()
        ),
    );
    let result = probe(ProviderKind::Claude, &program, dir.path(), LIMIT).await;
    assert_eq!(result.handshake, Ok(()));
    let pid = std::fs::read_to_string(&pid_file)
        .expect("pid file")
        .trim()
        .to_owned();
    // The killed process may stay a zombie for a moment: poll `/proc`.
    let stat = PathBuf::from(format!("/proc/{pid}/stat"));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let gone = std::fs::read_to_string(&stat).map_or(true, |s| {
            s.split(") ").nth(1).is_some_and(|r| r.starts_with('Z'))
        });
        if gone {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the background child {pid} still runs"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
