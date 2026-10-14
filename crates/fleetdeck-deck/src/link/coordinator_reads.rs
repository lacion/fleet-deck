//! The station side of the ADR-008 read queries (§4.1, §4.3).
//!
//! The store authorized the query and named one station request
//! ([`StationRead`]). This module sends it on the live link of the station
//! (20 s for each request), and shapes the answer. All text that comes from
//! a file, a diff, an issue, a PR or an artifact goes into the untrusted
//! envelope; ids, counts and states stay outside it. A station refusal
//! reaches the tool as `"<code>: <message>"`.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_link::control::Control;
use fleetdeck_proto::artifacts::{
    ArtifactDetail, ArtifactFile, ArtifactFileQuery, ArtifactKind, MAX_CHUNK_BYTES,
};
use fleetdeck_proto::files::{FileContent, FileSearch, SessionDiff};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::untrusted::{fence, fence_cut};
use fleetdeck_proto::{Reject, RejectCode, StationId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::DeckLink;
use crate::store::{ReadShape, StationRead};

/// The longest wait for one request to a station.
const STATION_WAIT: Duration = Duration::from_secs(20);
/// The most characters of a diff.
const DIFF_CHARS: usize = 256 * 1024;
/// The most characters of the items of a project.
const ITEMS_CHARS: usize = 256 * 1024;
/// The most characters of the hits of a search.
const SEARCH_CHARS: usize = 64 * 1024;
/// The most bytes of text of one artifact answer.
const ARTIFACT_BYTES: usize = 256 * 1024;

type Shaped = std::result::Result<Value, Reject>;

fn refusal(code: RejectCode, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

fn unreachable_station() -> Reject {
    refusal(
        RejectCode::InvalidState,
        "station_unreachable: the station of this read has no live link",
    )
}

/// The rejection for a station error `code`.
fn station_refusal(code: &str, message: &str) -> Reject {
    let kind = match code {
        "invalid_params" | "unknown_session" | "not_found" | "invalid_path" | "invalid_query"
        | "wrong_kind" => RejectCode::InvalidCommand,
        "forbidden" | "outside_roots" | "private_path" => RejectCode::Forbidden,
        _ => RejectCode::InvalidState,
    };
    refusal(kind, format!("{code}: {message}"))
}

fn station_error(e: impl std::fmt::Display) -> Reject {
    refusal(RejectCode::InvalidState, format!("station_error: {e}"))
}

/// One request to the station; the value of its answer.
async fn ask(control: &Control, method: &str, params: Value) -> Shaped {
    let outcome = tokio::time::timeout(STATION_WAIT, control.request(method, None, params))
        .await
        .map_err(|_| {
            refusal(
                RejectCode::InvalidState,
                "timeout: the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable_station())?;
    match outcome {
        Outcome::Ok(v) => Ok(v),
        Outcome::Err(e) => Err(station_refusal(&e.code, &e.message)),
    }
}

fn parse<T: DeserializeOwned>(value: Value) -> std::result::Result<T, Reject> {
    serde_json::from_value(value).map_err(station_error)
}

fn rejected(reject: Reject) -> Outcome {
    Outcome::Ok(json!({ "reject": reject }))
}

/// Serves one station read and shapes its answer.
pub(crate) async fn run(ctx: &DeckLink, read: StationRead) -> Outcome {
    let Some(control) = ctx.control_for(&read.station) else {
        return rejected(unreachable_station());
    };
    match shaped(&control, read).await {
        Ok(v) => Outcome::Ok(v),
        Err(r) => rejected(r),
    }
}

async fn shaped(control: &Control, read: StationRead) -> Shaped {
    let StationRead {
        station,
        method,
        params,
        shape,
        source,
    } = read;
    let first = ask(control, method, params).await?;
    match shape {
        ReadShape::SessionDiff { session_id } => {
            let diff: SessionDiff = parse(first)?;
            Ok(json!({
                "station": station, "session_id": session_id,
                "base_oid": diff.base_oid, "summary": diff.summary,
                "truncated": diff.truncated,
                "patch": fence_cut(&source, &diff.patch, DIFF_CHARS),
            }))
        }
        ReadShape::FileRead { session_id, path } => {
            let file: FileContent = parse(first)?;
            let text = file.text.as_deref().filter(|_| !file.binary);
            Ok(json!({
                "station": station, "session_id": session_id, "path": path,
                "size": file.size, "truncated": file.truncated, "binary": file.binary,
                "text": text.map(|t| fence(&source, t)),
            }))
        }
        ReadShape::FileSearch { session_id } => {
            let found: FileSearch = parse(first)?;
            let hits = serde_json::to_string(&found.hits).map_err(station_error)?;
            Ok(json!({
                "station": station, "session_id": session_id,
                "truncated": found.truncated,
                "hits": fence_cut(&source, &hits, SEARCH_CHARS),
            }))
        }
        ReadShape::ProjectItems { project_id, kind } => {
            let items = serde_json::to_string(&first).map_err(station_error)?;
            Ok(json!({
                "project_id": project_id, "kind": kind,
                "items": fence_cut(&source, &items, ITEMS_CHARS),
            }))
        }
        ReadShape::Artifact {
            kind, artifact_id, ..
        } => {
            let detail: ArtifactDetail = parse(first)?;
            artifact_text(control, (&station, &artifact_id, kind), &detail, &source).await
        }
    }
}

/// The bytes of one file of an artifact version, at most `limit` of them,
/// and whether data remains after them. It reads one byte more than `limit`
/// to know that.
async fn file_bytes(
    control: &Control,
    (artifact_id, version): (&str, u32),
    path: &str,
    limit: usize,
) -> std::result::Result<(Vec<u8>, bool), Reject> {
    let want = limit + 1;
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() < want {
        let length = u64::try_from(want - bytes.len())
            .unwrap_or(MAX_CHUNK_BYTES)
            .min(MAX_CHUNK_BYTES);
        let query = ArtifactFileQuery {
            artifact_id: artifact_id.to_owned(),
            version,
            path: path.to_owned(),
            offset: Some(bytes.len() as u64),
            length: Some(length),
        };
        let chunk: ArtifactFile = parse(ask(control, "artifacts.file", json!(query)).await?)?;
        let part = STANDARD.decode(&chunk.data_base64).map_err(station_error)?;
        bytes.extend_from_slice(&part);
        if chunk.eof || part.is_empty() {
            break;
        }
    }
    let more = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, more))
}

/// The text of the files of the version, up to 256 KiB in all. The file in
/// progress is cut back to a character boundary; later files are left out.
async fn artifact_text(
    control: &Control,
    (station, artifact_id, kind): (&StationId, &str, ArtifactKind),
    detail: &ArtifactDetail,
    source: &str,
) -> Shaped {
    let version = detail.version.version;
    let mut files = Vec::new();
    let mut used = 0usize;
    let mut cut = false;
    for path in &detail.version.files {
        let remaining = ARTIFACT_BYTES - used;
        let (bytes, more) = file_bytes(control, (artifact_id, version), path, remaining).await?;
        if more && bytes.is_empty() {
            // The budget is spent and this file has data.
            cut = true;
            break;
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => Some(text),
            // A cut inside a character: back to the last valid boundary.
            Err(e) if more && e.error_len().is_none() => {
                std::str::from_utf8(&bytes[..e.valid_up_to()]).ok()
            }
            Err(_) => None,
        };
        // Only text counts against the budget.
        used += text.map_or(0, str::len);
        let text = text.unwrap_or("(binary file)");
        files.push(json!({
            "path": path,
            "text": fence(&format!("{source}/{path}"), text),
        }));
        if more {
            cut = true;
            break;
        }
    }
    Ok(json!({
        "station": station, "artifact_id": artifact_id, "version": version,
        "kind": kind, "cut": cut, "files": files,
    }))
}
