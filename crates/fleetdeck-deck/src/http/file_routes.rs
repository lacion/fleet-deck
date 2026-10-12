//! The board side of the read-only file service (M3 DL-088, PLAN §6.6):
//!
//! - `GET /api/stations/{station}/files/list?session=|workspace=&path=`
//! - `GET /api/stations/{station}/files/read?session=|workspace=&path=&max_bytes=`
//! - `GET /api/stations/{station}/files/search?session=|workspace=&q=&mode=&max_hits=`
//! - `GET /api/stations/{station}/sessions/{session}/turns/{turn}/diff?max_bytes=`
//!
//! The deck forwards each query to the station's `files.*` method on a live
//! link and returns the answer. The station resolves the root and enforces
//! the allowed roots, symlink and hidden-directory rules; the deck keeps no
//! copy of file content. The methods need `drive` and do not mutate.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::files::{
    FileContent, FileListQuery, FileListing, FileReadQuery, FileRoot, FileSearch, FileSearchQuery,
    MAX_PATH_CHARS, SearchMode, TurnDiff, TurnDiffQuery,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{SessionId, TurnId};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::Board;
use super::guard::ApiError;

/// The longest the board waits for the station's answer (above the
/// station's own deadlines).
pub const FILES_WAIT: Duration = Duration::from_secs(20);

/// The query of a file route.
#[derive(Debug, Default, Deserialize)]
pub struct FileParams {
    pub session: Option<String>,
    pub workspace: Option<String>,
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
        match (&self.session, &self.workspace) {
            (Some(s), None) if text_ok(s) && !s.is_empty() => {
                Ok(FileRoot::Session(SessionId::from(s.as_str())))
            }
            (None, Some(w)) if text_ok(w) && !w.is_empty() => Ok(FileRoot::Workspace(w.clone())),
            _ => Err(bad(
                "invalid_root",
                "name exactly one root: `session` or `workspace`",
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

/// Sends `method` to the station's live link and decodes the answer.
async fn forward<T: DeserializeOwned>(
    board: &Board,
    station: &str,
    method: &'static str,
    params: serde_json::Value,
) -> Result<Json<T>, ApiError> {
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
        c::BUSY => StatusCode::TOO_MANY_REQUESTS,
        c::TIMEOUT => StatusCode::GATEWAY_TIMEOUT,
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
        c::BUSY,
        c::TIMEOUT,
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
    Path(station): Path<String>,
    Query(p): Query<FileParams>,
) -> Result<Json<FileListing>, ApiError> {
    let query = FileListQuery {
        root: p.root()?,
        path: p.path()?,
    };
    forward(&board, &station, "files.list", params(&query)?).await
}

/// # Errors
/// As [`list`].
pub async fn read(
    State(board): State<Arc<Board>>,
    Path(station): Path<String>,
    Query(p): Query<FileParams>,
) -> Result<Json<FileContent>, ApiError> {
    let query = FileReadQuery {
        root: p.root()?,
        path: p.path()?,
        max_bytes: p.max_bytes,
    };
    forward(&board, &station, "files.read", params(&query)?).await
}

/// # Errors
/// As [`list`], plus 400 `invalid_query` and 429 `busy`.
pub async fn search(
    State(board): State<Arc<Board>>,
    Path(station): Path<String>,
    Query(p): Query<FileParams>,
) -> Result<Json<FileSearch>, ApiError> {
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
        root: p.root()?,
        query: text,
        mode,
        max_hits: p.max_hits,
    };
    forward(&board, &station, "files.search", params(&query)?).await
}

/// # Errors
/// 404 `unknown_station`, `unknown_session`, `no_diff`; 400
/// `outside_roots`; 409 `station_unreachable`; 502 `station_error`; 504
/// `timeout`.
pub async fn diff(
    State(board): State<Arc<Board>>,
    Path((station, session, turn)): Path<(String, String, String)>,
    Query(p): Query<FileParams>,
) -> Result<Json<TurnDiff>, ApiError> {
    if !text_ok(&session) || !text_ok(&turn) {
        return Err(bad("invalid_path", "the session or turn id is too long"));
    }
    let query = TurnDiffQuery {
        session_id: SessionId::from(session.as_str()),
        turn_id: TurnId::from(turn.as_str()),
        max_bytes: p.max_bytes,
    };
    forward(&board, &station, "files.diff", params(&query)?).await
}
