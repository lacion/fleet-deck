//! The projects of the board and their issue and pull request views
//! (R3, DL-164).
//!
//! - `GET/POST /api/projects`, `PUT/DELETE /api/projects/{id}`,
//!   `POST /api/projects/{id}/preflight`: owner only. A project holds a
//!   repository on a station with its forge and tracker bindings. The deck
//!   stores bindings only; it never holds a token.
//! - `GET /api/projects/{id}/issues`, `.../issues/{key}`, `.../assets`,
//!   `.../prs`, `.../prs/{n}`, `.../prs/{n}/checks`: the deck relays them to
//!   the station's `integrations.v1` methods and returns the answer. It
//!   keeps no copy. The station returns all third-party text inside the
//!   injection envelope.
//!
//! The writes and the session starts are in [`super::project_writes`].

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_link::control::Control;
use fleetdeck_proto::integrations::{
    Asset, AssetQuery, Binding, BindingCheck, ChecksFailing, ForgeBinding, IntegrationsStatus,
    IssueDetail, IssueGet, IssueList, IssueSearch, PrDetail, PrList, PrRef, Project, ProjectInput,
    ProjectList, ProjectPreflight, PrsList, StatusQuery, TrackerBinding, TrackerKind,
};
use fleetdeck_proto::link::Outcome;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, NewAudit, Principal, StationRecord};

/// The longest the board waits for the station's answer to a read. The
/// station limits each of its calls to 20 s and a view makes up to three.
pub const READ_WAIT: Duration = Duration::from_secs(75);

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message.into())
}

/// The error for a body that does not parse.
pub(super) fn bad_body(e: &JsonRejection) -> ApiError {
    bad("invalid_body", e.body_text())
}

/// The project `id`.
pub(super) async fn load(board: &Board, id: &str) -> Result<Project, ApiError> {
    board
        .store
        .project(id.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_project",
                format!("no project `{id}`"),
            )
        })
}

/// The station of a project, with a live control channel to it.
pub(super) async fn station_of(
    board: &Board,
    project: &Project,
) -> Result<(StationRecord, Control), ApiError> {
    let rec = board
        .store
        .find_station(project.station_id.clone())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                "the station of this project is not paired with this deck",
            )
        })?;
    let control = board.link.control_for(&rec.station_id).ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            format!("station `{}` has no live link to this deck", rec.name),
        )
    })?;
    Ok((rec, control))
}

/// The HTTP error for a station error code of the integration methods.
fn station_error(code: &str, message: String) -> ApiError {
    let (status, known) = match code {
        "invalid_params" => (StatusCode::BAD_REQUEST, "invalid_params"),
        "outside_roots" => (StatusCode::BAD_REQUEST, "outside_roots"),
        "unavailable" => (StatusCode::CONFLICT, "integration_unavailable"),
        "upstream" => (StatusCode::BAD_GATEWAY, "upstream"),
        "timeout" => (StatusCode::GATEWAY_TIMEOUT, "timeout"),
        "feature_not_negotiated" => (StatusCode::BAD_GATEWAY, "station_error"),
        _ => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{code}: {message}"),
            );
        }
    };
    ApiError::new(status, known, message)
}

/// Sends `method` with `params` on `control` and decodes the answer.
pub(super) async fn ask<T: DeserializeOwned>(
    control: &Control,
    method: &str,
    params: Value,
    wait: Duration,
) -> Result<T, ApiError> {
    let unreachable = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            "the link to the station ended",
        )
    };
    let outcome = tokio::time::timeout(wait, control.request(method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable())?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(station_error(&e.code, e.message)),
    }
}

/// The binding of a project that `source` names: a tracker id, `forge`, or
/// (absent) the first tracker, else the forge.
pub(super) fn binding(project: &Project, source: Option<&str>) -> Result<Binding, ApiError> {
    let forge = || {
        project.forge.clone().map(Binding::Forge).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "no_binding",
                "this project has no forge binding",
            )
        })
    };
    match source {
        Some("forge") => forge(),
        Some(id) => project
            .trackers
            .iter()
            .find(|t| t.tracker == id)
            .cloned()
            .map(Binding::Tracker)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_source",
                    format!("this project has no tracker `{id}`"),
                )
            }),
        None => project
            .trackers
            .first()
            .cloned()
            .map(Binding::Tracker)
            .map_or_else(forge, Ok),
    }
}

