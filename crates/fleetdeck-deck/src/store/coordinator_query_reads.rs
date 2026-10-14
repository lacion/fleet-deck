//! The ADR-008 read queries of a coordinator (§4.1, §4.3): `session_tail`,
//! `session_diff`, `needs_you`, `projects`, `project_items`, `artifacts`,
//! `artifact`, `file_read` and `file_search`.
//!
//! [`DeckStore::read_query`] runs after the class and setting checks of
//! [`super::coordinator_query`]. It does the target rule, reads what the
//! deck holds, and returns either the answer or a [`StationRead`] that the
//! link layer serves from the station. Text that comes from a session, a
//! file, a diff, an issue, a PR or an artifact sits in the untrusted
//! envelope; ids, counts and states stay outside it.

use fleetdeck_proto::artifacts::{ArtifactGetQuery, ArtifactKind};
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::files::{
    CoordinatorDiffQuery, FileReadQuery, FileRoot, FileSearchQuery, SearchMode,
};
use fleetdeck_proto::integrations::{IssueSearch, PrsList};
use fleetdeck_proto::mutation::{CoordinatorQuery, ProjectItemKind};
use fleetdeck_proto::untrusted::{fence, fence_cut};
use fleetdeck_proto::{Reject, RejectCode, SessionId, StationId};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::coordinator::strict_refusal;
use super::coordinator_ops::{in_lineage, session_known};
use super::coordinator_query::{QueryAnswer, ReadShape, StationRead};
use super::grants::Grant;
use super::{DeckStore, Result, call};
use crate::http::projects_routes::{binding, forge_of};

/// The result of one read query.
type Read = std::result::Result<QueryAnswer, Reject>;

const TAIL_MAX_ITEMS: u32 = 200;
/// The longest fenced text of `session_tail`, and of one open request, in
/// characters.
const TAIL_CHARS: usize = 64 * 1024;
const REQUEST_CHARS: usize = 8 * 1024;
const FILE_BYTES: u64 = 256 * 1024;
const SEARCH_HITS: u32 = 50;
const ISSUE_LIMIT: u32 = 50;
const MAX_REQUESTS: usize = 100;
const MAX_ARTIFACTS: usize = 200;

fn no(code: RejectCode, message: &str) -> Result<Read> {
    Ok(Err(Reject::new(code, message)))
}

/// A failed `binding` or `forge_of` of the board routes.
fn binding_refusal(e: &crate::http::guard::ApiError) -> Reject {
    Reject::new(
        RejectCode::InvalidState,
        format!("{}: {}", e.code, e.message),
    )
}

/// The newest `n` rows of the timeline of a session, oldest first, without
/// the streaming deltas. Each value is the `event` of the stored envelope.
fn timeline_tail(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
    n: u32,
) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT kind, envelope FROM timeline WHERE station_id = ?1 AND session_id = ?2
         ORDER BY station_seq DESC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![station.as_str(), session.as_str(), n], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (kind, text) in rows.into_iter().rev() {
        if kind.ends_with(".delta") {
            continue;
        }
        let mut envelope: Value = serde_json::from_str(&text)?;
        out.push(
            envelope
                .get_mut("event")
                .map(Value::take)
                .unwrap_or_default(),
        );
    }
    Ok(out)
}

/// Rule 1 of ADR-008 §4.2 for a session id: the station that holds it.
fn resolve_session(
    conn: &Connection,
    grant: &Grant,
    session: &SessionId,
) -> Result<std::result::Result<StationId, Reject>> {
    let mut found = Vec::new();
    for station in &grant.stations {
        if session_known(conn, station, session)? {
            found.push(station.clone());
        }
    }
    let [station] = found.as_slice() else {
        let (code, message) = if found.is_empty() {
            (
                RejectCode::UnknownSession,
                "no such session on a station of the grant",
            )
        } else {
            (
                RejectCode::InvalidCommand,
                "the session id is on more than one station of the grant",
            )
        };
        return Ok(Err(Reject::new(code, message)));
    };
    if grant.scope == GrantScope::Lineage && !in_lineage(conn, grant, station, session)? {
        return Ok(Err(Reject::new(
            RejectCode::Forbidden,
            "the session is not on a station of the grant or not in its lineage",
        )));
    }
    Ok(Ok(station.clone()))
}

