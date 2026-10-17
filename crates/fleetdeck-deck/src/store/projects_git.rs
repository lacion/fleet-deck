//! The Fleet Git side of a project (design §3.1, §3.8, §8; phase 1): the
//! derived home and Git names of the project list, the remote URL parts, the
//! save of the settings, and the `FleetGitStart` of a start in a sync home.

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    FleetGitStart, GitHomeState, GitSettingsAnswer, GitSettingsPut, HomeCheck, IdentityError,
    LocalRemotes, ProjectHome, RemoteIdentity, SettingsError, SlugReservation, StartKind,
    check_settings, remote_identity,
};
use fleetdeck_proto::integrations::Project;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;
use std::path::Path;

use super::git_snapshots::{PHASE1_GENERATION, checks_of, clear_checks, verified};
use super::{DeckStore, LinkFeature, Result, call, slug_reservations, to_i64};

/// The wire name of a check state, for the code of an unverified home.
fn state_name(check: &HomeCheck) -> String {
    serde_json::to_value(check.state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Fills `homes` and `slugs` of `projects` (design §8.1). Phase 1: one home
/// at position 0 from `station_id` and `path`, generation 1. A sync-off
/// project is `ready` as today; a sync project is `ready` only after the
/// station's home check said `verified`.
pub(super) fn derive(conn: &Connection, projects: &mut [Project]) -> Result<()> {
    let mut checks: BTreeMap<String, Vec<HomeCheck>> = BTreeMap::new();
    for p in projects.iter_mut() {
        if !checks.contains_key(&p.station_id) {
            checks.insert(p.station_id.clone(), checks_of(conn, &p.station_id)?);
        }
        let own = &checks[&p.station_id];
        let (state, code) = if !p.git.sync || verified(own, &p.project_id).is_some() {
            (GitHomeState::Ready, None)
        } else {
            let seen = own
                .iter()
                .find(|c| c.project_id == p.project_id && c.generation == PHASE1_GENERATION);
            (
                GitHomeState::Unverified,
                seen.map(|c| c.code.clone().unwrap_or_else(|| state_name(c))),
            )
        };
        p.homes = vec![ProjectHome {
            station_id: StationId::from(p.station_id.clone()),
            generation: PHASE1_GENERATION,
            position: 0,
            state,
            path: Some(p.path.clone()),
            reserved_path: None,
            attempt_id: None,
            sync_owner: p.git.sync,
            code,
            message: None,
        }];
        p.slugs = match &p.repo {
            Some(repo) => {
                let mut stmt = conn.prepare(
                    "SELECT slug, station_id FROM slug_reservations WHERE repo = ?1 ORDER BY slug",
                )?;
                stmt.query_map([repo], |r| {
                    Ok(SlugReservation {
                        slug: r.get(0)?,
                        station_id: StationId::from(r.get::<_, String>(1)?),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => Vec::new(),
        };
    }
    Ok(())
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

fn refuse(field: &str, message: impl Into<String>) -> SettingsError {
    SettingsError {
        field: field.to_owned(),
        message: message.into(),
    }
}

/// Checks a remote URL and splits it (design §3.1, §3.8; phase 1 refuses
/// every local remote: no station reports `allow_local_remotes` yet).
///
/// # Errors
/// A [`SettingsError`] for field `remote_url`.
pub fn remote_write(url: &str) -> std::result::Result<RemoteWrite, SettingsError> {
    let url = url.trim();
    let identity = remote_identity(url, LocalRemotes::Deny).map_err(|e| {
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

/// How a start relates to the sync projects of its station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// No sync home holds the folder: the start is plain (O1).
    Plain,
    Attach(Box<FleetGitStart>),
    /// The station did not negotiate `fleet_git.v1`.
    Unsupported,
    /// The deck has no `station.info` of the station, so no slug.
    StationInfoMissing,
    /// The home holds the folder, but its check is not `verified`.
    Unverified {
        path: String,
        repo: String,
    },
    /// `(repo, slug)` is reserved for the station `other` (its name).
    SlugTaken {
        repo: String,
        slug: String,
        other: String,
    },
}

/// What a start asks (design §8.5, phase 1).
#[derive(Debug, Clone)]
pub struct StartQuery {
    pub station: String,
    pub cwd: String,
    pub kind: StartKind,
    /// The slug of the station name, when the deck knows the name.
    pub slug: Option<String>,
    pub feature: LinkFeature,
    /// A start that names its project (the batch start): only this project
    /// competes, whatever the folder.
    pub project: Option<String>,
}

impl DeckStore {
    /// Saves the Git settings of a project (design §3.1, §17.1). In one
    /// transaction, in this order: the revision must be the stored one
    /// (`Stale`); a change of the remote URL needs sync off (`RebindRequired`);
    /// the rules (`Invalid`); then the write bumps `git_revision`, stores the
    /// remote when given, and forgets the stored checks of the project (the
    /// home is unverified until the station answers the next snapshot).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn set_project_git(&self, id: String, put: GitSettingsPut) -> Result<GitWrite> {
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
                Some(url) => match remote_write(url) {
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
            if settings.sync {
                let other: Option<String> = tx
                    .query_row(
                        "SELECT name FROM projects
                         WHERE station_id = ?1 AND path = ?2 AND project_id <> ?3
                           AND COALESCE(json_extract(git, '$.sync') = 1, 0) = 1
                         LIMIT 1",
                        params![p.station_id, p.path, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if other.is_some() {
                    return Ok(GitWrite::Invalid(refuse(
                        "sync",
                        "Another synced project uses this folder on this station.",
                    )));
                }
            }
            if let Some(url) = &settings.wip_remote
                && let Err(e @ IdentityError::RemoteLocalRefused) =
                    remote_identity(url, LocalRemotes::Deny)
            {
                return Ok(GitWrite::Invalid(refuse("wip_remote", e.to_string())));
            }
            if settings
                .radar_home
                .as_ref()
                .is_some_and(|s| s.as_str() != p.station_id)
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

    /// The `FleetGitStart` for a start on `query.station` in `query.cwd`
    /// (design §2.1, §8.5; phase 1). The sync projects of the station whose
    /// home holds the folder compete: the longest path, then the oldest
    /// project. The home must be verified; then the slug is reserved for the
    /// station in the same transaction.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn git_start(&self, query: StartQuery) -> Result<StartOutcome> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut holders: Vec<Project> = super::projects::of_station(&tx, &query.station)?
                .into_iter()
                .filter(|p| {
                    p.git.sync
                        && match &query.project {
                            Some(id) => *id == p.project_id,
                            None => Path::new(&query.cwd).starts_with(&p.path),
                        }
                })
                .collect();
            // The longest path first, then the oldest project.
            holders.sort_by(|a, b| {
                b.path
                    .len()
                    .cmp(&a.path.len())
                    .then(a.created_at.cmp(&b.created_at))
                    .then(a.project_id.cmp(&b.project_id))
            });
            let Some(p) = holders.into_iter().next() else {
                return Ok(StartOutcome::Plain);
            };
            if query.feature == LinkFeature::Without {
                return Ok(StartOutcome::Unsupported);
            }
            let Some(slug) = query.slug.clone() else {
                return Ok(StartOutcome::StationInfoMissing);
            };
            let (Some(repo), Some(dest), Some(url)) = (&p.repo, &p.remote_dest, &p.remote_url)
            else {
                return Ok(StartOutcome::Unverified {
                    path: p.path.clone(),
                    repo: p.repo.clone().unwrap_or_default(),
                });
            };
            let checks = checks_of(&tx, &query.station)?;
            let Some(check) = verified(&checks, &p.project_id) else {
                return Ok(StartOutcome::Unverified {
                    path: p.path.clone(),
                    repo: repo.clone(),
                });
            };
            if let Some(other) = slug_reservations::reserve(&tx, repo, &slug, &query.station, now)?
            {
                return Ok(StartOutcome::SlugTaken {
                    repo: repo.clone(),
                    slug: slug.clone(),
                    other: slug_reservations::station_name(&tx, &other)?,
                });
            }
            let start = FleetGitStart {
                kind: query.kind,
                project_id: p.project_id.clone(),
                project_name: p.name.clone(),
                settings: p.git.clone(),
                settings_revision: p.git_revision,
                home_generation: PHASE1_GENERATION,
                home_path: check
                    .canonical_path
                    .clone()
                    .unwrap_or_else(|| p.path.clone()),
                remote: RemoteIdentity {
                    repo: repo.clone(),
                    dest: dest.clone(),
                },
                slug,
                forge: p.forge.clone(),
                clone_url: clone_url(url, p.remote_ssh_user.as_deref()),
            };
            tx.commit()?;
            Ok(StartOutcome::Attach(Box::new(start)))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
