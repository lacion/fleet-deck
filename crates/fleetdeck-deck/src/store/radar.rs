//! The conflict radar on the deck (design §10, §8.6; FG-7): the radar home, the
//! branches of a run, the `radar_pairs` rows and the answer of
//! `GET /api/radar`.
//!
//! - [`effective_radar_home`]: the radar home of a project, the default of
//!   §10.1 included. The snapshot build and the radar task both use it.
//! - [`DeckStore::radar_inputs`]: for each sync project, the pushed branches
//!   of its live sessions and the station that can run the radar now.
//! - [`DeckStore::radar_replace`]: the rows of one run replace the rows of
//!   the project (§10.3).
//! - [`DeckStore::radar_groups`]: the "will conflict" rows and the DL-306
//!   "same files" rows, grouped by project and group key (§8.6, §10.5).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::conflicts::{CONFLICT_WINDOW_MS, FileConflict};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    GitHomeState, PushState, RADAR_DEFAULT_KEY, RadarBranch, RadarResult,
};
use fleetdeck_proto::integrations::Project;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::params;
use serde::{Serialize, Serializer};

use super::conflicts::keyed_conflicts;
use super::project_sessions::project_of;
use super::{DeckStore, LinkFeature, Result, call};

/// The radar home of `project` (design §10.1, O21): `settings.radar_home`;
/// else the first home (by position) on a station with the label
/// `always_on`; else the first home. `None` for a project with no home.
#[must_use]
pub fn effective_radar_home(project: &Project, always_on: &BTreeSet<String>) -> Option<StationId> {
    if let Some(station) = &project.git.radar_home {
        return Some(station.clone());
    }
    let mut homes: Vec<_> = project.homes.iter().collect();
    homes.sort_by_key(|h| h.position);
    homes
        .iter()
        .find(|h| always_on.contains(h.station_id.as_str()))
        .or_else(|| homes.first())
        .map(|h| h.station_id.clone())
}

/// A session in an answer of `GET /api/radar` (decision Q15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRef {
    pub station_id: StationId,
    pub session_id: SessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The `b` side of a will-conflict row: a session, or the default branch,
/// which serializes as the string `"default"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadarSide {
    Session(SessionRef),
    Default,
}

impl Serialize for RadarSide {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Session(s) => s.serialize(serializer),
            Self::Default => serializer.serialize_str("default"),
        }
    }
}

/// Two branches that a merge cannot join (design §10.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WillConflict {
    pub a: SessionRef,
    pub b: RadarSide,
    pub files: Vec<String>,
    pub checked_at: i64,
}

/// One group of `GET /api/radar` (design §10.5, §8.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RadarGroup {
    pub project_id: Option<String>,
    pub group_key: String,
    pub will_conflict: Vec<WillConflict>,
    pub same_files: Vec<FileConflict>,
}

/// A line for the footer of a project in the radar panel (design §10.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RadarNotice {
    pub project_id: String,
    pub code: String,
    pub text: String,
}

/// What the radar task needs to know of one sync project.
#[derive(Debug, Clone)]
pub struct RadarInput {
    pub project_id: String,
    /// The station that can run the radar now: the radar home with a `ready`
    /// home of the project and a link with `fleet_git.land.v1`.
    pub station: Option<StationId>,
    /// The name of the radar home station (for the notices).
    pub home_name: String,
    /// The radar home has no live link.
    pub offline: bool,
    /// The newest `checked_at` of the stored rows.
    pub last_checked: Option<i64>,
    /// The live pushed branches, newest first, at most 50, each with the
    /// station of its session.
    pub branches: Vec<(RadarBranch, StationId)>,
}

/// `HH:MM UTC` of a Unix ms time.
fn clock_text(ms: i64) -> String {
    let day = ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02} UTC", day / 3600, day % 3600 / 60)
}

/// The offline line of a project (design §10.6).
fn offline_notice(input: &RadarInput) -> Option<RadarNotice> {
    if !input.offline || (input.last_checked.is_none() && input.branches.is_empty()) {
        return None;
    }
    let text = match input.last_checked {
        Some(at) => format!(
            "Radar: {} is offline; results from {}.",
            input.home_name,
            clock_text(at)
        ),
        None => format!("Radar: {} is offline.", input.home_name),
    };
    Some(RadarNotice {
        project_id: input.project_id.clone(),
        code: "station_offline".to_owned(),
        text,
    })
}

fn is_live(state: SessionState) -> bool {
    !matches!(state, SessionState::Exited | SessionState::Lost)
}

type Groups = BTreeMap<(Option<String>, String), RadarGroup>;

/// The group of `(project, key)`, made when it is new.
fn group_of<'a>(groups: &'a mut Groups, project: Option<&str>, key: String) -> &'a mut RadarGroup {
    groups
        .entry((project.map(str::to_owned), key.clone()))
        .or_insert_with(|| RadarGroup {
            project_id: project.map(str::to_owned),
            group_key: key,
            will_conflict: Vec::new(),
            same_files: Vec::new(),
        })
}

