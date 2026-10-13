//! The CLI device login, the token login and the status of the CLIs, with
//! fake `gh` and `glab` programs and the recorded output of `gh` 2.102 and
//! `glab` 1.120.

use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::accounts::{AccountKind, AccountMethod, AccountStatus, ForgeLoginState};

use super::forge::parse_cli_account_id;
use super::forge_parse::{Parsed, parse_gh_hosts, parse_glab_status, parse_login};
use super::test_support::{log, programs, script, service};
use super::{Accounts, codes};
use crate::integrations::exec::Programs;

/// `gh auth login --web` without a TTY (recorded; the clipboard line).
const GH_CLIPBOARD: &str = "\n! One-time code (F88A-39E2) copied to clipboard\nOpen this URL to continue in your web browser: https://github.com/login/device\n";
/// The same without a clipboard.
const GH_PLAIN: &str = "\n! First copy your one-time code: F88A-39E2\nOpen this URL to continue in your web browser: https://github.com/login/device\n";
/// `glab auth login --device` (recorded).
const GLAB_DEVICE: &str = "WARNING: The operating system keyring is unavailable. Storing credentials as plaintext in the configuration file.\n- Signing into gitlab.com\n\nFirst copy your one-time code: TDIZZI2T\nThen open this URL on any device to authorize: https://gitlab.com/oauth/device\n\nWaiting for authorization...\n";
/// `glab auth login --device` on a server with no OAuth application
/// (recorded).
const GLAB_NO_CLIENT: &str = "WARNING: The operating system keyring is unavailable. Storing credentials as plaintext in the configuration file.\n- Signing into example.com\n          \n   ERROR  \n          \n  Set 'client_id' first with `glab config set client_id <client_id> -g --host example.com`.\n";
/// `gh auth status --json hosts` (recorded).
const GH_STATUS: &str = r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"ana","tokenSource":"/home/ana/.config/gh/hosts.yml","scopes":"repo","gitProtocol":"https"}]}}"#;
/// `glab auth status --all` (recorded; `glab` writes it to stderr).
const GLAB_STATUS: &str = "gitlab.com\n  \u{2713} Logged in to gitlab.com as ana.b (/home/ana/.config/glab-cli/config.yml)\n  \u{2713} Git operations for gitlab.com configured to use https protocol.\n  \u{2713} REST API Endpoint: https://gitlab.com/api/v4/\n  ! To store this token more securely, run glab auth login --hostname gitlab.com to move it into the operating system keyring.\ngit.corp.example\n  x git.corp.example: API call failed: GET https://git.corp.example/api/v4/user: 401 {message: 401 Unauthorized}\n  \u{2713} REST API Endpoint: https://git.corp.example/api/v4/\n  ! No token found (checked config file, keyring, and environment variables).\n";

/// A fake `gh`: `login_body` runs for `auth login`; `auth status` prints
/// the recorded JSON; every call appends its arguments to `argv.log` and
/// its stdin to `stdin.log`, and `auth login` dumps the environment names
/// to `env.log`.
fn gh(dir: &Path, login_body: &str) -> std::path::PathBuf {
    script(
        dir,
        "gh",
        &format!(
            "D='{d}'\nprintf '%s\\n' \"$*\" >> \"$D/argv.log\"\ncase \"$*\" in\n*'auth status'*) cat <<'EOF_S'\n{GH_STATUS}\nEOF_S\nexit 0;;\n*'auth logout'*) exit 0;;\n*'--with-token'*) cat >> \"$D/stdin.log\"; exit 0;;\nesac\nenv >> \"$D/env.log\"\nH=\"$4\"\n{login_body}",
            d = dir.display()
        ),
    )
}

fn glab(dir: &Path, login_body: &str) -> std::path::PathBuf {
    script(
        dir,
        "glab",
        &format!(
            "D='{d}'\nprintf '%s\\n' \"$*\" >> \"$D/argv.log\"\ncase \"$*\" in\n*'auth status'*) cat >&2 <<'EOF_S'\n{GLAB_STATUS}\nEOF_S\nexit 0;;\n*'auth logout'*) exit 0;;\n*'--stdin'*) cat >> \"$D/stdin.log\"; exit 0;;\nesac\nH=\"$4\"\n{login_body}",
            d = dir.display()
        ),
    )
}

