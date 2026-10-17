//! The Fleet Git side of a project (design §3.1, §3.8, §8; phases 1 and 2):
//! the remote URL parts, the local-remote rule, and the save of the settings
//! with the sync transitions of the homes (decision B4b). The start in a
//! home is in [`super::fleet_git_start`].

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    GitHomeState, GitSettingsAnswer, GitSettingsPut, IdentityError, LocalRemotes, RemoteIdentity,
    SettingsError, check_settings, remote_identity,
};
use rusqlite::{Connection, params};

use super::git_snapshots::clear_checks;
use super::project_homes::{HomeRow, claim, homes_of, set_state, take_ownership};
use super::{DeckStore, Result, call, to_i64};

/// The environment variable that lets the deck accept local remotes.
const ALLOW_LOCAL_REMOTES_ENV: &str = "FLEETDECK_ALLOW_LOCAL_REMOTES";

/// `Allow` only for the value exactly `1`, else `Deny`.
fn local_remotes_for(value: Option<&str>) -> LocalRemotes {
    if value == Some("1") {
        LocalRemotes::Allow
    } else {
        LocalRemotes::Deny
    }
}

/// The phase-1 bridge: the deck's own process environment says that every
/// home station allows local remotes. (FG-5: T2.5 moves the test rig to
/// station.toml and removes this bridge)
#[must_use]
pub fn env_bridge() -> LocalRemotes {
    local_remotes_for(std::env::var(ALLOW_LOCAL_REMOTES_ENV).ok().as_deref())
}

/// The clone URL: `remote_url` with the SSH login name put back after
/// `ssh://` or before the host of a scp-like URL (design §8.1).
#[must_use]
pub fn clone_url(remote_url: &str, ssh_user: Option<&str>) -> String {
    let Some(user) = ssh_user else {
        return remote_url.to_owned();
    };
    if let Some(rest) = remote_url.strip_prefix("ssh://") {
        format!("ssh://{user}@{rest}")
    } else if remote_url.contains("://") || remote_url.starts_with('/') {
        remote_url.to_owned()
    } else {
        format!("{user}@{remote_url}")
    }
}

/// Splits the SSH login name off a remote URL: `(scrubbed URL, login name)`.
/// The caller checked the URL with `remote_identity`, so a password is not
/// possible here.
#[must_use]
pub fn split_login(url: &str) -> (String, Option<String>) {
    if let Some(rest) = url.strip_prefix("ssh://") {
        let end = rest.find('/').unwrap_or(rest.len());
        return match rest[..end].rsplit_once('@') {
            Some((user, host)) => (
                format!("ssh://{host}{}", &rest[end..]),
                Some(user.to_owned()),
            ),
            None => (url.to_owned(), None),
        };
    }
    if url.contains("://") || url.starts_with('/') {
        return (url.to_owned(), None);
    }
    // scp-like `user@host:path`; the `@` counts only before the first `:`.
    let colon = url.find(':').unwrap_or(url.len());
    match url[..colon].split_once('@') {
        Some((user, host)) => (format!("{host}{}", &url[colon..]), Some(user.to_owned())),
        None => (url.to_owned(), None),
    }
}

/// The remote fields of a save: the scrubbed URL, the login name and the
/// identity of the full clone URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteWrite {
    pub url: String,
    pub user: Option<String>,
    pub identity: RemoteIdentity,
}

/// The result of [`DeckStore::set_project_git`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitWrite {
    Saved(Box<GitSettingsAnswer>),
    /// The stored revision is not the one the client loaded.
    Stale,
    /// A change of the remote URL while sync is on.
    RebindRequired,
    /// A rule refused the settings.
    Invalid(SettingsError),
    UnknownProject,
}

pub(super) fn refuse(field: &str, message: impl Into<String>) -> SettingsError {
    SettingsError {
        field: field.to_owned(),
        message: message.into(),
    }
}

