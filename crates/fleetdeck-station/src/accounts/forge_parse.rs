//! What the station reads from the `gh` and `glab` CLIs: the code and the
//! page of a device login, and the login status of each host (DL-173,
//! design §2.2). The patterns are pinned to the recorded output of `gh`
//! 2.102 and `glab` 1.120 (see the tests); anything else is refused.

use fleetdeck_proto::accounts::AccountKind;
use serde_json::Value;

use super::forge::{Runner, cli_name};
use crate::integrations::exec::{self, Spec};

/// The result of a look at the output of a login.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Not enough output yet.
    Wait,
    /// The code and the page, both checked.
    Found(String, String),
    /// A code or a page that fails its check.
    Bad,
}

/// True when the text says that the device flow is not available: a `glab`
/// error for a server without an OAuth application or for a version that
/// has no device flow.
#[must_use]
pub fn unsupported(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("set 'client_id' first")
        || (lower.contains("device")
            && (lower.contains("not supported")
                || lower.contains("unsupported")
                || lower.contains("requires gitlab")))
}

/// Parses the output of a login. The code is the first word after
/// `one-time code` (`: CODE` or ` (CODE)`); the page is the first URL.
#[must_use]
pub fn parse_login(kind: AccountKind, host: &str, text: &str) -> Parsed {
    let mut code_text = None;
    let mut url = None;
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if code_text.is_none()
            && let Some(at) = lower.find("one-time code")
        {
            let rest = &line[at + "one-time code".len()..];
            let word = rest
                .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '('))
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|c: char| matches!(c, '(' | ')' | ':' | ',' | '.'));
            if !code_ok(kind, word) {
                return Parsed::Bad;
            }
            code_text = Some(word.to_owned());
        }
        if url.is_none()
            && lower.contains("url")
            && let Some(word) = line.split_whitespace().find(|w| w.contains("://"))
        {
            if !url_ok(kind, host, word) {
                return Parsed::Bad;
            }
            url = Some(word.to_owned());
        }
    }
    match (code_text, url) {
        (Some(c), Some(u)) => Parsed::Found(c, u),
        _ => Parsed::Wait,
    }
}

/// The code of `gh` is `XXXX-XXXX`; the code of `glab` is 4 to 16 of
/// `[A-Z0-9-]`.
fn code_ok(kind: AccountKind, word: &str) -> bool {
    let chars_ok = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    if kind == AccountKind::Github {
        word.split_once('-')
            .is_some_and(|(a, b)| a.len() == 4 && b.len() == 4 && chars_ok(a) && chars_ok(b))
    } else {
        (4..=16).contains(&word.len())
            && word
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    }
}

/// The page must be `https`, without userinfo, port, query or fragment,
/// with the host of the forge and the path of its device login.
fn url_ok(kind: AccountKind, host: &str, url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let want = if kind == AccountKind::Github {
        "login/device"
    } else {
        "oauth/device"
    };
    authority.eq_ignore_ascii_case(host)
        && !authority.contains(['@', ':'])
        && path == want
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// One host in the status of a CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStatus {
    pub host: String,
    pub user: Option<String>,
    pub ok: bool,
}

/// The hosts of `gh auth status --json hosts`.
#[must_use]
pub fn parse_gh_hosts(json: &str) -> Vec<HostStatus> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(hosts) = v["hosts"].as_object() else {
        return Vec::new();
    };
    hosts
        .iter()
        .filter_map(|(host, list)| {
            let list = list.as_array()?;
            let entry = list
                .iter()
                .find(|e| e["active"] == true)
                .or_else(|| list.first())?;
            Some(HostStatus {
                host: host.clone(),
                user: entry["login"].as_str().map(str::to_owned),
                ok: entry["state"] == "success",
            })
        })
        .collect()
}

/// The hosts of `glab auth status` (both streams): a line without an indent
/// names a host; `Logged in to H as U` signs it in.
#[must_use]
pub fn parse_glab_status(text: &str) -> Vec<HostStatus> {
    let mut out: Vec<HostStatus> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            let h = line.trim();
            if !h.contains(char::is_whitespace)
                && h.contains('.')
                && !out.iter().any(|r| r.host == h)
            {
                out.push(HostStatus {
                    host: h.to_owned(),
                    user: None,
                    ok: false,
                });
            }
            continue;
        }
        if let Some(at) = line.find("Logged in to ") {
            let mut words = line[at + "Logged in to ".len()..].split_whitespace();
            let (Some(host), Some("as"), Some(user)) = (words.next(), words.next(), words.next())
            else {
                continue;
            };
            match out.iter_mut().find(|r| r.host == host) {
                Some(r) => {
                    r.user = Some(user.to_owned());
                    r.ok = true;
                }
                None => out.push(HostStatus {
                    host: host.to_owned(),
                    user: Some(user.to_owned()),
                    ok: true,
                }),
            }
        }
    }
    out
}

/// Runs the status command of a CLI. `host` is one host; `None` lists
/// every host that the CLI knows.
pub(super) async fn run_status(
    runner: &Runner,
    kind: AccountKind,
    host: Option<&str>,
) -> Result<Vec<HostStatus>, String> {
    let (program, mut args) = if kind == AccountKind::Github {
        (
            runner.programs.gh.clone(),
            vec!["auth", "status", "--json", "hosts"],
        )
    } else {
        (runner.programs.glab.clone(), vec!["auth", "status"])
    };
    match (kind, host) {
        (_, Some(h)) => {
            args.push("--hostname");
            args.push(h);
        }
        (AccountKind::Gitlab, None) => args.push("--all"),
        _ => {}
    }
    let spec = Spec {
        program,
        args: args.into_iter().map(str::to_owned).collect(),
        cwd: None,
        env: exec::cli_env(&[]),
        stdin: None,
        max_stdout: 256 * 1024,
    };
    let name = cli_name(kind);
    let out = exec::run(spec).await.map_err(|e| match e {
        exec::ExecError::Spawn(ref io) if io.kind() == std::io::ErrorKind::NotFound => {
            format!("`{name}` is not installed on this station.")
        }
        _ => format!("`{name}` did not answer."),
    })?;
    if kind == AccountKind::Github {
        // `--json` exits 0 whatever the state of the logins.
        Ok(parse_gh_hosts(&String::from_utf8_lossy(&out.stdout)))
    } else {
        // `glab` writes its report to stderr.
        let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), out.stderr);
        Ok(parse_glab_status(&text))
    }
}

/// The user that the CLI is signed in as on `host`.
pub(super) async fn forge_user(runner: &Runner, kind: AccountKind, host: &str) -> Option<String> {
    run_status(runner, kind, Some(host))
        .await
        .ok()?
        .into_iter()
        .find(|r| r.host == host && r.ok)
        .and_then(|r| r.user)
}