fn with(tmp: &Path, gh_login: &str, glab_login: &str) -> Accounts {
    let mut p: Programs = programs(tmp);
    p.gh = gh(&tmp.join("gh"), gh_login);
    p.glab = glab(&tmp.join("glab"), glab_login);
    service(tmp, p)
}

/// A `gh auth login` that prints the code and the page of its host to
/// stderr, then waits.
const GH_WAIT: &str = "printf '\\n! First copy your one-time code: F88A-39E2\\nOpen this URL to continue in your web browser: https://%s/login/device\\n' \"$H\" >&2\nsleep 30";

async fn until_state(a: &Accounts, id: &str, want: impl Fn(&ForgeLoginState) -> bool) {
    for _ in 0..100 {
        if want(&a.forge_login_status(id)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the login stays {:?}", a.forge_login_status(id));
}

#[tokio::test]
async fn a_github_device_login_gives_the_code_and_the_page_and_runs_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let a = with(tmp.path(), GH_WAIT, "exit 1");
    let login = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap();
    assert_eq!(login.code, "F88A-39E2");
    assert_eq!(login.url, "https://github.com/login/device");
    assert!(login.expires_in > 800, "{login:?}");
    let gh = tmp.path().join("gh");
    assert_eq!(
        log(&gh, "argv.log").lines().next().unwrap(),
        "auth login --hostname github.com --web --git-protocol https --skip-ssh-key"
    );
    let env = log(&gh, "env.log");
    assert!(env.lines().any(|l| l == "BROWSER=true"), "{env}");
    assert!(env.lines().any(|l| l == "GH_PROMPT_DISABLED=1"), "{env}");
    for name in super::forge::TOKEN_VARS {
        assert!(
            !env.lines().any(|l| l.starts_with(&format!("{name}="))),
            "{name} reached the child"
        );
    }
    // The same host and kind: the same pending login.
    let again = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap();
    assert_eq!(again.login_id, login.login_id);
    assert_eq!(
        log(&gh, "argv.log")
            .lines()
            .filter(|l| l.starts_with("auth login"))
            .count(),
        1
    );
    assert_eq!(
        a.forge_login_status(&login.login_id),
        ForgeLoginState::Pending
    );
    assert_eq!(a.forge_login_status("unknown"), ForgeLoginState::Expired);
}

#[tokio::test]
async fn a_login_that_ends_well_is_done_with_the_user_of_the_status() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "printf '! One-time code (F88A-39E2) copied to clipboard\\nOpen this URL to continue in your web browser: https://%s/login/device\\n' \"$H\" >&2\nsleep 1\nexit 0";
    let a = with(tmp.path(), body, "exit 1");
    let login = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap();
    until_state(&a, &login.login_id, |s| *s != ForgeLoginState::Pending).await;
    assert_eq!(
        a.forge_login_status(&login.login_id),
        ForgeLoginState::Done { user: "ana".into() }
    );
}

#[tokio::test]
async fn a_login_that_fails_is_failed_with_a_fixed_code() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "printf '! First copy your one-time code: F88A-39E2\\nOpen this URL to continue in your web browser: https://%s/login/device\\n' \"$H\" >&2\necho 'secret-looking-text ghp_canary' >&2\nsleep 1\nexit 1";
    let a = with(tmp.path(), body, "exit 1");
    let login = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap();
    until_state(&a, &login.login_id, |s| *s != ForgeLoginState::Pending).await;
    assert_eq!(
        a.forge_login_status(&login.login_id),
        ForgeLoginState::Failed {
            code: "login_failed".into()
        }
    );
}