impl DeckStore {
    /// Answers one of the nine ADR-008 read queries for `grant`.
    pub(super) async fn read_query(&self, grant: &Grant, query: &CoordinatorQuery) -> Result<Read> {
        match query {
            CoordinatorQuery::SessionTail {
                session_id,
                max_items,
            } => self.session_tail(grant, session_id, *max_items).await,
            CoordinatorQuery::SessionDiff { session_id } => {
                let station = match self.resolve(grant, session_id).await? {
                    Ok(station) => station,
                    Err(r) => return Ok(Err(r)),
                };
                let params = json!(CoordinatorDiffQuery {
                    session_id: session_id.clone()
                });
                Ok(Ok(QueryAnswer::Station(StationRead {
                    source: format!("diff:{station}/{session_id}"),
                    station,
                    method: "files.coordinator_diff",
                    params,
                    shape: ReadShape::SessionDiff {
                        session_id: session_id.clone(),
                    },
                })))
            }
            CoordinatorQuery::NeedsYou => self.needs_you(grant).await,
            CoordinatorQuery::Projects => self.read_projects(grant).await,
            CoordinatorQuery::ProjectItems { project_id, kind } => {
                self.project_items(grant, project_id, *kind).await
            }
            CoordinatorQuery::Artifacts { session_id } => {
                self.read_artifacts(grant, session_id.as_ref()).await
            }
            CoordinatorQuery::Artifact {
                artifact_id,
                version,
            } => self.read_artifact(grant, artifact_id, *version).await,
            CoordinatorQuery::FileRead { session_id, path } => {
                let station = match self.resolve(grant, session_id).await? {
                    Ok(station) => station,
                    Err(r) => return Ok(Err(r)),
                };
                let params = json!(FileReadQuery {
                    root: FileRoot::Session(session_id.clone()),
                    path: path.clone(),
                    max_bytes: Some(FILE_BYTES),
                });
                Ok(Ok(QueryAnswer::Station(StationRead {
                    source: format!("file:{station}/{session_id}/{path}"),
                    station,
                    method: "files.read",
                    params,
                    shape: ReadShape::FileRead {
                        session_id: session_id.clone(),
                        path: path.clone(),
                    },
                })))
            }
            CoordinatorQuery::FileSearch {
                session_id,
                pattern,
                mode,
            } => {
                let mode = match mode.as_str() {
                    "name" => SearchMode::Name,
                    "content" => SearchMode::Content,
                    _ => return no(RejectCode::InvalidCommand, "mode is name or content"),
                };
                let station = match self.resolve(grant, session_id).await? {
                    Ok(station) => station,
                    Err(r) => return Ok(Err(r)),
                };
                let params = json!(FileSearchQuery {
                    root: FileRoot::Session(session_id.clone()),
                    query: pattern.clone(),
                    mode,
                    max_hits: Some(SEARCH_HITS),
                });
                Ok(Ok(QueryAnswer::Station(StationRead {
                    source: format!("search:{station}/{session_id}"),
                    station,
                    method: "files.search",
                    params,
                    shape: ReadShape::FileSearch {
                        session_id: session_id.clone(),
                    },
                })))
            }
            // The other queries are answered by `coordinator_query`.
            _ => no(RejectCode::Unsupported, "not a read query"),
        }
    }

    async fn resolve(
        &self,
        grant: &Grant,
        session: &SessionId,
    ) -> Result<std::result::Result<StationId, Reject>> {
        let (grant, session) = (grant.clone(), session.clone());
        call(&self.inner.db, move |conn| {
            resolve_session(conn, &grant, &session)
        })
        .await
    }

