//! The projections of the fleet view (MCP P3 design §2.3): the complete,
//! sorted, field-limited item list of each view, and the `me` answer.
//!
//! An item is built from typed structs of `fleetdeck_proto::fleet_view`, so
//! no field outside the design can leave the deck. A text field is cut to its
//! limit; an item with an ID above 128 bytes or more than 8 KiB serialized is
//! left out (with a debug line). No character in a name leaves an item out.

use std::collections::{BTreeSet, HashMap};

use fleetdeck_proto::CardStatus;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::StationId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_view::{
    CALLSIGN_MAX_CHARS, GRANT_CLASSES_MAX, GRANT_STATIONS_MAX, ID_MAX_BYTES, ITEM_MAX_BYTES,
    MAX_MODES_MAX, MODE_MAX_CHARS, MODEL_MAX_CHARS, MeGrant, MeItem, NAME_MAX_CHARS,
    PLATFORM_MAX_CHARS, PROJECT_NAME_MAX_CHARS, PROVIDER_INSTANCE_MAX_CHARS, PROVIDER_MAX_CHARS,
    PROVIDER_VERSION_MAX_CHARS, PROVIDERS_MAX, ProjectItem, ProviderItem, SessionItem, StationItem,
    TITLE_MAX_CHARS, cut,
};
use fleetdeck_proto::link::StationInfo;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::grant_scopes::in_effect;
use super::{DeckError, DeckStore, call, grants};

/// One item of a projection: its sort key, its JSON, the serialized bytes of
/// the JSON, and (projects) the stations that the `station` filter matches.
#[derive(Debug)]
pub(super) struct Entry {
    pub(super) key: Vec<String>,
    pub(super) value: serde_json::Value,
    pub(super) len: usize,
    pub(super) stations: Vec<StationId>,
}

/// The entry of `item`, or `None` (with a debug line) when it must be left
/// out: a serialization error, or more than [`ITEM_MAX_BYTES`].
fn entry(key: Vec<String>, item: &impl Serialize, stations: Vec<StationId>) -> Option<Entry> {
    let value = serde_json::to_value(item).ok()?;
    let len = serde_json::to_string(&value).ok()?.len();
    if len > ITEM_MAX_BYTES {
        tracing::debug!(bytes = len, "fleet view: an oversized item is left out");
        return None;
    }
    Some(Entry {
        key,
        value,
        len,
        stations,
    })
}

/// True when `id` may be listed; a longer ID leaves its item out.
fn id_fits(id: &str) -> bool {
    let fits = id.len() <= ID_MAX_BYTES;
    if !fits {
        tracing::debug!(
            bytes = id.len(),
            "fleet view: an item with a long ID is left out"
        );
    }
    fits
}

/// The platform of a station (design §2.3): `omarchy` when the desktop is
/// Omarchy; else `macos` when `traits.os` is `macos`; else `traits.os`;
/// `None` without traits.
#[must_use]
pub fn platform_of(info: &StationInfo) -> Option<String> {
    use fleetdeck_proto::desktop::DesktopKind;
    if info
        .desktop
        .as_ref()
        .is_some_and(|d| d.kind == DesktopKind::Omarchy)
    {
        return Some("omarchy".to_owned());
    }
    let os = &info.traits.as_ref()?.os;
    Some(if os == "macos" {
        "macos".to_owned()
    } else {
        cut(os, PLATFORM_MAX_CHARS)
    })
}

fn desktop_of(info: &StationInfo) -> Option<String> {
    use fleetdeck_proto::desktop::DesktopKind;
    info.desktop.as_ref().map(|d| {
        match d.kind {
            DesktopKind::Omarchy => "omarchy",
            DesktopKind::Hyprland => "hyprland",
        }
        .to_owned()
    })
}

fn providers_of(info: &StationInfo) -> (Vec<ProviderItem>, bool) {
    let mut all: Vec<_> = info.providers.iter().collect();
    all.sort_by(|a, b| a.instance.as_str().cmp(b.instance.as_str()));
    let more = all.len() > PROVIDERS_MAX;
    let items = all
        .into_iter()
        .take(PROVIDERS_MAX)
        .map(|p| ProviderItem {
            instance: cut(p.instance.as_str(), PROVIDER_INSTANCE_MAX_CHARS),
            provider: p.provider,
            version: p
                .version
                .as_deref()
                .map(|v| cut(v, PROVIDER_VERSION_MAX_CHARS)),
            signed_in: p.auth == "signed_in",
        })
        .collect();
    (items, more)
}

