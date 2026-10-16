//! The private coordinator credential handoff to a provider child
//! (M3 6-5, ADR-006 §5.4).
//!
//! The raw credential travels only in the child's environment variable
//! [`TOKEN_ENV`]. The provider arguments name the variable, never its
//! value: Claude expands `${VAR}` in an `--mcp-config` header, and Codex
//! reads `bearer_token_env_var`. The endpoint is loopback only.
//!
//! The handoff is declared only for a provider version for which the
//! opt-in live smoke (§8.5) recorded a passing handoff ([`VERIFIED`]);
//! [`handoff`] answers `handoff_unverified` for every other version and the
//! `coordinator` capability stays off there.

use fleetdeck_core::childenv::ChildEnvError;
use fleetdeck_core::{ChildEnv, Secret};
use fleetdeck_proto::event::ProviderKind;

/// The environment variable that carries the credential.
pub const TOKEN_ENV: &str = "FLEETDECK_COORDINATOR_TOKEN";
/// The capability reason while no live smoke has passed.
pub const UNVERIFIED: &str = "handoff_unverified";

/// The extra provider arguments for the coordinator endpoint at `url`, or
/// `None` when `url` is not a loopback MCP URL. The owner-authorized grant
/// is the authority of a coordinator tool call (ADR-006 §6): the station
/// checks its classes and the deck its scope, so the provider asks no
/// second, native approval for the `fleetdeck` server's tools (DL-220).
pub fn mcp_args(provider: ProviderKind, url: &str) -> Option<Vec<String>> {
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let port = rest.strip_suffix("/mcp")?;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(match provider {
        ProviderKind::Claude => {
            let config = serde_json::json!({"mcpServers": {"fleetdeck": {
                "type": "http",
                "url": url,
                "headers": {"Authorization": format!("Bearer ${{{TOKEN_ENV}}}")},
            }}});
            vec![
                "--mcp-config".to_owned(),
                config.to_string(),
                "--allowedTools".to_owned(),
                "mcp__fleetdeck".to_owned(),
            ]
        }
        ProviderKind::Codex => vec![
            "-c".to_owned(),
            format!("mcp_servers.fleetdeck.url=\"{url}\""),
            "-c".to_owned(),
            format!("mcp_servers.fleetdeck.bearer_token_env_var=\"{TOKEN_ENV}\""),
            "-c".to_owned(),
            "mcp_servers.fleetdeck.default_tools_approval_mode=\"approve\"".to_owned(),
        ],
        _ => return None,
    })
}

/// A TOML basic string of `text`, or `None` for a control character
/// (DL-194).
fn toml_string(text: &str) -> Option<String> {
    if text.chars().any(char::is_control) {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Some(out)
}

/// The provider arguments of a shim host (DL-194 §2.2): a stdio MCP server
/// `<program> mcp-bridge --socket <socket>` that relays to the station
/// socket. The credential is not in the arguments; the bridge reads
/// [`TOKEN_ENV`] from its environment. `None` for a path that is not
/// UTF-8 (or has a control character, for Codex) or another provider.
pub fn bridge_args(
    provider: ProviderKind,
    program: &std::path::Path,
    socket: &std::path::Path,
) -> Option<Vec<String>> {
    let program = program.to_str()?;
    let socket = socket.to_str()?;
    Some(match provider {
        ProviderKind::Claude => {
            let config = serde_json::json!({"mcpServers": {"fleetdeck": {
                "type": "stdio",
                "command": program,
                "args": ["mcp-bridge", "--socket", socket],
            }}});
            vec![
                "--mcp-config".to_owned(),
                config.to_string(),
                "--allowedTools".to_owned(),
                "mcp__fleetdeck".to_owned(),
            ]
        }
        ProviderKind::Codex => vec![
            "-c".to_owned(),
            format!("mcp_servers.fleetdeck.command={}", toml_string(program)?),
            "-c".to_owned(),
            format!(
                "mcp_servers.fleetdeck.args=[\"mcp-bridge\",\"--socket\",{}]",
                toml_string(socket)?
            ),
            "-c".to_owned(),
            format!("mcp_servers.fleetdeck.env_vars=[\"{TOKEN_ENV}\"]"),
            "-c".to_owned(),
            "mcp_servers.fleetdeck.default_tools_approval_mode=\"approve\"".to_owned(),
        ],
        _ => return None,
    })
}

/// The coordinator additions of one start (ADR-006 §5.4, DL-216): the
/// provider arguments that name [`TOKEN_ENV`] and the credential, which
/// goes only into the child environment. `Debug` never shows the value.
#[derive(Debug)]
pub struct CoordinatorLaunch {
    pub args: Vec<String>,
    pub token: Secret<String>,
}

impl CoordinatorLaunch {
    /// The child environment `base` plus the credential variable.
    ///
    /// # Errors
    /// [`ChildEnvError`] for an invalid value.
    pub fn child_env(&self, base: &ChildEnv) -> Result<ChildEnv, ChildEnvError> {
        let mut env = base.clone();
        env.set(TOKEN_ENV, self.token.expose())?;
        Ok(env)
    }

    /// The child environment of a start with an optional handoff.
    ///
    /// # Errors
    /// [`crate::adapter::ProviderError::Workspace`] for an invalid value.
    pub(crate) fn env_for(
        launch: Option<&Self>,
        base: &ChildEnv,
        cwd: &std::path::Path,
    ) -> crate::adapter::Result<ChildEnv> {
        match launch {
            Some(c) => c
                .child_env(base)
                .map_err(|e| crate::adapter::ProviderError::Workspace {
                    path: cwd.to_path_buf(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, e),
                }),
            None => Ok(base.clone()),
        }
    }
}

/// The provider families and minimum versions for which the opt-in live
/// smoke (ADR-006 §8.5, test 31) recorded a passing handoff (DL-219,
/// DL-220: both providers, also through the station path).
pub const VERIFIED: &[(ProviderKind, &str)] = &[
    (ProviderKind::Claude, "2.1.296"),
    (ProviderKind::Codex, "0.160.0"),
];

/// The numeric parts of a dotted version (`2.1.296 (Claude Code)` →
/// `[2, 1, 296]`), or `None` when it does not start with a number.
fn numeric(version: &str) -> Option<Vec<u64>> {
    let head = version.split_whitespace().next()?;
    let parts: Vec<u64> = head.split('.').map_while(|p| p.parse().ok()).collect();
    (!parts.is_empty()).then_some(parts)
}

/// The oldest version of each provider whose stdio bridge passed the live
/// smoke `live_bridge` (DL-194 §9.10). Codex joins after the owner's run.
pub const BRIDGE_VERIFIED: &[(ProviderKind, &str)] = &[(ProviderKind::Claude, "2.1.296")];

/// `Ok` for a `version` at or above the minimum that `list` names for
/// `provider`; `Err(UNVERIFIED)` for an unknown version, a lower one or a
/// provider that `list` does not name.
fn at_or_above(
    list: &[(ProviderKind, &str)],
    provider: ProviderKind,
    version: Option<&str>,
) -> Result<(), &'static str> {
    let min = list
        .iter()
        .find(|(kind, _)| *kind == provider)
        .and_then(|(_, min)| numeric(min))
        .ok_or(UNVERIFIED)?;
    let got = version.and_then(numeric).ok_or(UNVERIFIED)?;
    if got >= min { Ok(()) } else { Err(UNVERIFIED) }
}

