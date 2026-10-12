//! The private coordinator credential handoff to a provider child
//! (M3 6-5, ADR-006 §5.4).
//!
//! The raw credential travels only in the child's environment variable
//! [`TOKEN_ENV`]. The provider arguments name the variable, never its
//! value: Claude expands `${VAR}` in an `--mcp-config` header, and Codex
//! reads `bearer_token_env_var`. The endpoint is loopback only.
//!
//! The handoff is declared only for a provider version for which the
//! opt-in live smoke (§8.5) recorded a passing handoff. None has run yet,
//! so [`handoff`] answers `handoff_unverified` for every provider and the
//! `coordinator` capability stays off.

use fleetdeck_core::childenv::ChildEnvError;
use fleetdeck_core::{ChildEnv, Secret};
use fleetdeck_proto::event::ProviderKind;

/// The environment variable that carries the credential.
pub const TOKEN_ENV: &str = "FLEETDECK_COORDINATOR_TOKEN";
/// The capability reason while no live smoke has passed.
pub const UNVERIFIED: &str = "handoff_unverified";

/// The extra provider arguments for the coordinator endpoint at `url`, or
/// `None` when `url` is not a loopback MCP URL.
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
            vec!["--mcp-config".to_owned(), config.to_string()]
        }
        ProviderKind::Codex => vec![
            "-c".to_owned(),
            format!("mcp_servers.fleetdeck.url=\"{url}\""),
            "-c".to_owned(),
            format!("mcp_servers.fleetdeck.bearer_token_env_var=\"{TOKEN_ENV}\""),
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
}

/// Whether the handoff is verified for a provider version: `Err` with the
/// capability reason until the live smoke records a passing version.
pub fn handoff(_provider: ProviderKind, _version: Option<&str>) -> Result<(), &'static str> {
    Err(UNVERIFIED)
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
    fn the_capability_stays_off_until_the_live_smoke_passes() {
        for provider in [ProviderKind::Claude, ProviderKind::Codex] {
            assert_eq!(handoff(provider, Some("2.1.0")), Err(UNVERIFIED));
            assert_eq!(handoff(provider, None), Err(UNVERIFIED));
        }
    }
}