fn session_ref(card: &DeckSession) -> SessionRef {
    SessionRef {
        station_id: card.station_id.clone(),
        session_id: card.summary.session_id.clone(),
        callsign: card.summary.callsign.clone(),
        title: card.summary.title.clone(),
    }
}

impl DeckStore {
    /// The stations that report the label `always_on` (design §10.1).
    pub(super) fn always_on_stations(&self) -> BTreeSet<String> {
        self.station_infos()
            .into_iter()
            .filter(|(_, (info, _))| {
                info.traits
                    .as_ref()
                    .is_some_and(|t| t.labels.iter().any(|l| l == "always_on"))
            })
            .map(|(id, _)| id.as_str().to_owned())
            .collect()
    }

    /// The radar input of each sync project (design §10.2, decision Q14).
    ///
    /// A session is in a run when it is live, its `push.state` is `pushed`,
    /// `failing` or `stopped`, and `push.oid` is set. The newest session (the
    /// highest `updated_at`, then the lowest session id) comes first.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn radar_inputs(&self) -> Result<Vec<RadarInput>> {
        let projects = self.projects().await?;
        let assignments = self.project_assignments().await?;
        let cards = self.sessions().await?;
        let names: HashMap<String, String> = self
            .stations()
            .await?
            .into_iter()
            .map(|s| (s.station_id.as_str().to_owned(), s.name))
            .collect();
        let checked: HashMap<String, i64> = call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(
                "SELECT project_id, max(checked_at) FROM radar_pairs GROUP BY project_id",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await?;
        let features = self.land_features();
        let always_on = self.always_on_stations();
        let mut branches: HashMap<String, Vec<(i64, String, StationId, String)>> = HashMap::new();
        for card in &cards {
            let summary = &card.summary;
            let Some(push) = summary
                .push
                .as_ref()
                .filter(|p| p.oid.is_some())
                .filter(|p| {
                    matches!(
                        p.state,
                        PushState::Pushed | PushState::Failing | PushState::Stopped
                    )
                })
            else {
                continue;
            };
            if !is_live(summary.state) {
                continue;
            }
            if let Some(project) = project_of(&projects, &assignments, card) {
                branches.entry(project).or_default().push((
                    summary.updated_at,
                    summary.session_id.as_str().to_owned(),
                    card.station_id.clone(),
                    push.ref_name.clone(),
                ));
            }
        }
        let mut out = Vec::new();
        for p in projects.iter().filter(|p| p.git.sync) {
            let home = effective_radar_home(p, &always_on);
            let feature = home
                .as_ref()
                .and_then(|s| features.get(s.as_str()).copied())
                .unwrap_or(LinkFeature::Down);
            let ready = home.as_ref().is_some_and(|s| {
                p.homes
                    .iter()
                    .any(|h| h.station_id == *s && h.state == GitHomeState::Ready)
            });
            let mut mine = branches.remove(&p.project_id).unwrap_or_default();
            mine.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let mut seen = BTreeSet::new();
            let branches = mine
                .into_iter()
                .filter(|(_, session, ..)| seen.insert(session.clone()))
                .take(fleetdeck_proto::fleet_git::RADAR_MAX_BRANCHES)
                .map(|(_, session, station, ref_name)| {
                    (
                        RadarBranch {
                            key: session,
                            ref_name,
                        },
                        station,
                    )
                })
                .collect();
            out.push(RadarInput {
                project_id: p.project_id.clone(),
                station: home
                    .clone()
                    .filter(|_| ready && feature == LinkFeature::Negotiated),
                home_name: home
                    .as_ref()
                    .map(|s| {
                        names
                            .get(s.as_str())
                            .cloned()
                            .unwrap_or_else(|| s.to_string())
                    })
                    .unwrap_or_default(),
                offline: home.is_some() && feature == LinkFeature::Down,
                last_checked: checked.get(&p.project_id).copied(),
                branches,
            });
        }
        Ok(out)
    }