/// The forge of a project, for the pull request routes.
pub(super) fn forge_of(project: &Project) -> Result<ForgeBinding, ApiError> {
    project.forge.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "no_binding",
            "this project has no forge binding",
        )
    })
}

fn check_input(input: &ProjectInput) -> Result<(), ApiError> {
    input.check().map_err(|why| bad("invalid_project", why))
}

async fn station_id_of(board: &Board, name: &str) -> Result<String, ApiError> {
    board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .map(|rec| rec.station_id.0)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{name}`"),
            )
        })
}

async fn audited(board: &Board, principal: &Principal, action: &str, target: &str, detail: &str) {
    board
        .store
        .audit_or_log(
            NewAudit::new(principal.actor(), action.to_owned(), AuditResult::Ok)
                .target(target.to_owned())
                .detail(detail.to_owned()),
        )
        .await;
}

/// `GET /api/projects`.
///
/// # Errors
/// 403 `owner_only`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<ProjectList>, ApiError> {
    owner_only(&principal, "lists the projects")?;
    Ok(Json(ProjectList {
        projects: board.store.projects().await.map_err(ApiError::internal)?,
    }))
}

/// `POST /api/projects`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_project`; 403 `owner_only`; 404
/// `unknown_station`.
pub async fn create(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<ProjectInput>, JsonRejection>,
) -> Result<Json<Project>, ApiError> {
    owner_only(&principal, "creates a project")?;
    let Json(input) = body.map_err(|e| bad_body(&e))?;
    check_input(&input)?;
    let station_id = station_id_of(&board, &input.station).await?;
    let project = board
        .store
        .create_project(station_id, input)
        .await
        .map_err(ApiError::internal)?;
    audited(
        &board,
        &principal,
        "projects.create",
        &project.project_id,
        &project.name,
    )
    .await;
    Ok(Json(project))
}

/// `PUT /api/projects/{id}`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_project`; 403 `owner_only`; 404
/// `unknown_project` / `unknown_station`.
pub async fn update(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<ProjectInput>, JsonRejection>,
) -> Result<Json<Project>, ApiError> {
    owner_only(&principal, "changes a project")?;
    let Json(input) = body.map_err(|e| bad_body(&e))?;
    check_input(&input)?;
    let station_id = station_id_of(&board, &input.station).await?;
    let project = board
        .store
        .update_project(id.clone(), station_id, input)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_project",
                format!("no project `{id}`"),
            )
        })?;
    audited(&board, &principal, "projects.update", &id, &project.name).await;
    Ok(Json(project))
}

/// `DELETE /api/projects/{id}`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_project`.
pub async fn delete(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    owner_only(&principal, "deletes a project")?;
    if !board
        .store
        .delete_project(id.clone())
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_project",
            format!("no project `{id}`"),
        ));
    }
    audited(&board, &principal, "projects.delete", &id, "").await;
    Ok(StatusCode::NO_CONTENT)
}

fn tracker_label(t: &TrackerBinding) -> String {
    format!(
        "{} project {} (tracker {})",
        match t.kind {
            TrackerKind::Jira => "Jira",
            TrackerKind::Linear => "Linear",
        },
        t.key,
        t.tracker
    )
}

/// One reading check of a binding: `issues.search` with one result, and the
/// sentence for the owner.
async fn check_binding(
    control: &Control,
    binding: Binding,
    label: String,
    site: Option<String>,
) -> BindingCheck {
    let query = IssueSearch {
        binding,
        query: None,
        mine: false,
        status: None,
        limit: Some(1),
    };
    let at = site.map_or_else(String::new, |s| format!(" at {s}"));
    match ask::<IssueList>(control, "issues.search", json!(query), READ_WAIT).await {
        Ok(_) => BindingCheck {
            message: format!("the station can read {label}{at}"),
            label,
            ok: true,
        },
        Err(e) => BindingCheck {
            message: format!("the station cannot read {label}{at}: {}", e.message),
            label,
            ok: false,
        },
    }
}