/// Checks a remote URL and splits it (design §3.1, §3.8). `local` says
/// whether a local remote is allowed ([`DeckStore::local_remotes_of`]).
///
/// # Errors
/// A [`SettingsError`] for field `remote_url`.
pub fn remote_write(
    url: &str,
    local: LocalRemotes,
) -> std::result::Result<RemoteWrite, SettingsError> {
    let url = url.trim();
    let identity = remote_identity(url, local).map_err(|e| {
        refuse(
            "remote_url",
            match e {
                IdentityError::RemoteLocalRefused
                | IdentityError::RemoteUnsupported { escaped_path: true } => e.to_string(),
                _ => {
                    "The remote URL must be an https, ssh or user@host:path URL without a password."
                        .to_owned()
                }
            },
        )
    })?;
    let (scrubbed, user) = split_login(url);
    if let Some(user) = &user
        && !((1..=64).contains(&user.len())
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
    {
        return Err(refuse(
            "remote_url",
            "The SSH login name must be 1 to 64 characters of letters, digits, dot, dash and underscore.",
        ));
    }
    Ok(RemoteWrite {
        url: scrubbed,
        user,
        identity,
    })
}

impl DeckStore {
    /// Whether a local remote is allowed for `project` (decision B10): every
    /// home station of the project (any state) reports
    /// `FleetGitInfo.allow_local_remotes = true` in its newest `station.info`
    /// (a missing `station.info` or `fleet_git` counts false); the
    /// environment bridge is an OR.
    ///
    /// # Errors
    /// A database error.
    pub async fn local_remotes_of(&self, project: String) -> Result<LocalRemotes> {
        let stations: Vec<String> = call(&self.inner.db, move |conn| {
            Ok(homes_of(conn, &project)?
                .into_iter()
                .map(|h| h.station_id)
                .collect())
        })
        .await?;
        Ok(self.local_remotes_for_stations(&stations))
    }

    /// [`Self::local_remotes_of`] for a list of stations.
    #[must_use]
    pub fn local_remotes_for_stations(&self, stations: &[String]) -> LocalRemotes {
        if env_bridge() == LocalRemotes::Allow {
            return LocalRemotes::Allow;
        }
        let all = !stations.is_empty()
            && stations.iter().all(|s| {
                self.station_info(&StationId::from(s.clone()))
                    .and_then(|(info, _)| info.fleet_git)
                    .is_some_and(|g| g.allow_local_remotes)
            });
        if all {
            LocalRemotes::Allow
        } else {
            LocalRemotes::Deny
        }
    }

    /// Saves the Git settings of a project (design §3.1, §17.1). In one
    /// transaction, in this order: the revision must be the stored one
    /// (`Stale`); a change of the remote URL needs sync off (`RebindRequired`);
    /// the rules (`Invalid`); then the write bumps `git_revision`, stores the
    /// remote when given, applies the sync transition to the homes (decision
    /// B4b), and forgets the stored checks of the project.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn set_project_git(&self, id: String, put: GitSettingsPut) -> Result<GitWrite> {
        let local = self.local_remotes_of(id.clone()).await?;
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(p) = super::projects::find(&tx, &id)? else {
                return Ok(GitWrite::UnknownProject);
            };
            if p.git_revision != put.revision {
                return Ok(GitWrite::Stale);
            }
            let current = p
                .remote_url
                .as_deref()
                .map(|u| clone_url(u, p.remote_ssh_user.as_deref()));
            let new_url = put.remote_url.as_deref().map(str::trim);
            if let Some(url) = new_url
                && p.git.sync
                && current.as_deref() != Some(url)
            {
                return Ok(GitWrite::RebindRequired);
            }
            let settings = &put.settings;
            if let Err(e) = check_settings(settings) {
                return Ok(GitWrite::Invalid(e));
            }
            let remote = match new_url {
                Some(url) => match remote_write(url, local) {
                    Ok(r) => Some(r),
                    Err(e) => return Ok(GitWrite::Invalid(e)),
                },
                None => None,
            };
            if settings.sync && remote.is_none() && p.remote_url.is_none() {
                return Ok(GitWrite::Invalid(refuse(
                    "sync",
                    "Set the remote URL first.",
                )));
            }
            let homes = homes_of(&tx, &id)?;
            let turning_on = settings.sync && !p.git.sync;
            if turning_on && let Some(e) = ownership_refusal(&tx, &id, &homes)? {
                return Ok(GitWrite::Invalid(e));
            }
            if let Some(url) = &settings.wip_remote
                && let Err(e @ IdentityError::RemoteLocalRefused) = remote_identity(url, local)
            {
                return Ok(GitWrite::Invalid(refuse("wip_remote", e.to_string())));
            }
            // The radar home is a station with a home of the project, in any
            // state (decision B22).
            if settings
                .radar_home
                .as_ref()
                .is_some_and(|s| !homes.iter().any(|h| h.station_id == s.as_str()))
            {
                return Ok(GitWrite::Invalid(refuse(
                    "radar_home",
                    "Pick a station with a ready home.",
                )));
            }
            let next = put.revision.saturating_add(1);
            let json = serde_json::to_string(settings)?;
            match &remote {
                Some(r) => tx.execute(
                    "UPDATE projects SET git = ?2, git_revision = ?3, remote_url = ?4,
                         remote_ssh_user = ?5, repo = ?6, remote_dest = ?7
                     WHERE project_id = ?1",
                    params![
                        id,
                        json,
                        to_i64(next)?,
                        r.url,
                        r.user,
                        r.identity.repo,
                        r.identity.dest
                    ],
                )?,
                None => tx.execute(
                    "UPDATE projects SET git = ?2, git_revision = ?3 WHERE project_id = ?1",
                    params![id, json, to_i64(next)?],
                )?,
            };
            if turning_on {
                sync_on(&tx, &id, &homes, now)?;
            } else if !settings.sync && p.git.sync {
                sync_off(&tx, &id, &homes, now)?;
            }
            clear_checks(&tx, &id)?;
            tx.commit()?;
            Ok(GitWrite::Saved(Box::new(GitSettingsAnswer {
                git: put.settings,
                git_revision: next,
            })))
        })
        .await;
        self.board_changed();
        out
    }
}

