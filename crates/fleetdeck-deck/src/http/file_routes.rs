//! The board side of the read-only file service (M3 DL-088, PLAN §6.6):
//!
//! - `GET /api/stations/{station}/files/list?session=|workspace=|artifacts=&path=`
//! - `GET /api/stations/{station}/files/read?session=|workspace=|artifacts=&path=&max_bytes=`
//! - `GET /api/stations/{station}/files/search?session=|workspace=|artifacts=&q=&mode=&max_hits=`
//! - `GET /api/stations/{station}/sessions/{session}/turns/{turn}/diff?max_bytes=`
//! - `GET /api/stations/{station}/sessions/{session}/diff?max_bytes=` (the
//!   working tree against the base commit of the session, R1.2)
//!
//! The deck forwards each query to the station's `files.*` method on a live
//! link and returns the answer. The station resolves the root and enforces
//! the allowed roots, symlink and hidden-directory rules; the deck keeps no
//! copy of file content. The methods need `drive` and do not mutate.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::files::{
    FileContent, FileListQuery, FileListing, FileReadQuery, FileRoot, FileSearch, FileSearchQuery,
    MAX_PATH_CHARS, SearchMode, SessionDiff, SessionDiffQuery, TurnDiff, TurnDiffQuery,
};
use fleetdeck_proto::fleet_git::code as fleet_code;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{SessionId, TurnId};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::Board;
use super::access::session_of;
use super::guard::ApiError;
use crate::store::{LinkFeature, MemberAction, Principal};

/// The longest the board waits for the station's answer (above the
/// station's own deadlines).
pub const FILES_WAIT: Duration = Duration::from_secs(20);

/// The query of a file route.
#[derive(Debug, Default, Deserialize)]
pub struct FileParams {
    pub session: Option<String>,
    pub workspace: Option<String>,
    /// The artifact folder of this session (FG-12, design §15.6).
    pub artifacts: Option<String>,
    pub path: Option<String>,
    pub max_bytes: Option<u64>,
    pub q: Option<String>,
    pub mode: Option<String>,
    pub max_hits: Option<u32>,
}

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message.into())
}

fn text_ok(text: &str) -> bool {
    text.chars().count() <= MAX_PATH_CHARS && !text.contains('\0')
}

impl FileParams {
    fn root(&self) -> Result<FileRoot, ApiError> {
        match (&self.session, &self.workspace, &self.artifacts) {
            (Some(s), None, None) if text_ok(s) && !s.is_empty() => {
                Ok(FileRoot::Session(SessionId::from(s.as_str())))
            }
            (None, Some(w), None) if text_ok(w) && !w.is_empty() => {
                Ok(FileRoot::Workspace(w.clone()))
            }
            (None, None, Some(a)) if text_ok(a) && !a.is_empty() => {
                Ok(FileRoot::Artifacts(SessionId::from(a.as_str())))
            }
            _ => Err(bad(
                "invalid_root",
                "name exactly one root: `session`, `workspace` or `artifacts`",
            )),
        }
    }

    fn path(&self) -> Result<String, ApiError> {
        let path = self.path.clone().unwrap_or_default();
        if text_ok(&path) {
            Ok(path)
        } else {
            Err(bad(
                "invalid_path",
                format!("a path has at most {MAX_PATH_CHARS} characters and no NUL"),
            ))
        }
    }
}

/// For a member (R11.9, DL-166; FG-12, M11): the query names the `session`
/// root of a shared session, or the `artifacts` root of that exact session,
/// never a `workspace`, and the call goes to the station with the member flag,
/// so the station hides the files of its deny list.
async fn member_check(
    board: &Board,
    principal: &Principal,
    station: &str,
    p: &FileParams,
) -> Result<bool, ApiError> {
    if !matches!(principal, Principal::Member(..)) {
        return Ok(false);
    }
    let session = match (&p.session, &p.workspace, &p.artifacts) {
        (Some(s), None, None) | (None, None, Some(s)) => s.as_str(),
        _ => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "not_shared",
                "a member browses the files of a shared session only",
            ));
        }
    };
    session_of(
        board,
        principal,
        station,
        session,
        MemberAction::Read,
        "read these files",
    )
    .await?;
    Ok(true)
}