/// `POST /api/projects/{id}/preflight`: the station's `integrations.status`
/// and one test search for each binding (like the Git preflight, DL-144).
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_project` / `unknown_station`; 409
/// `station_unreachable`.
pub async fn preflight(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<ProjectPreflight>, ApiError> {
    owner_only(&principal, "checks a project")?;
    let project = load(&board, &id).await?;
    let (_, control) = station_of(&board, &project).await?;
    let mut checks = Vec::new();
    let status = match ask::<IntegrationsStatus>(
        &control,
        "integrations.status",
        json!(StatusQuery {}),
        READ_WAIT,
    )
    .await
    {
        Ok(s) => Some(s),
        Err(e) => {
            checks.push(BindingCheck {
                label: "station".to_owned(),
                ok: false,
                message: format!("the station did not answer the status check: {}", e.message),
            });
            None
        }
    };
    if let Some(forge) = &project.forge {
        let label = format!("{} repository {}", forge_name(forge), forge.repo);
        let site = Some(forge.host.clone());
        checks.push(check_binding(&control, Binding::Forge(forge.clone()), label, site).await);
    }
    for tracker in &project.trackers {
        let site = status
            .as_ref()
            .and_then(|s| s.trackers.iter().find(|t| t.id == tracker.tracker))
            .and_then(|t| t.site.clone());
        checks.push(
            check_binding(
                &control,
                Binding::Tracker(tracker.clone()),
                tracker_label(tracker),
                site,
            )
            .await,
        );
    }
    let failed = checks.iter().filter(|c| !c.ok).count();
    audited(
        &board,
        &principal,
        "projects.preflight",
        &id,
        &format!("{failed} of {} checks failed", checks.len()),
    )
    .await;
    Ok(Json(ProjectPreflight { status, checks }))
}

fn forge_name(forge: &ForgeBinding) -> &'static str {
    match forge.kind {
        fleetdeck_proto::integrations::ForgeKind::Github => "GitHub",
        fleetdeck_proto::integrations::ForgeKind::Gitlab => "GitLab",
    }
}

/// The query of the issue list.
#[derive(Debug, Default, Deserialize)]
pub struct IssuesParams {
    /// A tracker id or `forge`.
    pub source: Option<String>,
    pub query: Option<String>,
    #[serde(default)]
    pub mine: bool,
    pub status: Option<String>,
    pub limit: Option<u32>,
}

/// The query of the routes that need only a source.
#[derive(Debug, Default, Deserialize)]
pub struct SourceParams {
    pub source: Option<String>,
}

/// The query of the asset route.
#[derive(Debug, Default, Deserialize)]
pub struct AssetParams {
    pub source: Option<String>,
    pub url: String,
}

/// `GET /api/projects/{id}/issues?source=&query=&mine=&status=&limit=`.
///
/// # Errors
/// 400 `invalid_params`; 404 `unknown_project` / `unknown_source`; 409
/// `station_unreachable` / `no_binding` / `integration_unavailable`; 502
/// `upstream` / `station_error`; 504 `timeout`.
pub async fn issues(
    State(board): State<Arc<Board>>,
    Path(id): Path<String>,
    Query(q): Query<IssuesParams>,
) -> Result<Json<IssueList>, ApiError> {
    let project = load(&board, &id).await?;
    let search = IssueSearch {
        binding: binding(&project, q.source.as_deref())?,
        query: q.query,
        mine: q.mine,
        status: q.status,
        limit: q.limit,
    };
    search.check().map_err(|why| bad("invalid_params", why))?;
    let (_, control) = station_of(&board, &project).await?;
    ask(&control, "issues.search", json!(search), READ_WAIT)
        .await
        .map(Json)
}

/// `GET /api/projects/{id}/issues/{key}?source=`.
///
/// # Errors
/// As [`issues`].
pub async fn issue(
    State(board): State<Arc<Board>>,
    Path((id, key)): Path<(String, String)>,
    Query(q): Query<SourceParams>,
) -> Result<Json<IssueDetail>, ApiError> {
    let project = load(&board, &id).await?;
    let get = IssueGet {
        binding: binding(&project, q.source.as_deref())?,
        key,
    };
    get.check().map_err(|why| bad("invalid_params", why))?;
    let (_, control) = station_of(&board, &project).await?;
    ask(&control, "issues.get", json!(get), READ_WAIT)
        .await
        .map(Json)
}

/// `GET /api/projects/{id}/assets?source=&url=`: one image of an issue body,
/// through the station (the deck holds no token). The answer is the content
/// type and the base64 bytes; the board shows it as a `data:` URL.
///
/// # Errors
/// As [`issues`].
pub async fn asset(
    State(board): State<Arc<Board>>,
    Path(id): Path<String>,
    Query(q): Query<AssetParams>,
) -> Result<Json<Asset>, ApiError> {
    let project = load(&board, &id).await?;
    let query = AssetQuery {
        binding: binding(&project, q.source.as_deref())?,
        url: q.url,
    };
    query.check().map_err(|why| bad("invalid_params", why))?;
    let (_, control) = station_of(&board, &project).await?;
    ask(&control, "issues.asset", json!(query), READ_WAIT)
        .await
        .map(Json)
}

/// `GET /api/projects/{id}/prs`.
///
/// # Errors
/// As [`issues`].
pub async fn prs(
    State(board): State<Arc<Board>>,
    Path(id): Path<String>,
) -> Result<Json<PrList>, ApiError> {
    let project = load(&board, &id).await?;
    let list = PrsList {
        forge: forge_of(&project)?,
    };
    let (_, control) = station_of(&board, &project).await?;
    ask(&control, "prs.list", json!(list), READ_WAIT)
        .await
        .map(Json)
}

async fn pr_call<T: DeserializeOwned>(
    board: &Board,
    id: &str,
    number: u64,
    method: &str,
) -> Result<Json<T>, ApiError> {
    let project = load(board, id).await?;
    let pr = PrRef {
        forge: forge_of(&project)?,
        number,
    };
    pr.check().map_err(|why| bad("invalid_params", why))?;
    let (_, control) = station_of(board, &project).await?;
    ask(&control, method, json!(pr), READ_WAIT).await.map(Json)
}

/// `GET /api/projects/{id}/prs/{n}`.
///
/// # Errors
/// As [`issues`].
pub async fn pr(
    State(board): State<Arc<Board>>,
    Path((id, number)): Path<(String, u64)>,
) -> Result<Json<PrDetail>, ApiError> {
    pr_call(&board, &id, number, "prs.get").await
}

/// `GET /api/projects/{id}/prs/{n}/checks`: the failing checks with the
/// last 200 log lines of each.
///
/// # Errors
/// As [`issues`].
pub async fn pr_checks(
    State(board): State<Arc<Board>>,
    Path((id, number)): Path<(String, u64)>,
) -> Result<Json<ChecksFailing>, ApiError> {
    pr_call(&board, &id, number, "prs.checks").await
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::integrations::ForgeKind;

    use super::*;

    fn project(forge: bool, trackers: &[(&str, &str)]) -> Project {
        Project {
            project_id: "p".into(),
            name: "App".into(),
            station_id: "st".into(),
            path: "/w".into(),
            forge: forge.then(|| ForgeBinding {
                kind: ForgeKind::Github,
                host: "github.com".into(),
                repo: "a/b".into(),
            }),
            trackers: trackers
                .iter()
                .map(|(id, key)| TrackerBinding {
                    tracker: (*id).into(),
                    kind: TrackerKind::Jira,
                    key: (*key).into(),
                })
                .collect(),
            prompt_template: None,
            review_prompt: None,
            created_at: 0,
        }
    }

    #[test]
    fn a_source_names_a_tracker_the_forge_or_defaults_to_the_first_tracker() {
        let p = project(true, &[("a", "PROJ"), ("b", "OPS")]);
        assert!(matches!(binding(&p, None).unwrap(), Binding::Tracker(t) if t.tracker == "a"));
        assert!(matches!(binding(&p, Some("b")).unwrap(), Binding::Tracker(t) if t.key == "OPS"));
        assert!(matches!(
            binding(&p, Some("forge")).unwrap(),
            Binding::Forge(_)
        ));
        assert_eq!(binding(&p, Some("zzz")).unwrap_err().code, "unknown_source");
        let only_forge = project(true, &[]);
        assert!(matches!(
            binding(&only_forge, None).unwrap(),
            Binding::Forge(_)
        ));
        let none = project(false, &[]);
        assert_eq!(binding(&none, None).unwrap_err().code, "no_binding");
        assert_eq!(forge_of(&none).unwrap_err().code, "no_binding");
    }

    #[test]
    fn station_errors_map_to_http_errors() {
        let code = |c: &str| station_error(c, "m".into());
        assert_eq!(code("invalid_params").status, StatusCode::BAD_REQUEST);
        assert_eq!(code("unavailable").code, "integration_unavailable");
        assert_eq!(code("upstream").status, StatusCode::BAD_GATEWAY);
        assert_eq!(code("timeout").status, StatusCode::GATEWAY_TIMEOUT);
        let other = code("surprise");
        assert_eq!(
            (other.status, other.code),
            (StatusCode::BAD_GATEWAY, "station_error")
        );
        assert!(other.message.starts_with("surprise: "));
    }
}