/// The refusal of a save that turns sync on: another sync project owns, holds
/// a folder above, or holds a folder below a home with a path (decisions B4b,
/// B5a, B14). The text for the same folder is the phase-1 text.
fn ownership_refusal(
    conn: &Connection,
    project: &str,
    homes: &[HomeRow],
) -> Result<Option<SettingsError>> {
    for home in homes {
        let Some(folder) = home.folder() else {
            continue;
        };
        let found = claim(conn, project, &home.station_id, folder)?;
        if let Some((code, text)) = found.refusal() {
            let message = if code == "home_owned" {
                "Another synced project uses this folder on this station.".to_owned()
            } else {
                text
            };
            return Ok(Some(refuse("sync", message)));
        }
    }
    Ok(None)
}

/// Sync turns on: every home with a path is `unverified` (no code) and owns
/// its folder (decision B4b).
fn sync_on(conn: &Connection, id: &str, homes: &[HomeRow], now: i64) -> Result<()> {
    for home in homes {
        let Some(folder) = home.folder() else {
            continue;
        };
        take_ownership(conn, id, &home.station_id, folder, now)?;
        set_state(
            conn,
            (id, &home.station_id),
            GitHomeState::Unverified,
            None,
            None,
            now,
        )?;
    }
    Ok(())
}

/// Sync turns off: every home with a path is `ready` and owns nothing; a
/// `cloning` or `failed` home keeps its state (decision B4b).
fn sync_off(conn: &Connection, id: &str, homes: &[HomeRow], now: i64) -> Result<()> {
    for home in homes.iter().filter(|h| h.path.is_some()) {
        conn.execute(
            "UPDATE project_homes SET sync_owner = 0 WHERE project_id = ?1 AND station_id = ?2",
            [id, home.station_id.as_str()],
        )?;
        set_state(
            conn,
            (id, &home.station_id),
            GitHomeState::Ready,
            None,
            None,
            now,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deck_allows_local_remotes_only_for_the_value_one() {
        for (value, want) in [
            (Some("1"), LocalRemotes::Allow),
            (None, LocalRemotes::Deny),
            (Some(""), LocalRemotes::Deny),
            (Some("0"), LocalRemotes::Deny),
            (Some("true"), LocalRemotes::Deny),
            (Some("11"), LocalRemotes::Deny),
            (Some(" 1"), LocalRemotes::Deny),
        ] {
            assert_eq!(local_remotes_for(value), want, "{value:?}");
        }
    }

    #[test]
    fn the_login_name_is_split_off_and_put_back() {
        for (full, scrubbed, user) in [
            (
                "git@github.com:acme/app.git",
                "github.com:acme/app.git",
                Some("git"),
            ),
            (
                "ssh://git@github.com/acme/app.git",
                "ssh://github.com/acme/app.git",
                Some("git"),
            ),
            (
                "ssh://git@host:2222/acme/app",
                "ssh://host:2222/acme/app",
                Some("git"),
            ),
            ("ssh://host/acme/app", "ssh://host/acme/app", None),
            ("host:acme/app", "host:acme/app", None),
            (
                "https://github.com/acme/app.git",
                "https://github.com/acme/app.git",
                None,
            ),
            ("/srv/remote.git", "/srv/remote.git", None),
        ] {
            let (url, who) = split_login(full);
            assert_eq!((url.as_str(), who.as_deref()), (scrubbed, user), "{full}");
            assert_eq!(clone_url(&url, who.as_deref()), full, "{full}");
        }
    }
}