/// Sends `method` to the station's live link and decodes the answer.
async fn forward<T: DeserializeOwned>(
    board: &Board,
    station: &str,
    method: &'static str,
    mut params: serde_json::Value,
    member: bool,
) -> Result<Json<T>, ApiError> {
    if member && let Some(o) = params.as_object_mut() {
        o.insert(
            fleetdeck_proto::member_deny::MEMBER_PARAM.to_owned(),
            serde_json::Value::Bool(true),
        );
    }
    let rec = board
        .store
        .find_station(station.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{station}`"),
            )
        })?;
    // A station without `fleet_git.homes.v1` refuses the `artifacts` root, so
    // the deck does not send it (FG-12, design §15.6).
    if params.pointer("/root/artifacts").is_some()
        && board
            .store
            .link_feature(&rec.station_id, feature::FLEET_GIT_HOMES)
            == LinkFeature::Without
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            fleet_code::FLEET_GIT_UNSUPPORTED,
            "This station does not support Fleet Git. Update it first.",
        ));
    }
    let unreachable = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            format!("station `{}` has no live link to this deck", rec.name),
        )
    };
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(unreachable)?;
    let outcome = tokio::time::timeout(FILES_WAIT, control.request(method, None, params))
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
            .map(Json)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(station_error(&e.code, e.message)),
    }
}

/// Maps a station `files.*` error code to an HTTP status (codes listed in
/// `fleetdeck_proto::files::code`). A link-level refusal (for example a
/// station without `files.v1`) is `station_error`.
fn station_error(code: &str, message: String) -> ApiError {
    use fleetdeck_proto::files::code as c;
    let status = match code {
        c::INVALID_PATH
        | c::INVALID_QUERY
        | c::WRONG_KIND
        | c::MISSING_WORKSPACE
        | c::OUTSIDE_ROOTS => StatusCode::BAD_REQUEST,
        c::NOT_FOUND | c::UNKNOWN_SESSION | c::NO_DIFF => StatusCode::NOT_FOUND,
        c::NO_BASE => StatusCode::CONFLICT,
        c::BUSY => StatusCode::TOO_MANY_REQUESTS,
        c::TIMEOUT => StatusCode::GATEWAY_TIMEOUT,
        // The artifact root (FG-12, decision C5): the station's own text.
        fleet_code::ARTIFACTS_UNAVAILABLE
        | fleet_code::SHARE_EXPOSED
        | fleet_code::SHARE_UNVERIFIED => StatusCode::CONFLICT,
        _ => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{code}: {message}"),
            );
        }
    };
    let known: &'static str = [
        c::INVALID_PATH,
        c::INVALID_QUERY,
        c::WRONG_KIND,
        c::MISSING_WORKSPACE,
        c::OUTSIDE_ROOTS,
        c::NOT_FOUND,
        c::UNKNOWN_SESSION,
        c::NO_DIFF,
        c::NO_BASE,
        c::BUSY,
        c::TIMEOUT,
        fleet_code::ARTIFACTS_UNAVAILABLE,
        fleet_code::SHARE_EXPOSED,
        fleet_code::SHARE_UNVERIFIED,
    ]
    .into_iter()
    .find(|k| *k == code)
    .unwrap_or("station_error");
    ApiError::new(status, known, message)
}

fn params<T: serde::Serialize>(query: &T) -> Result<serde_json::Value, ApiError> {
    serde_json::to_value(query).map_err(ApiError::internal)
}

/// # Errors
/// 400 `invalid_root`, `invalid_path`, `wrong_kind`, `missing_workspace`,
/// `outside_roots`; 404 `unknown_station`, `not_found`, `unknown_session`;
/// 409 `station_unreachable`; 502 `station_error`; 504 `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    Query(p): Query<FileParams>,
) -> Result<Json<FileListing>, ApiError> {
    // More than one root is 400 `invalid_root` before any member rule (D10).
    let root = p.root()?;
    let member = member_check(&board, &principal, &station, &p).await?;
    let query = FileListQuery {
        root,
        path: p.path()?,
    };
    forward(&board, &station, "files.list", params(&query)?, member).await
}

/// # Errors
/// As [`list`].
pub async fn read(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    Query(p): Query<FileParams>,
) -> Result<Json<FileContent>, ApiError> {
    // More than one root is 400 `invalid_root` before any member rule (D10).
    let root = p.root()?;
    let member = member_check(&board, &principal, &station, &p).await?;
    let query = FileReadQuery {
        root,
        path: p.path()?,
        max_bytes: p.max_bytes,
    };
    forward(&board, &station, "files.read", params(&query)?, member).await
}

/// # Errors
/// As [`list`], plus 400 `invalid_query` and 429 `busy`.
pub async fn search(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    Query(p): Query<FileParams>,
) -> Result<Json<FileSearch>, ApiError> {
    // More than one root is 400 `invalid_root` before any member rule (D10).
    let root = p.root()?;
    let member = member_check(&board, &principal, &station, &p).await?;
    let mode = match p.mode.as_deref() {
        None | Some("name") => SearchMode::Name,
        Some("content") => SearchMode::Content,
        Some(_) => return Err(bad("invalid_query", "`mode` is `name` or `content`")),
    };
    let text = p.q.clone().unwrap_or_default();
    if !text_ok(&text) {
        return Err(bad("invalid_query", "the search text has no NUL"));
    }
    let query = FileSearchQuery {
        root,
        query: text,
        mode,
        max_hits: p.max_hits,
    };
    forward(&board, &station, "files.search", params(&query)?, member).await
}

/// # Errors
/// 404 `unknown_station`, `unknown_session`, `no_diff`; 400
/// `outside_roots`; 409 `station_unreachable`; 502 `station_error`; 504
/// `timeout`.
pub async fn diff(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, turn)): Path<(String, String, String)>,
    Query(p): Query<FileParams>,
) -> Result<Json<TurnDiff>, ApiError> {
    // The turn diff has no artifacts root (design §15.6).
    if p.artifacts.is_some() {
        return Err(bad(
            "invalid_root",
            "the turn diff takes no `artifacts` root",
        ));
    }
    let own = FileParams {
        session: Some(session.clone()),
        ..FileParams::default()
    };
    let member = member_check(&board, &principal, &station, &own).await?;
    if !text_ok(&session) || !text_ok(&turn) {
        return Err(bad("invalid_path", "the session or turn id is too long"));
    }
    let query = TurnDiffQuery {
        session_id: SessionId::from(session.as_str()),
        turn_id: Some(TurnId::from(turn.as_str())),
        checkpoint: None,
        max_bytes: p.max_bytes,
    };
    forward(&board, &station, "files.diff", params(&query)?, member).await
}

/// `GET /api/stations/{station}/sessions/{session}/diff` (R1.2).
///
/// # Errors
/// 404 `unknown_station`, `unknown_session`, `no_diff`; 400
/// `outside_roots`, `missing_workspace`; 409 `no_base`,
/// `station_unreachable`; 502 `station_error`; 504 `timeout`.
pub async fn session_diff(
    State(board): State<Arc<Board>>,
    Path((station, session)): Path<(String, String)>,
    Query(p): Query<FileParams>,
) -> Result<Json<SessionDiff>, ApiError> {
    if !text_ok(&session) {
        return Err(bad("invalid_path", "the session id is too long"));
    }
    let query = SessionDiffQuery {
        session_id: SessionId::from(session.as_str()),
        max_bytes: p.max_bytes,
    };
    forward(
        &board,
        &station,
        "files.session_diff",
        params(&query)?,
        false,
    )
    .await
}
