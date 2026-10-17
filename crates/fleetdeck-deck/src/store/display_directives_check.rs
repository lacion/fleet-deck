//! The checks of a directive (R13 §5A item 3, §5B item 3, §5C item 2, §5D
//! items 4 and 5): the shape of a body, the references against the ceiling,
//! the existence of what a directive names, and the live re-check of a stored
//! row. The set operation and the live read share these functions.

use std::collections::BTreeSet;

use fleetdeck_proto::StationId;
use fleetdeck_proto::display_directive::{
    ArtifactRef, DirectiveBody, DirectiveFilter, MAX_FILTER_REPO, MAX_FILTER_STATIONS, SessionRef,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::store::{DeckError, Result};

/// The longest artifact id in a directive, in bytes.
pub const MAX_ARTIFACT_ID: usize = 128;
/// The longest batch id (the grammar of `BatchRef::problem`).
const MAX_BATCH_ID: usize = 64;

/// Why a directive is refused.
#[derive(Debug, thiserror::Error)]
pub enum DirectiveError {
    /// 400 `invalid_directive`: the text is fixed and holds no input.
    #[error("{0}")]
    Invalid(&'static str),
    /// 403 `outside_ceiling`.
    #[error("the directive points outside the ceiling of the display")]
    OutsideCeiling,
    /// 403 `not_granted`: the sender has no authority to send a directive.
    #[error("the sender is not granted to send a directive")]
    NotGranted,
    /// 404 `display_unknown`.
    #[error("no such display")]
    DisplayUnknown,
    #[error(transparent)]
    Store(#[from] DeckError),
}

impl From<rusqlite::Error> for DirectiveError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}

/// The stored `body` of a row (R2 Q1 B): the validated directive and the
/// expanded session references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Stored {
    pub directive: DirectiveBody,
    pub refs: Vec<SessionRef>,
}

fn control_free(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn check_filter(filter: &DirectiveFilter) -> std::result::Result<(), &'static str> {
    if filter.stations.len() > MAX_FILTER_STATIONS {
        return Err("the filter names too many stations");
    }
    if let Some(repo) = &filter.repo
        && (repo.is_empty()
            || repo.len() > MAX_FILTER_REPO
            || repo.contains('/')
            || !control_free(repo))
    {
        return Err("the filter repository is not a valid name");
    }
    Ok(())
}

fn batch_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_BATCH_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The shape of a body, with no database access.
///
/// # Errors
/// A fixed text for the first problem.
pub(super) fn check_shape(body: &DirectiveBody) -> std::result::Result<(), &'static str> {
    if let Some(filter) = body.filter() {
        check_filter(filter)?;
    }
    match body {
        DirectiveBody::Race { batch_id, .. } if !batch_id_ok(batch_id) => {
            Err("the batch id is not valid")
        }
        DirectiveBody::Artifact { artifact, .. }
            if artifact.version == 0
                || artifact.artifact_id.is_empty()
                || artifact.artifact_id.len() > MAX_ARTIFACT_ID
                || !control_free(&artifact.artifact_id) =>
        {
            Err("the artifact reference is not valid")
        }
        _ => Ok(()),
    }
}

/// The members of a batch, in a fixed order: every session on a station of
/// the deck whose card names the batch.
fn expand_batch(conn: &Connection, batch_id: &str) -> Result<Vec<SessionRef>> {
    let mut stmt = conn.prepare(
        "SELECT s.station_id, s.session_id FROM sessions s
         JOIN stations t ON t.station_id = s.station_id
         WHERE json_extract(s.summary, '$.batch.batch_id') = ?1
         ORDER BY s.station_id, s.session_id",
    )?;
    let rows = stmt
        .query_map(params![batch_id], |r| {
            Ok(SessionRef {
                station_id: StationId::from(r.get::<_, String>(0)?),
                session_id: r.get::<_, String>(1)?.as_str().into(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn session_exists(conn: &Connection, s: &SessionRef) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sessions s JOIN stations t ON t.station_id = s.station_id
             WHERE s.station_id = ?1 AND s.session_id = ?2",
            params![s.station_id.as_str(), s.session_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn artifact_exists(conn: &Connection, a: &ArtifactRef) -> Result<bool> {
    let latest: Option<i64> = conn
        .query_row(
            "SELECT latest_version FROM artifacts WHERE station_id = ?1 AND artifact_id = ?2",
            params![a.station_id.as_str(), a.artifact_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(latest.is_some_and(|l| i64::from(a.version) <= l))
}

fn inside(ceiling: &BTreeSet<&StationId>, station: &StationId) -> bool {
    ceiling.contains(station)
}

/// The references of a body, checked against the ceiling and then against
/// what exists now (R2 Q4 steps 5 and 6). The caller checked the shape.
///
/// # Errors
/// [`DirectiveError::OutsideCeiling`] for a station outside the ceiling;
/// [`DirectiveError::Invalid`] for a reference that does not exist.
pub(super) fn check_references(
    conn: &Connection,
    ceiling: &[StationId],
    body: &DirectiveBody,
) -> std::result::Result<Vec<SessionRef>, DirectiveError> {
    let ceiling: BTreeSet<&StationId> = ceiling.iter().collect();
    if let Some(filter) = body.filter()
        && filter.stations.iter().any(|s| !inside(&ceiling, s))
    {
        return Err(DirectiveError::OutsideCeiling);
    }
    match body {
        DirectiveBody::Mission { .. } | DirectiveBody::Ambient { .. } => Ok(Vec::new()),
        DirectiveBody::Focus { session, .. } | DirectiveBody::Review { session, .. } => {
            if !inside(&ceiling, &session.station_id) {
                return Err(DirectiveError::OutsideCeiling);
            }
            if !session_exists(conn, session)? {
                return Err(DirectiveError::Invalid("unknown reference"));
            }
            Ok(vec![session.clone()])
        }
        DirectiveBody::Race { batch_id, .. } => {
            let members = expand_batch(conn, batch_id)?;
            // Every member is authorized, not only the four columns that the
            // wall shows (R13 §5D item 5).
            if members.iter().any(|m| !inside(&ceiling, &m.station_id)) {
                return Err(DirectiveError::OutsideCeiling);
            }
            if members.is_empty() {
                return Err(DirectiveError::Invalid("unknown reference"));
            }
            Ok(members)
        }
        DirectiveBody::Artifact { artifact, .. } => {
            if !inside(&ceiling, &artifact.station_id) {
                return Err(DirectiveError::OutsideCeiling);
            }
            if !artifact_exists(conn, artifact)? {
                return Err(DirectiveError::Invalid("unknown reference"));
            }
            Ok(Vec::new())
        }
    }
}

/// The live re-check of a stored directive: true when it still holds.
pub(super) fn still_valid(
    conn: &Connection,
    ceiling: &[StationId],
    directive: &DirectiveBody,
) -> Result<bool> {
    if check_shape(directive).is_err() {
        return Ok(false);
    }
    match check_references(conn, ceiling, directive) {
        Ok(_) => Ok(true),
        Err(DirectiveError::Store(e)) => Err(e),
        Err(_) => Ok(false),
    }
}