    async fn session_tail(
        &self,
        grant: &Grant,
        session: &SessionId,
        max_items: u32,
    ) -> Result<Read> {
        if !(1..=TAIL_MAX_ITEMS).contains(&max_items) {
            return no(RejectCode::InvalidCommand, "max_items is 1 to 200");
        }
        let station = match self.resolve(grant, session).await? {
            Ok(station) => station,
            Err(r) => return Ok(Err(r)),
        };
        let (st, se) = (station.clone(), session.clone());
        let events = call(&self.inner.db, move |conn| {
            timeline_tail(conn, &st, &se, max_items)
        })
        .await?;
        let text = serde_json::to_string(&events)?;
        let items = fence_cut(&format!("session:{station}/{session}"), &text, TAIL_CHARS);
        Ok(Ok(QueryAnswer::Value(json!({
            "station": station, "session_id": session,
            "count": events.len(), "items": items,
        }))))
    }

    async fn needs_you(&self, grant: &Grant) -> Result<Read> {
        let now = self.now_ms();
        let sessions: Vec<_> = self
            .sessions()
            .await?
            .into_iter()
            .filter(|s| grant.stations.contains(&s.station_id))
            .collect();
        let pairs: Vec<(StationId, SessionId)> = sessions
            .iter()
            .map(|s| (s.station_id.clone(), s.summary.session_id.clone()))
            .collect();
        let grant = grant.clone();
        // The sessions that pass every rule of ADR-008 §4.2.
        let allowed: Vec<bool> = call(&self.inner.db, move |conn| {
            let mut out = Vec::with_capacity(pairs.len());
            for (station, session) in &pairs {
                let refusal = strict_refusal(conn, &grant, (station, session), (true, now))?;
                out.push(refusal.is_none());
            }
            Ok(out)
        })
        .await?;
        let mut found: Vec<(i64, Value)> = Vec::new();
        for (s, _) in sessions.iter().zip(&allowed).filter(|(_, ok)| **ok) {
            let (station, summary) = (&s.station_id, &s.summary);
            let session = &summary.session_id;
            let title = summary
                .title
                .clone()
                .or_else(|| summary.callsign.clone())
                .unwrap_or_else(|| session.to_string());
            let title = fence(&format!("session:{station}/{session}"), &title);
            for r in &summary.open_requests {
                let mut text = r.details.clone();
                if let Some(input) = &r.input {
                    text.push('\n');
                    text.push_str(&serde_json::to_string(input)?);
                }
                let source = format!("request:{station}/{session}/{}", r.request_id);
                let options: Vec<Value> = r
                    .options
                    .iter()
                    .map(|o| {
                        let label = fence(&format!("{source}/option"), &o.label);
                        json!({"option_id": o.option_id, "label": label, "kind": o.kind})
                    })
                    .collect();
                let tool = fence(&format!("{source}/title"), &r.title);
                found.push((
                    r.requested_at,
                    json!({
                        "station": station, "session_id": session, "title": title,
                        "request_id": r.request_id, "request_kind": r.request_kind,
                        "tool": tool,
                        "input": fence_cut(&source, &text, REQUEST_CHARS),
                        "options": options,
                        "age_ms": now.saturating_sub(r.requested_at).max(0),
                    }),
                ));
            }
        }
        found.sort_by_key(|(at, _)| *at);
        found.truncate(MAX_REQUESTS);
        let requests: Vec<Value> = found.into_iter().map(|(_, v)| v).collect();
        Ok(Ok(QueryAnswer::Value(json!({ "requests": requests }))))
    }

    async fn read_projects(&self, grant: &Grant) -> Result<Read> {
        let projects: Vec<Value> = self
            .projects()
            .await?
            .into_iter()
            .filter(|p| grant.stations.iter().any(|s| s.as_str() == p.station_id))
            .map(|p| json!({"project_id": p.project_id, "name": p.name, "station": p.station_id}))
            .collect();
        Ok(Ok(QueryAnswer::Value(json!({ "projects": projects }))))
    }