    /// The offline notices of the projects whose radar home has no link
    /// (design §10.6). They are computed at read time.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn radar_offline(&self) -> Result<Vec<RadarNotice>> {
        Ok(self
            .radar_inputs()
            .await?
            .iter()
            .filter_map(offline_notice)
            .collect())
    }

    /// The project of a session, for the trigger after `session.push`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn radar_project_of(
        &self,
        station: &StationId,
        session: &str,
    ) -> Result<Option<String>> {
        let projects = self.projects().await?;
        let assignments = self.project_assignments().await?;
        let card = self
            .sessions()
            .await?
            .into_iter()
            .find(|c| c.station_id == *station && c.summary.session_id.as_str() == session);
        Ok(card.and_then(|c| project_of(&projects, &assignments, &c)))
    }

    /// Replaces the rows of `project` with one row per pair of `result`
    /// (design §10.3). `sides` maps each branch key (a session id) to its
    /// station; a pair with an unknown key is left out. The key
    /// `@default` is the station `''`.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn radar_replace(
        &self,
        project: String,
        result: RadarResult,
        sides: BTreeMap<String, String>,
    ) -> Result<()> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM radar_pairs WHERE project_id = ?1", [&project])?;
            for pair in &result.pairs {
                let Some(a_station) = sides.get(&pair.a) else {
                    continue;
                };
                let b_station = if pair.b == RADAR_DEFAULT_KEY {
                    ""
                } else if let Some(station) = sides.get(&pair.b) {
                    station
                } else {
                    continue;
                };
                tx.execute(
                    "INSERT OR REPLACE INTO radar_pairs (project_id, a_station, a_session,
                         b_station, b_session, conflict, files, checked_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        project,
                        a_station,
                        pair.a,
                        b_station,
                        pair.b,
                        i64::from(pair.conflict),
                        serde_json::to_string(&pair.files)?,
                        result.checked_at,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// Deletes the rows of `project`; `true` when there were some.
    ///
    /// # Errors
    /// A database error.
    pub async fn radar_delete(&self, project: String) -> Result<bool> {
        let n = call(&self.inner.db, move |conn| {
            Ok(conn.execute("DELETE FROM radar_pairs WHERE project_id = ?1", [&project])?)
        })
        .await?;
        if n > 0 {
            self.board_changed();
        }
        Ok(n > 0)
    }

    /// Deletes the rows of each project that is not in `keep` (a project that
    /// was removed or has sync off).
    ///
    /// # Errors
    /// A database error.
    pub async fn radar_prune(&self, keep: Vec<String>) -> Result<()> {
        let n = call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare("SELECT DISTINCT project_id FROM radar_pairs")?;
            let ids: Vec<String> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            let mut n = 0;
            for id in ids.iter().filter(|id| !keep.contains(id)) {
                n += conn.execute("DELETE FROM radar_pairs WHERE project_id = ?1", [id])?;
            }
            Ok(n)
        })
        .await?;
        if n > 0 {
            self.board_changed();
        }
        Ok(())
    }

    /// The groups of `GET /api/radar` (design §8.6, §10.5, decision Q15).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn radar_groups(&self) -> Result<Vec<RadarGroup>> {
        let projects = self.projects().await?;
        let assignments = self.project_assignments().await?;
        let cards: HashMap<(String, String), DeckSession> = self
            .sessions()
            .await?
            .into_iter()
            .map(|c| {
                (
                    (
                        c.station_id.as_str().to_owned(),
                        c.summary.session_id.as_str().to_owned(),
                    ),
                    c,
                )
            })
            .collect();
        let since = self.now_ms().saturating_sub(CONFLICT_WINDOW_MS);
        let (pairs, touched) = call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT project_id, a_station, a_session, b_station, b_session, files, checked_at
                 FROM radar_pairs WHERE conflict = 1
                 ORDER BY checked_at DESC, a_station, a_session, b_station, b_session",
            )?;
            let pairs = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                        (r.get::<_, String>(3)?, r.get::<_, String>(4)?),
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((pairs, keyed_conflicts(conn, since)?))
        })
        .await?;
        let mut groups = Groups::new();
        for (project_id, (a_station, a_session), (b_station, b_session), files, checked_at) in pairs
        {
            let Some(project) = projects.iter().find(|p| p.project_id == project_id) else {
                continue;
            };
            let Some(a) = cards.get(&(a_station, a_session)).map(session_ref) else {
                continue;
            };
            let b = if b_session == RADAR_DEFAULT_KEY {
                RadarSide::Default
            } else {
                match cards.get(&(b_station, b_session)) {
                    Some(card) => RadarSide::Session(session_ref(card)),
                    None => continue,
                }
            };
            let files: Vec<String> = serde_json::from_str(&files)?;
            let key = format!("origin:{}", project.repo.clone().unwrap_or_default());
            group_of(&mut groups, Some(&project_id), key)
                .will_conflict
                .push(WillConflict {
                    a,
                    b,
                    files,
                    checked_at,
                });
        }
        for (key, conflict) in touched {
            let project = conflict.touches.first().and_then(|t| {
                cards
                    .get(&(
                        t.station_id.as_str().to_owned(),
                        t.session_id.as_str().to_owned(),
                    ))
                    .and_then(|card| project_of(&projects, &assignments, card))
            });
            group_of(&mut groups, project.as_deref(), key)
                .same_files
                .push(conflict);
        }
        let name_of = |id: &Option<String>| {
            id.as_ref().and_then(|id| {
                projects
                    .iter()
                    .find(|p| p.project_id == *id)
                    .map(|p| p.name.clone())
            })
        };
        let mut out: Vec<RadarGroup> = groups.into_values().collect();
        // By project name, "No project" last, then by group key.
        out.sort_by(|a, b| {
            let (an, bn) = (name_of(&a.project_id), name_of(&b.project_id));
            (an.is_none(), &an, &a.group_key, &a.project_id).cmp(&(
                bn.is_none(),
                &bn,
                &b.group_key,
                &b.project_id,
            ))
        });
        Ok(out)
    }
}

#[cfg(test)]
#[path = "radar_tests.rs"]
mod tests;
