//! The `[[trackers]]` entries of `station.toml` (R3, DL-164): Jira and
//! Linear. The config holds where a token comes from, never the token.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fleetdeck_proto::integrations::{TrackerKind, valid_host, valid_tracker_id};
use serde::Deserialize;

use super::{ConfigError, expand_path, invalid, valid_env_key};
use crate::secret::Secret;

/// Where a tracker token comes from. The config never holds the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// The name of an environment variable of the station process.
    Env(String),
    /// A file that only the owner can read (mode 0600).
    File(PathBuf),
}

/// One `[[trackers]]` entry (R3, DL-164).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerConfig {
    pub id: String,
    pub kind: TrackerKind,
    /// Jira only: `https://acme.atlassian.net`, without a path.
    pub site: Option<String>,
    /// Jira only: the account e-mail for the basic-auth pair.
    pub email: Option<String>,
    pub token: TokenSource,
}

/// Why a tracker token cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("the environment variable `{0}` is not set")]
    EnvMissing(String),
    #[error("cannot read the token file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the token file {path} has mode {mode:o}; use mode 600")]
    Mode { path: PathBuf, mode: u32 },
    #[error("the token is empty or holds a control character")]
    Bad,
}

impl TrackerConfig {
    /// Reads the token now. The caller keeps it only for one request.
    ///
    /// # Errors
    /// [`TokenError`]: a missing variable, an unreadable file, a file that
    /// other users can read, or a token with a control character.
    pub fn read_token(&self) -> Result<Secret<String>, TokenError> {
        let raw = match &self.token {
            TokenSource::Env(name) => {
                std::env::var(name).map_err(|_| TokenError::EnvMissing(name.clone()))?
            }
            TokenSource::File(path) => {
                let io = |source| TokenError::Io {
                    path: path.clone(),
                    source,
                };
                let mode = fs::metadata(path).map_err(io)?.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return Err(TokenError::Mode {
                        path: path.clone(),
                        mode,
                    });
                }
                fs::read_to_string(path).map_err(io)?
            }
        };
        let token = raw.trim();
        if token.is_empty() || token.chars().any(char::is_control) {
            return Err(TokenError::Bad);
        }
        Ok(Secret::new(token.to_owned()))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawTracker {
    pub(super) id: String,
    pub(super) kind: TrackerKind,
    pub(super) site: Option<String>,
    pub(super) email: Option<String>,
    pub(super) token_env: Option<String>,
    pub(super) token_file: Option<String>,
}

/// The most `[[trackers]]` entries.
const MAX_TRACKERS: usize = 16;

pub(super) fn parse_trackers(
    list: Vec<RawTracker>,
    home: Option<&Path>,
) -> Result<Vec<TrackerConfig>, ConfigError> {
    if list.len() > MAX_TRACKERS {
        return Err(invalid("trackers", "use at most 16 entries"));
    }
    let mut out: Vec<TrackerConfig> = Vec::with_capacity(list.len());
    for (i, raw) in list.into_iter().enumerate() {
        let field = |k: &str| format!("trackers[{i}].{k}");
        if !valid_tracker_id(&raw.id) {
            return Err(invalid(
                field("id"),
                format!(
                    "`{}`: use 1-32 of a-z, 0-9, `-`, `_`, starting with a letter or digit",
                    raw.id
                ),
            ));
        }
        if out.iter().any(|o| o.id == raw.id) {
            return Err(invalid(field("id"), format!("`{}` is used twice", raw.id)));
        }
        let (site, email) = match raw.kind {
            TrackerKind::Jira => {
                let site = raw.site.ok_or_else(|| {
                    invalid(
                        field("site"),
                        "a Jira tracker needs its site, for example https://acme.atlassian.net",
                    )
                })?;
                let email = raw.email.ok_or_else(|| {
                    invalid(field("email"), "a Jira tracker needs the account e-mail")
                })?;
                (
                    Some(check_site(&field("site"), &site)?),
                    Some(check_email(&field("email"), email)?),
                )
            }
            TrackerKind::Linear => {
                if raw.site.is_some() || raw.email.is_some() {
                    return Err(invalid(
                        field("site"),
                        "a Linear tracker takes neither a site nor an e-mail",
                    ));
                }
                (None, None)
            }
        };
        let token = match (raw.token_env, raw.token_file) {
            (Some(name), None) if valid_env_key(&name) => TokenSource::Env(name),
            (Some(name), None) => {
                return Err(invalid(
                    field("token_env"),
                    format!("`{name}` is not a valid variable name"),
                ));
            }
            (None, Some(path)) => TokenSource::File(expand_path(&path, home).map_err(
                |source| ConfigError::Path {
                    field: field("token_file"),
                    source,
                },
            )?),
            _ => {
                return Err(invalid(
                    field("token_env"),
                    "give exactly one of token_env and token_file",
                ));
            }
        };
        out.push(TrackerConfig {
            id: raw.id,
            kind: raw.kind,
            site,
            email,
            token,
        });
    }
    Ok(out)
}

/// `https://host[:port]` without a path, a user or a query; a final `/` is
/// dropped.
fn check_site(field: &str, site: &str) -> Result<String, ConfigError> {
    let host = site
        .strip_prefix("https://")
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| valid_host(h));
    host.map(|h| format!("https://{h}")).ok_or_else(|| {
        invalid(
            field,
            "use https://<host> without a path, for example https://acme.atlassian.net",
        )
    })
}

fn check_email(field: &str, email: String) -> Result<String, ConfigError> {
    let ok = email.len() <= 254
        && email.contains('@')
        && !email
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '"' || c == '\\');
    if ok {
        Ok(email)
    } else {
        Err(invalid(field, "give the account e-mail address"))
    }
}
