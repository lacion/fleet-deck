//! The Fleet Git part of a session start on the deck (design §2.1, §8.5;
//! FG-5 phase 2, M12). The deck decides what a start does for a sync
//! project: it finds the home that owns the folder, checks that the home is
//! ready and verified, reserves the Git name of the station, and builds the
//! `FleetGitStart`. A sync-off project starts plain (O1).

use fleetdeck_proto::fleet_git::{FleetGitStart, GitHomeState, RemoteIdentity, StartKind};
use fleetdeck_proto::integrations::Project;
use rusqlite::Connection;

use super::project_homes::{HomeRow, home_on, homes_on_station, not_ready_text, path_within};
use super::projects_git::clone_url;
use super::{DeckStore, LinkFeature, Result, call, projects, slug_reservations};

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
    /// The home holds the folder, but its check is not `verified` (or the
    /// home is not the sync owner of the folder).
    Unverified {
        path: String,
        repo: String,
    },
    /// The home is still cloning or failed (`home_not_ready`): the text.
    NotReady(String),
    /// `(repo, slug)` is reserved for the station `other` (its name).
    SlugTaken {
        repo: String,
        slug: String,
        other: String,
    },
}

/// What a start asks (design §8.5).
#[derive(Debug, Clone)]
pub struct StartQuery {
    pub station: String,
    pub cwd: String,
    pub kind: StartKind,
    /// The slug of the station name, when the deck knows the name.
    pub slug: Option<String>,
    pub feature: LinkFeature,
    /// A start that names its project (the batch start): only the home of
    /// this project on the station competes, whatever the folder.
    pub project: Option<String>,
}

/// The length of the longest path of `home` that holds `cwd` (equal, or
/// above it by path components); `None` when no path holds it.
fn match_len(home: &HomeRow, cwd: &str) -> Option<usize> {
    [
        home.path.as_deref(),
        home.canonical_path.as_deref(),
        home.reserved_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|p| path_within(p, cwd))
    .map(str::len)
    .max()
}

/// The owner of the folder (design §8.5, step 1): the homes of sync projects
/// on the station, in any state, that hold `cwd`. The sync owner wins; else
/// the longest path; on a tie the oldest project.
fn find_owner(conn: &Connection, query: &StartQuery) -> Result<Option<(Project, HomeRow)>> {
    let mut found: Vec<(Project, HomeRow, usize)> = Vec::new();
    for home in homes_on_station(conn, &query.station)? {
        let Some(len) = match_len(&home, &query.cwd) else {
            continue;
        };
        let Some(p) = projects::find(conn, &home.project_id)? else {
            continue;
        };
        // A sync-off project never matches.
        if p.git.sync {
            found.push((p, home, len));
        }
    }
    found.sort_by(|a, b| {
        b.1.sync_owner
            .cmp(&a.1.sync_owner)
            .then(b.2.cmp(&a.2))
            .then(a.0.created_at.cmp(&b.0.created_at))
            .then(a.0.project_id.cmp(&b.0.project_id))
    });
    Ok(found.into_iter().next().map(|(p, h, _)| (p, h)))
}

/// The home of the named project on the station, for a batch start.
fn named_home(
    conn: &Connection,
    query: &StartQuery,
    id: &str,
) -> Result<Option<(Project, HomeRow)>> {
    let Some(p) = projects::find(conn, id)? else {
        return Ok(None);
    };
    let Some(home) = home_on(conn, id, &query.station)? else {
        return Ok(None);
    };
    Ok(Some((p, home)))
}

impl DeckStore {
    /// The `FleetGitStart` for a start on `query.station` in `query.cwd`
    /// (design §2.1, §8.5, decision B8, B20). In one transaction: the owner
    /// home is found, then checked (`ready` and sync owner), then the slug is
    /// reserved for the station.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn git_start(&self, query: StartQuery) -> Result<StartOutcome> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let holder = match &query.project {
                Some(id) => named_home(&tx, &query, id)?,
                None => find_owner(&tx, &query)?,
            };
            let Some((p, home)) = holder else {
                return Ok(StartOutcome::Plain);
            };
            if !p.git.sync {
                // A sync-off project with a ready home starts plain; one
                // whose home still clones, or failed, cannot start (B20).
                return Ok(match not_ready_text(&home.wire(), &p.name) {
                    Some(text) => StartOutcome::NotReady(text),
                    None => StartOutcome::Plain,
                });
            }
            if query.feature == LinkFeature::Without {
                return Ok(StartOutcome::Unsupported);
            }
            let Some(slug) = query.slug.clone() else {
                return Ok(StartOutcome::StationInfoMissing);
            };
            if let Some(text) = not_ready_text(&home.wire(), &p.name) {
                return Ok(StartOutcome::NotReady(text));
            }
            let (Some(repo), Some(dest), Some(url)) = (&p.repo, &p.remote_dest, &p.remote_url)
            else {
                return Ok(StartOutcome::Unverified {
                    path: home.path.clone().unwrap_or_default(),
                    repo: p.repo.clone().unwrap_or_default(),
                });
            };
            if home.state != GitHomeState::Ready || !home.sync_owner {
                return Ok(StartOutcome::Unverified {
                    path: home.path.clone().unwrap_or_default(),
                    repo: repo.clone(),
                });
            }
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
                home_generation: home.generation,
                home_path: home.folder().unwrap_or_default().to_owned(),
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