#[tokio::test]
async fn a_gitlab_device_login_uses_the_recorded_glab_output() {
    let tmp = tempfile::tempdir().unwrap();
    let body = format!("cat >&2 <<'EOF_G'\n{GLAB_DEVICE}EOF_G\nsleep 30");
    let a = with(tmp.path(), "exit 1", &body);
    let login = a
        .forge_login(AccountKind::Gitlab, "gitlab.com")
        .await
        .unwrap();
    assert_eq!(login.code, "TDIZZI2T");
    assert_eq!(login.url, "https://gitlab.com/oauth/device");
    assert_eq!(
        log(&tmp.path().join("glab"), "argv.log")
            .lines()
            .next()
            .unwrap(),
        "auth login --hostname gitlab.com --device"
    );
}

#[tokio::test]
async fn a_page_on_another_host_is_unexpected_cli_output() {
    let tmp = tempfile::tempdir().unwrap();
    for url in [
        "https://evil.example/login/device",
        "https://github.com.evil.example/login/device",
        "https://user@github.com/login/device",
        "http://github.com/login/device",
        "https://github.com/login/device?next=x",
        "https://github.com/other",
    ] {
        let body = format!(
            "printf '! First copy your one-time code: F88A-39E2\\nOpen this URL to continue in your web browser: {url}\\n' >&2\nsleep 30"
        );
        let a = with(tmp.path(), &body, "exit 1");
        let err = a
            .forge_login(AccountKind::Github, "github.com")
            .await
            .unwrap_err();
        assert_eq!(err.code, codes::UNEXPECTED_CLI_OUTPUT, "{url}");
        assert!(!err.message.contains("evil"), "{err:?}");
    }
}

#[tokio::test]
async fn a_bad_code_is_unexpected_cli_output() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "printf 'First copy your one-time code: f88a-39e2\\nOpen this URL: https://github.com/login/device\\n' >&2\nsleep 30";
    let a = with(tmp.path(), body, "exit 1");
    let err = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap_err();
    assert_eq!(err.code, codes::UNEXPECTED_CLI_OUTPUT);
}

#[tokio::test]
async fn output_over_64_kib_without_a_code_is_refused_and_the_rest_is_drained() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "head -c 200000 /dev/zero | tr '\\0' x >&2\nexit 0";
    let a = with(tmp.path(), body, "exit 1");
    let err = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap_err();
    assert_eq!(err.code, codes::UNEXPECTED_CLI_OUTPUT);

    // A child that writes far more than a pipe holds is not blocked: the
    // station drains both streams.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("gh");
    let body = format!(
        "printf '! First copy your one-time code: F88A-39E2\\nOpen this URL to continue in your web browser: https://%s/login/device\\n' \"$H\" >&2\nhead -c 400000 /dev/zero | tr '\\0' x >&2\nhead -c 400000 /dev/zero | tr '\\0' y\necho written > '{}/marker'\nsleep 30",
        d.display()
    );
    let a = with(tmp.path(), &body, "exit 1");
    a.forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap();
    for _ in 0..100 {
        if d.join("marker").exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the child is blocked on a full pipe");
}

#[tokio::test]
async fn a_token_in_the_station_environment_refuses_the_login_before_any_child_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = with(tmp.path(), GH_WAIT, "exit 1");
    a.env_probe = |name| name == "GH_TOKEN";
    assert_eq!(a.env_tokens(), ["GH_TOKEN"]);
    let err = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap_err();
    assert_eq!(err.code, codes::ENV_TOKEN_PRESENT);
    let err = a
        .forge_token_login(AccountKind::Github, "github.com", "ghp_canary")
        .await
        .unwrap_err();
    assert_eq!(err.code, codes::ENV_TOKEN_PRESENT);
    assert!(log(&tmp.path().join("gh"), "argv.log").is_empty());
}

#[tokio::test]
async fn a_server_without_device_login_is_device_flow_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let body = format!("cat >&2 <<'EOF_G'\n{GLAB_NO_CLIENT}EOF_G\nexit 1");
    let a = with(tmp.path(), "exit 1", &body);
    let err = a
        .forge_login(AccountKind::Gitlab, "example.com")
        .await
        .unwrap_err();
    assert_eq!(err.code, codes::DEVICE_FLOW_UNSUPPORTED);
    let body = "echo 'the device flow is not supported by this GitLab' >&2\nexit 1";
    let a = with(tmp.path(), "exit 1", body);
    let err = a
        .forge_login(AccountKind::Gitlab, "gitlab.example.org")
        .await
        .unwrap_err();
    assert_eq!(err.code, codes::DEVICE_FLOW_UNSUPPORTED);
}