impl DeckStore {
    /// The `stations` projection: every paired station, by `(name,
    /// station_id)`.
    pub(super) async fn build_stations(&self) -> Result<Vec<Entry>, DeckError> {
        let records = self.stations().await?;
        let projects = self.projects().await?;
        let infos = self.station_infos();
        let mut counts: HashMap<String, u32> = HashMap::new();
        for project in &projects {
            let mut homes: BTreeSet<&str> = BTreeSet::new();
            homes.insert(project.station_id.as_str());
            homes.extend(project.homes.iter().map(|h| h.station_id.as_str()));
            for station in homes {
                let n = counts.entry(station.to_owned()).or_default();
                *n = n.saturating_add(1);
            }
        }
        let mut out = Vec::new();
        for record in records {
            if !id_fits(record.station_id.as_str()) {
                continue;
            }
            let info = infos.get(&record.station_id).map(|(info, _)| info);
            let (providers, providers_more) = info.map_or((Vec::new(), false), providers_of);
            let name = cut(&record.name, NAME_MAX_CHARS);
            let item = StationItem {
                station_id: record.station_id.clone(),
                name: name.clone(),
                online: self.is_reachable(&record.station_id),
                platform: info.and_then(platform_of),
                arch: info
                    .and_then(|i| i.traits.as_ref())
                    .map(|t| cut(&t.arch, PLATFORM_MAX_CHARS)),
                desktop: info.and_then(desktop_of),
                providers,
                providers_more,
                live_sessions: info.map_or(0, |i| i.live_sessions),
                max_sessions: info.and_then(|i| i.max_sessions),
                project_count: counts.get(record.station_id.as_str()).copied().unwrap_or(0),
            };
            let key = vec![name, record.station_id.as_str().to_owned()];
            out.extend(entry(key, &item, Vec::new()));
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// The `projects` projection: every project, by `(name, project_id)`.
    /// The `stations` of an entry are the project's own station and its
    /// homes, for the `station` filter.
    pub(super) async fn build_projects(&self) -> Result<Vec<Entry>, DeckError> {
        let records = self.stations().await?;
        let projects = self.projects().await?;
        let names: HashMap<&str, &str> = records
            .iter()
            .map(|r| (r.station_id.as_str(), r.name.as_str()))
            .collect();
        let mut out = Vec::new();
        for project in &projects {
            if !id_fits(&project.project_id) || !id_fits(&project.station_id) {
                continue;
            }
            let Some(station) = names.get(project.station_id.as_str()) else {
                tracing::debug!("fleet view: a project of an unknown station is left out");
                continue;
            };
            let name = cut(&project.name, PROJECT_NAME_MAX_CHARS);
            let item = ProjectItem {
                project_id: project.project_id.clone(),
                name: name.clone(),
                station_id: StationId::from(project.station_id.clone()),
                station: cut(station, NAME_MAX_CHARS),
            };
            let mut matches = BTreeSet::new();
            matches.insert(StationId::from(project.station_id.clone()));
            matches.extend(project.homes.iter().map(|h| h.station_id.clone()));
            let key = vec![name, project.project_id.clone()];
            out.extend(entry(key, &item, matches.into_iter().collect()));
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// The `sessions` projection: every session whose card status is not
    /// `ended`, by `(station name, callsign, session_id)`.
    pub(super) async fn build_sessions(&self) -> Result<Vec<Entry>, DeckError> {
        let records = self.stations().await?;
        let sessions = self.sessions().await?;
        let names: HashMap<&str, &str> = records
            .iter()
            .map(|r| (r.station_id.as_str(), r.name.as_str()))
            .collect();
        let mut out = Vec::new();
        for session in &sessions {
            if session.status == CardStatus::Ended {
                continue;
            }
            let summary = &session.summary;
            if !id_fits(session.station_id.as_str()) || !id_fits(summary.session_id.as_str()) {
                continue;
            }
            let Some(station) = names.get(session.station_id.as_str()) else {
                continue;
            };
            let station = cut(station, NAME_MAX_CHARS);
            let callsign = cut(
                summary.callsign.as_deref().unwrap_or(""),
                CALLSIGN_MAX_CHARS,
            );
            let item = SessionItem {
                station_id: session.station_id.clone(),
                station: station.clone(),
                session_id: summary.session_id.clone(),
                callsign: callsign.clone(),
                title: summary.title.as_deref().map(|t| cut(t, TITLE_MAX_CHARS)),
                provider: summary.provider.unwrap_or(ProviderKind::Unknown),
                status: session.status,
            };
            let key = vec![station, callsign, summary.session_id.as_str().to_owned()];
            out.extend(entry(key, &item, Vec::new()));
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// The `me` answer of the session `(station, caller)`: the session row
    /// and the in-effect grant. `None` when the session row is gone or an ID
    /// is above 128 bytes. It reads only: no `last_used_at` changes.
    pub(super) async fn build_me(
        &self,
        station: &StationId,
        caller: &SessionId,
    ) -> Result<Option<serde_json::Value>, DeckError> {
        let (st, session) = (station.clone(), caller.clone());
        let now = self.now_ms();
        let found = call(&self.inner.db, move |conn| {
            let summary: Option<String> = conn
                .query_row(
                    "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
                    params![st.as_str(), session.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(summary) = summary else {
                return Ok(None);
            };
            let name: Option<String> = conn
                .query_row(
                    "SELECT name FROM stations WHERE station_id = ?1",
                    [st.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let grant = grants::caller_grant(conn, &st, &session)?.filter(|g| in_effect(g, now));
            let mut names = BTreeSet::new();
            if let Some(grant) = &grant {
                for id in &grant.stations {
                    let found: Option<String> = conn
                        .query_row(
                            "SELECT name FROM stations WHERE station_id = ?1",
                            [id.as_str()],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(found) = found {
                        names.insert(found);
                    }
                }
            }
            Ok(Some((summary, name, grant, names)))
        })
        .await?;
        let Some((summary, name, grant, names)) = found else {
            return Ok(None);
        };
        let summary: SessionSummary = serde_json::from_str(&summary)?;
        let Some(name) = name else {
            return Ok(None);
        };
        if !id_fits(station.as_str()) || !id_fits(summary.session_id.as_str()) {
            return Ok(None);
        }
        let grant = grant.and_then(|g| {
            if !id_fits(&g.grant_id) {
                return None;
            }
            let classes = g
                .classes
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .filter_map(|v| v.as_str().map(str::to_owned))
                .take(GRANT_CLASSES_MAX)
                .collect();
            let stations = names
                .into_iter()
                .take(GRANT_STATIONS_MAX)
                .map(|n| cut(&n, NAME_MAX_CHARS))
                .collect();
            let max_modes = g
                .max_modes
                .iter()
                .take(MAX_MODES_MAX)
                .map(|(family, mode)| (cut(family, PROVIDER_MAX_CHARS), cut(mode, MODE_MAX_CHARS)))
                .collect();
            Some(MeGrant {
                grant_id: g.grant_id,
                classes,
                stations,
                scope: g.scope,
                spawn_cap: g.spawn_cap,
                spawn_used: g.spawn_used,
                max_modes,
                expires_at: g.expires_at,
            })
        });
        let item = MeItem {
            station_id: station.clone(),
            station: cut(&name, NAME_MAX_CHARS),
            session_id: summary.session_id.clone(),
            callsign: cut(
                summary.callsign.as_deref().unwrap_or(""),
                CALLSIGN_MAX_CHARS,
            ),
            title: summary.title.as_deref().map(|t| cut(t, TITLE_MAX_CHARS)),
            provider: summary.provider.unwrap_or(ProviderKind::Unknown),
            model: summary.model.as_deref().map(|m| cut(m, MODEL_MAX_CHARS)),
            mode: summary.mode.as_deref().map(|m| cut(m, MODE_MAX_CHARS)),
            grant,
        };
        // The same item limit as the other views (MCP P3 PRE-PR finding): an
        // oversized answer is left out, and the caller gets `forbidden`.
        let value = serde_json::to_value(item)?;
        let len = serde_json::to_string(&value)?.len();
        if len > ITEM_MAX_BYTES {
            tracing::debug!(bytes = len, "fleet view: an oversized me item is left out");
            return Ok(None);
        }
        Ok(Some(value))
    }
}
