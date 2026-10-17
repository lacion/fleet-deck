//! Placement by station traits (FG-6, design §9.2, O19, O20): the pure choice
//! of the stations for new sessions. The candidates are the stations of
//! [`crate::routing::spawn_candidates`]; the requirements filter them, and a
//! score orders them. Routing never calls a model (O20).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use fleetdeck_proto::board::{DeckSession, StationView};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::Prefer;
use fleetdeck_proto::resources::StationResources;
use fleetdeck_proto::{CardStatus, StationId};

use crate::routing::{Needs, spawn_candidates};

/// What the placement needs (design §9.2).
#[derive(Debug, Clone, Copy)]
pub struct PlaceNeeds<'a> {
    pub requires: &'a [String],
    pub prefer: &'a [Prefer],
    pub provider: Option<ProviderKind>,
    /// The stations with a ready home of the project, when the placement is
    /// for a project; an empty list means the project has no ready home.
    pub homes: Option<&'a [StationId]>,
}

/// Why no station fits (decision D1). `text()` is the 409 `no_station`
/// message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceError {
    /// No candidate before the requirements.
    NoneFree,
    /// The first `requires` entry that no candidate meets.
    Unmet(String),
    /// Every entry is met by a candidate, but no candidate meets all.
    NoneHasAll,
    /// `homes` is set and empty.
    NoReadyHome,
}

impl PlaceError {
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::NoneFree => "No station matches: no station is free.".to_owned(),
            Self::Unmet(entry) => format!("No station matches: {entry}."),
            Self::NoneHasAll => "No station matches: no station has every requirement.".to_owned(),
            Self::NoReadyHome => "No station matches: the project has no ready home.".to_owned(),
        }
    }
}

/// True when the station meets one `requires` entry: a label of its traits,
/// `os:<os>` or `arch:<arch>`. A station without traits meets none.
fn meets(st: &StationView, entry: &str) -> bool {
    let Some(traits) = st.info.as_ref().and_then(|i| i.traits.as_ref()) else {
        return false;
    };
    if let Some(os) = entry.strip_prefix("os:") {
        traits.os == os
    } else if let Some(arch) = entry.strip_prefix("arch:") {
        traits.arch == arch
    } else {
        traits.labels.iter().any(|l| l == entry)
    }
}

/// The candidates that meet every requirement (steps 1 and 2).
fn candidates<'a>(
    stations: &'a [StationView],
    needs: &PlaceNeeds<'_>,
) -> Result<Vec<&'a StationView>, PlaceError> {
    if needs.homes.is_some_and(<[StationId]>::is_empty) {
        return Err(PlaceError::NoReadyHome);
    }
    let mut found: Vec<&StationView> = spawn_candidates(
        stations,
        &Needs {
            repo: None,
            provider: needs.provider,
        },
    )
    .into_iter()
    .filter(|st| needs.homes.is_none_or(|h| h.contains(&st.station_id)))
    .collect();
    if found.is_empty() {
        return Err(PlaceError::NoneFree);
    }
    if let Some(entry) = needs
        .requires
        .iter()
        .find(|e| !found.iter().any(|st| meets(st, e)))
    {
        return Err(PlaceError::Unmet(entry.clone()));
    }
    found.retain(|st| needs.requires.iter().all(|e| meets(st, e)));
    if found.is_empty() {
        return Err(PlaceError::NoneHasAll);
    }
    Ok(found)
}

/// The free resources of DL-193: `min(1 - load5 / cores, mem_available /
/// mem_total)`. No `resources` counts 0; a missing, zero-core or zero-total
/// part counts 0.
fn room(resources: Option<&StationResources>) -> f64 {
    let Some(r) = resources else {
        return 0.0;
    };
    let cpu = r.cpu.as_ref().map_or(0.0, |c| {
        if c.cores == 0 {
            0.0
        } else {
            1.0 - (f64::from(c.load5_x100) / 100.0) / f64::from(c.cores)
        }
    });
    let memory = r.memory.as_ref().map_or(0.0, |m| {
        if m.total_bytes == 0 {
            0.0
        } else {
            m.available_bytes as f64 / m.total_bytes as f64
        }
    });
    cpu.min(memory)
}