#[tokio::test]
async fn the_process_group_is_killed_at_the_time_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let pids = tmp.path().join("gh");
    let body = format!(
        "printf '! First copy your one-time code: F88A-39E2\\nOpen this URL to continue in your web browser: https://%s/login/device\\n' \"$H\" >&2\n(sleep 60 & echo $! > '{}/child.pid'; wait) &\necho $$ > '{}/leader.pid'\nsleep 60",
        pids.display(),
        pids.display()
    );
    let mut a = with(tmp.path(), &body, "exit 1");
    a.limits.login_ttl = Duration::from_millis(600);
    let login = a
        .forge_login(AccountKind::Github, "github.com")
        .await
        .unwrap();
    assert_eq!(
        a.forge_login_status(&login.login_id),
        ForgeLoginState::Pending
    );
    until_state(&a, &login.login_id, |s| *s == ForgeLoginState::Expired).await;
    let pid = std::fs::read_to_string(pids.join("child.pid"))
        .unwrap()
        .trim()
        .to_owned();
    for _ in 0..100 {
        // A killed process that nobody has reaped yet is a zombie.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if stat.is_empty() || stat.contains(") Z") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the child of the login still runs");
}

#[tokio::test]
async fn a_fourth_login_is_login_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let a = with(tmp.path(), GH_WAIT, "exit 1");
    for host in ["github.com", "ghe.one.example", "ghe.two.example"] {
        a.forge_login(AccountKind::Github, host).await.unwrap();
    }
    let err = a
        .forge_login(AccountKind::Github, "ghe.three.example")
        .await
        .unwrap_err();
    assert_eq!(err.code, codes::LOGIN_BUSY);
    // A running login of a known host still answers.
    assert!(
        a.forge_login(AccountKind::Github, "github.com")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_host_must_be_a_bare_host_name_with_a_dot() {
    let tmp = tempfile::tempdir().unwrap();
    let a = with(tmp.path(), GH_WAIT, "exit 1");
    for bad in [
        "localhost",
        "Github.com",
        "github.com:443",
        "github.com/x",
        "-a.com",
        "a..com",
        "",
        "a b.com",
    ] {
        let err = a.forge_login(AccountKind::Github, bad).await.unwrap_err();
        assert_eq!(err.code, "invalid_params", "{bad}");
    }
    let err = a
        .forge_login(AccountKind::Jira, "github.com")
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
    assert!(log(&tmp.path().join("gh"), "argv.log").is_empty());
}

#[tokio::test]
async fn a_forge_token_goes_to_the_cli_on_stdin_never_in_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let a = with(tmp.path(), "exit 1", "exit 1");
    let view = a
        .forge_token_login(AccountKind::Github, "github.com", "ghp_canary_token")
        .await
        .unwrap();
    assert_eq!(
        (view.account_id.as_str(), view.user.as_deref(), view.method),
        ("cli-github-github.com", Some("ana"), AccountMethod::Token)
    );
    let gh = tmp.path().join("gh");
    assert_eq!(
        log(&gh, "argv.log").lines().next().unwrap(),
        "auth login --hostname github.com --with-token"
    );
    assert!(!log(&gh, "argv.log").contains("canary"));
    assert!(log(&gh, "stdin.log").contains("ghp_canary_token"));
    let view = a
        .forge_token_login(AccountKind::Gitlab, "gitlab.com", "glpat-canary-token")
        .await
        .unwrap();
    assert_eq!(view.user.as_deref(), Some("ana.b"));
    let glab = tmp.path().join("glab");
    assert!(
        log(&glab, "argv.log")
            .lines()
            .next()
            .unwrap()
            .ends_with("auth login --hostname gitlab.com --stdin")
    );
    assert!(!log(&glab, "argv.log").contains("canary"));
    assert!(log(&glab, "stdin.log").contains("glpat-canary-token"));
}