    async fn project_items(
        &self,
        grant: &Grant,
        project_id: &str,
        kind: ProjectItemKind,
    ) -> Result<Read> {
        let Some(project) = self.project(project_id.to_owned()).await? else {
            return no(RejectCode::UnknownRequest, "no such project");
        };
        if !grant
            .stations
            .iter()
            .any(|s| s.as_str() == project.station_id)
        {
            return no(
                RejectCode::Forbidden,
                "the project is not on a station of the grant",
            );
        }
        let (method, params, label) = match kind {
            ProjectItemKind::Issues => {
                let binding = match binding(&project, None) {
                    Ok(binding) => binding,
                    Err(e) => return Ok(Err(binding_refusal(&e))),
                };
                let search = IssueSearch {
                    binding,
                    query: None,
                    mine: false,
                    status: None,
                    limit: Some(ISSUE_LIMIT),
                };
                ("issues.search", json!(search), "issues")
            }
            ProjectItemKind::Prs => {
                let forge = match forge_of(&project) {
                    Ok(forge) => forge,
                    Err(e) => return Ok(Err(binding_refusal(&e))),
                };
                ("prs.list", json!(PrsList { forge }), "prs")
            }
        };
        Ok(Ok(QueryAnswer::Station(StationRead {
            station: StationId::from(project.station_id),
            method,
            params,
            shape: ReadShape::ProjectItems {
                project_id: project_id.to_owned(),
                kind,
            },
            source: format!("{label}:{project_id}"),
        })))
    }

    async fn read_artifacts(&self, grant: &Grant, session: Option<&SessionId>) -> Result<Read> {
        let mut rows = Vec::new();
        if let Some(session) = session {
            let station = match self.resolve(grant, session).await? {
                Ok(station) => station,
                Err(r) => return Ok(Err(r)),
            };
            rows = self
                .artifacts(Some(station), Some(session.clone()), None)
                .await?;
        } else {
            for station in &grant.stations {
                rows.extend(self.artifacts(Some(station.clone()), None, None).await?);
            }
        }
        rows.sort_by(|a, b| (b.updated_at, &b.artifact_id).cmp(&(a.updated_at, &a.artifact_id)));
        rows.truncate(MAX_ARTIFACTS);
        let artifacts: Vec<Value> = rows
            .into_iter()
            .map(|a| {
                let source = format!("artifact:{}/{}", a.station_id, a.artifact_id);
                json!({
                    "station": a.station_id, "artifact_id": a.artifact_id,
                    "session_id": a.session_id, "kind": a.kind,
                    "latest_version": a.latest_version,
                    "title": fence(&source, &a.title),
                })
            })
            .collect();
        Ok(Ok(QueryAnswer::Value(json!({ "artifacts": artifacts }))))
    }

    async fn read_artifact(
        &self,
        grant: &Grant,
        artifact_id: &str,
        version: Option<u32>,
    ) -> Result<Read> {
        let mut found = Vec::new();
        for station in &grant.stations {
            if let Some(a) = self
                .artifact(station.clone(), artifact_id.to_owned())
                .await?
            {
                found.push((station.clone(), a));
            }
        }
        let [(station, artifact)] = found.as_slice() else {
            return if found.is_empty() {
                no(
                    RejectCode::UnknownRequest,
                    "no such artifact on a station of the grant",
                )
            } else {
                no(
                    RejectCode::InvalidCommand,
                    "the artifact id is on more than one station of the grant",
                )
            };
        };
        if matches!(artifact.kind, ArtifactKind::Image | ArtifactKind::Pdf) {
            return no(
                RejectCode::InvalidCommand,
                "only text artifacts can be read",
            );
        }
        Ok(Ok(QueryAnswer::Station(StationRead {
            station: station.clone(),
            method: "artifacts.get",
            params: json!(ArtifactGetQuery {
                artifact_id: artifact_id.to_owned(),
                version,
            }),
            shape: ReadShape::Artifact {
                kind: artifact.kind,
                artifact_id: artifact_id.to_owned(),
                version,
            },
            source: format!("artifact:{station}/{artifact_id}"),
        })))
    }
}