/// One candidate with the parts of its score that a batch does not change.
struct Scored<'a> {
    st: &'a StationView,
    satisfied: usize,
    room: f64,
    live: u64,
}

fn satisfied(st: &StationView, sessions: &[DeckSession], prefer: &[Prefer]) -> usize {
    let traits = st.info.as_ref().and_then(|i| i.traits.as_ref());
    prefer
        .iter()
        .filter(|p| match p {
            Prefer::Idle => !sessions.iter().any(|s| {
                s.station_id == st.station_id
                    && matches!(
                        s.status,
                        CardStatus::Working | CardStatus::Verifying | CardStatus::Queued
                    )
            }),
            Prefer::NotOnBattery => traits.is_some_and(|t| t.on_battery == Some(false)),
            Prefer::AlwaysOn => traits.is_some_and(|t| t.labels.iter().any(|l| l == "always_on")),
            Prefer::Unknown => false,
        })
        .count()
}

fn score<'a>(
    found: Vec<&'a StationView>,
    sessions: &[DeckSession],
    prefer: &[Prefer],
) -> Vec<Scored<'a>> {
    found
        .into_iter()
        .map(|st| Scored {
            satisfied: satisfied(st, sessions, prefer),
            room: room(st.info.as_ref().and_then(|i| i.resources.as_ref())),
            live: st.info.as_ref().map_or(0, |i| i.live_sessions),
            st,
        })
        .collect()
}

/// The score order (design §9.2 step 3): more satisfied preferences, more
/// free resources, fewer live sessions, then the station name.
fn by_score(a: &Scored<'_>, b: &Scored<'_>, extra: &BTreeMap<&StationId, u64>) -> Ordering {
    let live = |s: &Scored<'_>| s.live + extra.get(&s.st.station_id).copied().unwrap_or(0);
    b.satisfied
        .cmp(&a.satisfied)
        .then_with(|| b.room.total_cmp(&a.room))
        .then_with(|| live(a).cmp(&live(b)))
        .then_with(|| a.st.name.cmp(&b.st.name))
        .then_with(|| a.st.station_id.cmp(&b.st.station_id))
}

/// Every candidate in score order (steps 1 to 3); empty when none fits.
#[must_use]
pub fn rank(
    stations: &[StationView],
    sessions: &[DeckSession],
    needs: &PlaceNeeds<'_>,
) -> Vec<StationId> {
    let Ok(found) = candidates(stations, needs) else {
        return Vec::new();
    };
    let mut scored = score(found, sessions, needs.prefer);
    let none = BTreeMap::new();
    scored.sort_by(|a, b| by_score(a, b, &none));
    scored
        .into_iter()
        .map(|s| s.st.station_id.clone())
        .collect()
}

/// The stations for `count` new sessions, one at a time (step 4). After each
/// placement the chosen station counts one more live session. With `spread`,
/// a station that got a session of this batch sorts after every station that
/// got none. There is no capacity limit beyond the candidate filter.
///
/// # Errors
/// [`PlaceError`] when no station fits.
pub fn place(
    stations: &[StationView],
    sessions: &[DeckSession],
    needs: &PlaceNeeds<'_>,
    count: u32,
    spread: bool,
) -> Result<Vec<StationId>, PlaceError> {
    let scored = score(candidates(stations, needs)?, sessions, needs.prefer);
    let mut extra: BTreeMap<&StationId, u64> = BTreeMap::new();
    let mut placed = Vec::new();
    for _ in 0..count {
        let best = scored.iter().min_by(|a, b| {
            let got = |s: &Scored<'_>| spread && extra.contains_key(&s.st.station_id);
            got(a).cmp(&got(b)).then_with(|| by_score(a, b, &extra))
        });
        let Some(best) = best else {
            break;
        };
        *extra.entry(&best.st.station_id).or_insert(0) += 1;
        placed.push(best.st.station_id.clone());
    }
    Ok(placed)
}

#[cfg(test)]
#[path = "routing_place_tests.rs"]
mod tests;