#[tokio::test]
async fn a_logout_runs_the_cli_logout_for_the_host() {
    let tmp = tempfile::tempdir().unwrap();
    let a = with(tmp.path(), "exit 1", "exit 1");
    a.call(
        "accounts.logout",
        serde_json::json!({"account_id": "cli-github-ghe.example.org"}),
        &[],
    )
    .await
    .unwrap();
    assert!(
        log(&tmp.path().join("gh"), "argv.log")
            .lines()
            .any(|l| l == "auth logout --hostname ghe.example.org")
    );
    assert_eq!(
        parse_cli_account_id("cli-gitlab-git.corp.example"),
        Some((AccountKind::Gitlab, "git.corp.example".to_owned()))
    );
    assert_eq!(parse_cli_account_id("cli-github-../x"), None);
    let err = a
        .call(
            "accounts.logout",
            serde_json::json!({"account_id": "nope"}),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
}

#[tokio::test]
async fn the_list_shows_every_host_that_the_clis_report() {
    let tmp = tempfile::tempdir().unwrap();
    let a = with(tmp.path(), "exit 1", "exit 1");
    let accounts = a.cli_accounts().await;
    let find = |id: &str| accounts.iter().find(|v| v.account_id == id).unwrap();
    let gh = find("cli-github-github.com");
    assert_eq!(
        (gh.user.as_deref(), gh.status, gh.method),
        (Some("ana"), AccountStatus::Ok, AccountMethod::Cli)
    );
    let gl = find("cli-gitlab-gitlab.com");
    assert_eq!(
        (gl.user.as_deref(), gl.status),
        (Some("ana.b"), AccountStatus::Ok)
    );
    let corp = find("cli-gitlab-git.corp.example");
    assert_eq!(
        (corp.user.as_deref(), corp.status),
        (None, AccountStatus::NeedsLogin)
    );
}

#[tokio::test]
async fn a_missing_cli_shows_an_error_row_for_its_default_host() {
    let tmp = tempfile::tempdir().unwrap();
    let mut p = programs(tmp.path());
    p.gh = tmp.path().join("no-such-gh");
    p.glab = tmp.path().join("no-such-glab");
    let a = service(tmp.path(), p);
    let accounts = a.cli_accounts().await;
    assert_eq!(accounts.len(), 2);
    assert!(accounts.iter().all(|v| v.status == AccountStatus::Error));
    assert!(accounts[0].message.contains("not installed"));
}

#[test]
fn the_parsers_read_the_recorded_output() {
    for text in [GH_CLIPBOARD, GH_PLAIN] {
        assert_eq!(
            parse_login(AccountKind::Github, "github.com", text),
            Parsed::Found("F88A-39E2".into(), "https://github.com/login/device".into()),
            "{text}"
        );
    }
    assert_eq!(
        parse_login(AccountKind::Gitlab, "gitlab.com", GLAB_DEVICE),
        Parsed::Found("TDIZZI2T".into(), "https://gitlab.com/oauth/device".into())
    );
    // Half of the output: wait.
    assert_eq!(
        parse_login(
            AccountKind::Github,
            "github.com",
            "! First copy your one-time code: F88A-39E2\n"
        ),
        Parsed::Wait
    );
    // A glab code is not a gh code.
    assert_eq!(
        parse_login(AccountKind::Github, "github.com", GLAB_DEVICE),
        Parsed::Bad
    );
    let hosts = parse_gh_hosts(GH_STATUS);
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].user.as_deref(), Some("ana"));
    assert!(parse_gh_hosts("You are not logged into any GitHub hosts.").is_empty());
    assert!(parse_gh_hosts(r#"{"hosts":{}}"#).is_empty());
    let hosts = parse_glab_status(GLAB_STATUS);
    assert_eq!(hosts.len(), 2);
    assert_eq!(
        (
            hosts[0].host.as_str(),
            hosts[0].user.as_deref(),
            hosts[0].ok
        ),
        ("gitlab.com", Some("ana.b"), true)
    );
    assert_eq!(
        (hosts[1].host.as_str(), hosts[1].ok),
        ("git.corp.example", false)
    );
}