/// Whether the handoff is verified for a provider version (ADR-006 §5.4):
/// `Ok` for a version at or above the [`VERIFIED`] minimum of its
/// provider family; `Err` with the capability reason for an unknown
/// version, a lower one, or a provider without a verified handoff.
pub fn handoff(provider: ProviderKind, version: Option<&str>) -> Result<(), &'static str> {
    at_or_above(VERIFIED, provider, version)
}

/// [`handoff`] for the stdio bridge of a shim host, with the
/// [`BRIDGE_VERIFIED`] list (DL-194 §9.10).
pub fn bridge_handoff(provider: ProviderKind, version: Option<&str>) -> Result<(), &'static str> {
    at_or_above(BRIDGE_VERIFIED, provider, version)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_provider_arguments_name_the_variable_never_a_credential_and_need_a_loopback_url() {
        let url = "http://127.0.0.1:41234/mcp";
        let claude = mcp_args(ProviderKind::Claude, url).unwrap();
        assert_eq!(claude[0], "--mcp-config");
        assert_eq!(claude[2..], ["--allowedTools", "mcp__fleetdeck"]);
        let config: serde_json::Value = serde_json::from_str(&claude[1]).unwrap();
        let server = &config["mcpServers"]["fleetdeck"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], url);
        assert_eq!(
            server["headers"]["Authorization"],
            "Bearer ${FLEETDECK_COORDINATOR_TOKEN}"
        );
        let codex = mcp_args(ProviderKind::Codex, url).unwrap();
        assert_eq!(
            codex,
            [
                "-c",
                "mcp_servers.fleetdeck.url=\"http://127.0.0.1:41234/mcp\"",
                "-c",
                "mcp_servers.fleetdeck.bearer_token_env_var=\"FLEETDECK_COORDINATOR_TOKEN\"",
                "-c",
                "mcp_servers.fleetdeck.default_tools_approval_mode=\"approve\"",
            ]
        );
        for args in [&claude, &codex] {
            assert!(args.iter().all(|a| !a.contains("fdc1_")));
        }
        for bad in [
            "http://0.0.0.0:1/mcp",
            "http://example.com:1/mcp",
            "https://127.0.0.1:1/mcp",
            "http://127.0.0.1:1/other",
            "http://127.0.0.1:/mcp",
            "http://127.0.0.1:1@evil/mcp",
        ] {
            assert!(mcp_args(ProviderKind::Claude, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_bridge_arguments_name_the_program_the_socket_and_the_variable_only() {
        use std::path::Path;
        let program = Path::new("/home/u/.local/share/fleetdeck/station/bin/current");
        let socket = Path::new("/run/user/1000/fleetdeck/01INC/mcp.s");
        let claude = bridge_args(ProviderKind::Claude, program, socket).unwrap();
        assert_eq!(claude[0], "--mcp-config");
        assert_eq!(claude[2..], ["--allowedTools", "mcp__fleetdeck"]);
        let config: serde_json::Value = serde_json::from_str(&claude[1]).unwrap();
        let server = &config["mcpServers"]["fleetdeck"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], program.to_str().unwrap());
        assert_eq!(
            server["args"],
            serde_json::json!(["mcp-bridge", "--socket", socket.to_str().unwrap()])
        );
        let codex = bridge_args(ProviderKind::Codex, program, socket).unwrap();
        assert_eq!(
            codex,
            [
                "-c",
                "mcp_servers.fleetdeck.command=\"/home/u/.local/share/fleetdeck/station/bin/current\"",
                "-c",
                "mcp_servers.fleetdeck.args=[\"mcp-bridge\",\"--socket\",\"/run/user/1000/fleetdeck/01INC/mcp.s\"]",
                "-c",
                "mcp_servers.fleetdeck.env_vars=[\"FLEETDECK_COORDINATOR_TOKEN\"]",
                "-c",
                "mcp_servers.fleetdeck.default_tools_approval_mode=\"approve\"",
            ]
        );
        for args in [&claude, &codex] {
            assert!(args.iter().all(|a| !a.contains("fdc1_")));
        }
        assert!(bridge_args(ProviderKind::Unknown, program, socket).is_none());
    }

    #[test]
    fn a_bridge_path_with_a_quote_or_a_backslash_is_escaped_and_a_control_character_is_refused() {
        use std::path::Path;
        let program = Path::new("/odd \"dir\\x/current");
        let socket = Path::new("/run/s.s");
        let codex = bridge_args(ProviderKind::Codex, program, socket).unwrap();
        assert_eq!(
            codex[1],
            "mcp_servers.fleetdeck.command=\"/odd \\\"dir\\\\x/current\""
        );
        let claude = bridge_args(ProviderKind::Claude, program, socket).unwrap();
        let config: serde_json::Value = serde_json::from_str(&claude[1]).unwrap();
        assert_eq!(
            config["mcpServers"]["fleetdeck"]["command"],
            "/odd \"dir\\x/current"
        );
        let newline = Path::new("/odd\ndir/current");
        assert!(bridge_args(ProviderKind::Codex, newline, socket).is_none());
        assert!(bridge_args(ProviderKind::Codex, program, newline).is_none());
    }

    #[test]
    fn a_bridge_path_that_is_not_utf8_gives_no_arguments() {
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/odd\xffdir/current"));
        let socket = Path::new("/run/s.s");
        for provider in [ProviderKind::Claude, ProviderKind::Codex] {
            assert!(bridge_args(provider, bad, socket).is_none());
            assert!(bridge_args(provider, socket, bad).is_none());
        }
    }

    #[test]
    fn the_capability_stays_off_until_the_live_smoke_passes() {
        for (provider, older) in [
            (ProviderKind::Claude, "2.1.0"),
            (ProviderKind::Codex, "0.159.9"),
        ] {
            assert_eq!(handoff(provider, Some(older)), Err(UNVERIFIED));
            assert_eq!(handoff(provider, None), Err(UNVERIFIED));
            assert_eq!(handoff(provider, Some("unknown")), Err(UNVERIFIED));
        }
        // DL-219, DL-220: the live smoke passed for Claude 2.1.296 and
        // Codex 0.160.0.
        assert_eq!(
            handoff(ProviderKind::Claude, Some("2.1.295")),
            Err(UNVERIFIED)
        );
        assert_eq!(handoff(ProviderKind::Claude, Some("2.1.296")), Ok(()));
        assert_eq!(
            handoff(ProviderKind::Claude, Some("2.1.296 (Claude Code)")),
            Ok(())
        );
        assert_eq!(handoff(ProviderKind::Claude, Some("2.10.0")), Ok(()));
        assert_eq!(handoff(ProviderKind::Codex, Some("0.160.0")), Ok(()));
        assert_eq!(handoff(ProviderKind::Codex, Some("0.161.2")), Ok(()));
    }

    #[test]
    fn the_bridge_is_verified_for_claude_2_1_296_and_not_for_codex_yet() {
        assert_eq!(
            bridge_handoff(ProviderKind::Claude, Some("2.1.296")),
            Ok(())
        );
        assert_eq!(
            bridge_handoff(ProviderKind::Claude, Some("2.1.295")),
            Err(UNVERIFIED)
        );
        assert_eq!(bridge_handoff(ProviderKind::Claude, None), Err(UNVERIFIED));
        assert_eq!(
            bridge_handoff(ProviderKind::Codex, Some("0.162.1")),
            Err(UNVERIFIED)
        );
    }
}
