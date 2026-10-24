//! Byte ranges of MP4 artifacts (R13 P5b E1, contract 11.2 to 11.5).
//!
//! A version with exactly one file of content type `video/mp4` is served
//! with one HTTP byte range (`206`, `416`), `HEAD`, a strong `ETag` and the
//! ordinary preconditions. The bytes come from a complete cache entry (one
//! SQLite slice at a time) or from the station (one `artifacts.file` request
//! at a time, starting at the first wanted byte). Every station answer is
//! validated before one of its bytes leaves the deck. Before every station
//! read and immediately before every chunk that leaves, the deck checks the
//! tombstone and the read authority of the route (a [`Gate`]). A failure
//! after the headers ends the body with an error: JSON is never appended to
//! MP4 bytes.

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_link::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{
    ArtifactFile, ArtifactFileQuery, ArtifactVersion, MAX_CHUNK_BYTES, content_type_of,
};
use tokio::time::Instant;

use super::common::{ask_on, control_of, db, live_artifact, unknown_artifact, video_headers};
use super::stream::Gate;
use super::{ApiError, Board, StationRecord};
use crate::store::DeckStore;

/// The content type that the range behavior applies to.
pub(crate) const VIDEO_MIME: &str = "video/mp4";
/// The longest `Range` header that the deck parses.
const RANGE_HEADER_LIMIT: usize = 256;
/// The most bytes in one station request and in one cache slice.
const CHUNK: u64 = MAX_CHUNK_BYTES;
/// From the request to the response headers (contract 10).
pub(crate) const HEADERS_LIMIT: Duration = Duration::from_secs(25);
/// The whole response, backpressure included (contract 10).
pub(crate) const VIDEO_LIFETIME: Duration = Duration::from_secs(180);

/// What the `Range` header asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeReq {
    /// The whole representation (`200`).
    Full,
    /// The bytes `start..=end`, already clamped to the file (`206`).
    Single { start: u64, end: u64 },
    /// A valid range that selects no byte (`416`).
    Unsatisfiable,
}

/// A `Range` header with a malformed number or a malformed single range
/// (`400 invalid_range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidRange;

/// One `first-last`, `first-` or `-suffix` of a `bytes` set.
enum Spec {
    From { first: u64, last: Option<u64> },
    Suffix(u64),
}

/// An unsigned decimal number: digits only (no sign, no space), no overflow.
fn number(s: &str) -> Result<u64, InvalidRange> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(InvalidRange);
    }
    s.parse().map_err(|_| InvalidRange)
}

fn spec_of(part: &str) -> Result<Spec, InvalidRange> {
    let (first, last) = part.split_once('-').ok_or(InvalidRange)?;
    if first.is_empty() {
        return Ok(Spec::Suffix(number(last)?));
    }
    let first = number(first)?;
    if last.is_empty() {
        return Ok(Spec::From { first, last: None });
    }
    let last = number(last)?;
    if last < first {
        return Err(InvalidRange);
    }
    Ok(Spec::From {
        first,
        last: Some(last),
    })
}

/// True for the characters of an HTTP token (a range unit).
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// The range of the `Range` header for a file of `n` bytes.
///
/// - No header: [`RangeReq::Full`].
/// - A header over 256 bytes, with a bad number (overflow included) or with
///   bad syntax: `Err`.
/// - An unknown unit, or several valid ranges: [`RangeReq::Full`].
/// - A zero suffix, or a start at or after the end of the file:
///   [`RangeReq::Unsatisfiable`].
/// - An end past the end of the file is clamped to `n - 1`.
pub(crate) fn parse_range(v: Option<&HeaderValue>, n: u64) -> Result<RangeReq, InvalidRange> {
    let Some(v) = v else {
        return Ok(RangeReq::Full);
    };
    if v.as_bytes().len() > RANGE_HEADER_LIMIT {
        return Err(InvalidRange);
    }
    let s = v.to_str().map_err(|_| InvalidRange)?.trim();
    let (unit, set) = s.split_once('=').ok_or(InvalidRange)?;
    let unit = unit.trim();
    if !unit.eq_ignore_ascii_case("bytes") {
        return if is_token(unit) {
            Ok(RangeReq::Full)
        } else {
            Err(InvalidRange)
        };
    }
    let mut specs = Vec::new();
    for part in set.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        specs.push(spec_of(part)?);
    }
    let spec = match specs.as_slice() {
        [] => return Err(InvalidRange),
        [only] => only,
        _ => return Ok(RangeReq::Full),
    };
    Ok(match *spec {
        Spec::Suffix(0) => RangeReq::Unsatisfiable,
        Spec::Suffix(_) if n == 0 => RangeReq::Unsatisfiable,
        Spec::Suffix(len) => RangeReq::Single {
            start: n.saturating_sub(len),
            end: n - 1,
        },
        Spec::From { first, .. } if first >= n => RangeReq::Unsatisfiable,
        Spec::From { first, last } => RangeReq::Single {
            start: first,
            end: last.map_or(n - 1, |l| l.min(n - 1)),
        },
    })
}

/// The strong `ETag` of a version: its main file's SHA-256, quoted.
pub(crate) fn etag_of(sha256_hex: &str) -> String {
    format!("\"sha256-{sha256_hex}\"")
}

/// True when the `If-Range` header lets the `Range` apply: the header is
/// absent, or it is a strong entity tag that equals `etag` exactly. A weak
/// tag, another tag and a date select the whole representation.
pub(crate) fn if_range_matches(v: Option<&HeaderValue>, etag: &str) -> bool {
    match v {
        None => true,
        Some(v) => v.to_str().is_ok_and(|s| {
            let s = s.trim();
            !s.starts_with("W/") && s == etag
        }),
    }
}

/// What the ordinary preconditions decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cond {
    Proceed,
    /// `If-None-Match` matches: `304`.
    NotModified,
    /// `If-Match` does not match: `412`.
    Failed,
}

/// The entity tags of every `name` header line; `None` without the header.
fn tags<'a>(req: &'a HeaderMap, name: &HeaderName) -> Option<Vec<&'a str>> {
    let mut seen = false;
    let mut out = Vec::new();
    for v in req.get_all(name) {
        seen = true;
        if let Ok(s) = v.to_str() {
            out.extend(s.split(',').map(str::trim).filter(|t| !t.is_empty()));
        }
    }
    seen.then_some(out)
}

/// `If-Match` (strong comparison, `*` matches) and then `If-None-Match`
/// (weak comparison, `*` matches), for `GET` and `HEAD` (RFC 9110 13.2.2).
pub(crate) fn conditions(req: &HeaderMap, etag: &str) -> Cond {
    if let Some(list) = tags(req, &header::IF_MATCH)
        && !list
            .iter()
            .any(|t| *t == "*" || (!t.starts_with("W/") && *t == etag))
    {
        return Cond::Failed;
    }
    if let Some(list) = tags(req, &header::IF_NONE_MATCH)
        && list
            .iter()
            .any(|t| *t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
    {
        return Cond::NotModified;
    }
    Cond::Proceed
}

/// True when `version` is in the range scope for `path`: exactly one file,
/// that file is `path`, and its content type is `video/mp4`.
pub(crate) fn in_scope(version: &ArtifactVersion, path: &str) -> bool {
    matches!(version.files.as_slice(), [only] if only == path && content_type_of(only) == VIDEO_MIME)
}

fn invalid_range() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_range",
        "The byte range is not valid.",
    )
}

fn unsatisfiable(n: u64) -> Response {
    let mut response = ApiError::new(
        StatusCode::RANGE_NOT_SATISFIABLE,
        "range_not_satisfiable",
        "The requested byte range is not satisfiable.",
    )
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("bytes */{n}")) {
        response.headers_mut().insert(header::CONTENT_RANGE, v);
    }
    response
}

fn precondition_failed() -> Response {
    ApiError::new(
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
        "The precondition of the request is not met.",
    )
    .into_response()
}

fn bad_data() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "station_error",
        "The station returned invalid artifact data.",
    )
}

pub(crate) fn video_timeout() -> ApiError {
    ApiError::new(
        StatusCode::GATEWAY_TIMEOUT,
        "timeout",
        "The video data did not finish loading in time.",
    )
}

fn video_unreachable() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        "The station is not connected.",
    )
}

/// The fixed answer for a failure of the station link or of the station.
fn station_failure(e: ApiError) -> ApiError {
    match e.code {
        "timeout" => video_timeout(),
        "station_unreachable" => video_unreachable(),
        "not_found" | "unknown_artifact" => e,
        _ => bad_data(),
    }
}

fn not_modified(etag: HeaderValue) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    (StatusCode::NOT_MODIFIED, headers).into_response()
}

/// A station answer that is exactly the `want` bytes at `offset` of the
/// `n`-byte file `path` (contract 11.4): the same path, offset and file
/// size, a reported length that equals the wanted length and the decoded
/// length, an `eof` that agrees with the position, and bounded valid base64.
/// A short or an empty answer is invalid: the request never reaches past the
/// end of the file.
fn checked(
    a: &ArtifactFile,
    path: &str,
    offset: u64,
    want: u64,
    n: u64,
) -> Result<Vec<u8>, ApiError> {
    let longest = want.div_ceil(3).saturating_mul(4);
    if a.path != path
        || a.offset != offset
        || a.size != n
        || a.length != want
        || a.eof != (offset.saturating_add(want) == n)
        || a.data_base64.len() as u64 > longest
    {
        return Err(bad_data());
    }
    let bytes = STANDARD.decode(&a.data_base64).map_err(|_| bad_data())?;
    if bytes.len() as u64 != want {
        return Err(bad_data());
    }
    Ok(bytes)
}

/// One station request of `want` bytes at `offset`, validated. It ends with
/// `timeout` when the `deadline` passes.
#[allow(clippy::too_many_arguments)]
async fn read_station(
    control: &Control,
    name: &str,
    artifact_id: &str,
    version: u32,
    path: &str,
    n: u64,
    offset: u64,
    want: u64,
    deadline: Instant,
) -> Result<Vec<u8>, ApiError> {
    if Instant::now() >= deadline {
        return Err(video_timeout());
    }
    let request = ask_on::<ArtifactFile>(
        control,
        name,
        "artifacts.file",
        serde_json::json!(ArtifactFileQuery {
            artifact_id: artifact_id.to_owned(),
            version,
            path: path.to_owned(),
            offset: Some(offset),
            length: Some(want),
        }),
    );
    let answer = tokio::time::timeout_at(deadline, request)
        .await
        .map_err(|_| video_timeout())?
        .map_err(station_failure)?;
    checked(&answer, path, offset, want, n)
}

/// Where the bytes of the body come from.
enum Source {
    /// A complete cache entry of the file.
    Cache,
    Station {
        control: Control,
        name: String,
    },
}

/// What is checked before every read and before every chunk that leaves:
/// the response lifetime, the tombstone and the read authority.
struct Check {
    store: DeckStore,
    station: StationId,
    artifact_id: String,
    gate: Gate,
    /// The end of the response lifetime.
    deadline: Instant,
}

impl Check {
    /// An error text ends the body; a lookup that fails counts as a refusal.
    async fn run(&self) -> Result<(), String> {
        if Instant::now() >= self.deadline {
            return Err("the video response lifetime ended".to_owned());
        }
        let alive = matches!(
            self.store
                .tombstone(self.station.clone(), self.artifact_id.clone())
                .await,
            Ok(None)
        );
        if !alive {
            return Err("the artifact was deleted".to_owned());
        }
        if !(self.gate)().await {
            return Err("the read authority ended".to_owned());
        }
        Ok(())
    }
}

fn cut(reason: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(reason.to_string())
}

/// The body of a range between two chunks.
struct Ranged {
    check: Check,
    version: u32,
    path: String,
    /// The size of the whole file.
    total: u64,
    source: Source,
    /// The bytes that are read and not yet sent.
    buf: Bytes,
    /// The offset of the first byte that is not read yet.
    next: u64,
    /// The offset after the last wanted byte.
    stop: u64,
    done: bool,
}

impl Ranged {
    /// Reads the next chunk (the check runs first).
    async fn refill(&mut self) -> Result<(), String> {
        self.check.run().await?;
        let want = (self.stop - self.next).min(CHUNK);
        let c = &self.check;
        let bytes = match &self.source {
            Source::Cache => read_cache(
                &c.store,
                &c.station,
                &c.artifact_id,
                self.version,
                &self.path,
                self.next,
                want,
            )
            .await
            .map_err(|e| format!("{}: {}", e.code, e.message))?
            .ok_or_else(|| "the cached file is gone".to_owned())?,
            Source::Station { control, name } => read_station(
                control,
                name,
                &c.artifact_id,
                self.version,
                &self.path,
                self.total,
                self.next,
                want,
                c.deadline,
            )
            .await
            .map_err(|e| format!("{}: {}", e.code, e.message))?,
        };
        self.next += want;
        self.buf = Bytes::from(bytes);
        Ok(())
    }
}

/// The body of a range. Right before each station or cache read and right
/// before each chunk leaves, the check runs; a refusal ends the body with an
/// error (the client sees a short body). One chunk is outstanding at a time.
fn ranged_body(state: Ranged) -> Body {
    let stream = futures_util::stream::unfold(state, |mut s| async move {
        if s.done {
            return None;
        }
        if s.buf.is_empty() {
            if s.next >= s.stop {
                return None;
            }
            if let Err(why) = s.refill().await {
                s.done = true;
                return Some((Err(cut(why)), s));
            }
        }
        if let Err(why) = s.check.run().await {
            s.done = true;
            return Some((Err(cut(why)), s));
        }
        let chunk = std::mem::take(&mut s.buf);
        Some((Ok::<_, std::io::Error>(chunk), s))
    });
    Body::from_stream(stream)
}

/// A slice of the cached file with the exact `want` bytes, or `None` when
/// the cache has no complete row any more.
async fn read_cache(
    store: &DeckStore,
    station: &StationId,
    artifact_id: &str,
    version: u32,
    path: &str,
    offset: u64,
    want: u64,
) -> Result<Option<Vec<u8>>, ApiError> {
    let slice = store
        .cached_artifact_slice(
            station.clone(),
            artifact_id.to_owned(),
            version,
            path.to_owned(),
            offset,
            want,
        )
        .await
        .map_err(db)?;
    Ok(slice.filter(|b| b.len() as u64 == want))
}

/// Serves one file of a version that is in the range scope ([`in_scope`]).
/// The caller authenticated the request and resolved the version; `gate`
/// checks the route's read authority again whenever the deck is about to
/// read or send bytes.
///
/// Order: the tombstone, the preconditions (`If-Match`, `If-None-Match`),
/// the range (`If-Range`, `Range`), then the answer.
///
/// # Errors
/// 400 `invalid_range`; 404 `unknown_artifact` (a tombstone or a refused
/// gate), `not_found`; 409 `station_unreachable` (not cached and the station
/// is not connected); 502 `station_error`; 504 `timeout`. A `304`, `412` and
/// `416` are answers, not errors.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_file(
    board: &Board,
    rec: &StationRecord,
    id: &str,
    v: &ArtifactVersion,
    path: &str,
    req: &HeaderMap,
    head: bool,
    gate: Gate,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    live_artifact(board, &rec.station_id, id).await?;
    let n = v.size;
    let etag = etag_of(&v.sha256);
    let etag_value = HeaderValue::from_str(&etag).map_err(|_| bad_data())?;
    match conditions(req, &etag) {
        Cond::Failed => return Ok(precondition_failed()),
        Cond::NotModified => return Ok(not_modified(etag_value)),
        Cond::Proceed => {}
    }
    if head {
        // Metadata only: no station byte read, no cache read.
        let headers = video_headers(path, etag_value, n);
        return Ok((StatusCode::OK, headers, Body::empty()).into_response());
    }
    let range = if if_range_matches(req.get(header::IF_RANGE), &etag) {
        parse_range(req.get(header::RANGE), n).map_err(|_| invalid_range())?
    } else {
        RangeReq::Full
    };
    let (status, start, len) = match range {
        RangeReq::Full => (StatusCode::OK, 0, n),
        RangeReq::Single { start, end } => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
        RangeReq::Unsatisfiable => return Ok(unsatisfiable(n)),
    };
    let mut headers = video_headers(path, etag_value, len);
    if status == StatusCode::PARTIAL_CONTENT {
        let value = HeaderValue::from_str(&format!("bytes {start}-{}/{n}", start + len - 1))
            .map_err(|_| bad_data())?;
        headers.insert(header::CONTENT_RANGE, value);
    }
    if len == 0 {
        if !gate().await {
            return Err(unknown_artifact());
        }
        return Ok((status, headers, Body::empty()).into_response());
    }
    let stop = start + len;
    let first = len.min(CHUNK);
    let check = Check {
        store: board.store.clone(),
        station: rec.station_id.clone(),
        artifact_id: id.to_owned(),
        gate,
        deadline: started + VIDEO_LIFETIME,
    };
    check.run().await.map_err(|_| unknown_artifact())?;
    let cached = board
        .store
        .cached_artifact_len(
            rec.station_id.clone(),
            id.to_owned(),
            v.version,
            path.to_owned(),
        )
        .await
        .map_err(db)?;
    let from_cache = if cached == Some(n) {
        read_cache(
            &board.store,
            &rec.station_id,
            id,
            v.version,
            path,
            start,
            first,
        )
        .await?
    } else {
        None
    };
    let (source, bytes) = match from_cache {
        Some(bytes) => (Source::Cache, bytes),
        None => {
            let control = control_of(board, rec).map_err(station_failure)?;
            let bytes = read_station(
                &control,
                &rec.name,
                id,
                v.version,
                path,
                n,
                start,
                first,
                started + HEADERS_LIMIT,
            )
            .await?;
            let source = Source::Station {
                control,
                name: rec.name.clone(),
            };
            (source, bytes)
        }
    };
    // The tombstone and the authority of this moment, right before the
    // headers: a deletion or a revocation during the read is a refusal, not
    // a cut body.
    check.run().await.map_err(|_| unknown_artifact())?;
    let body = ranged_body(Ranged {
        check,
        version: v.version,
        path: path.to_owned(),
        total: n,
        source,
        buf: Bytes::from(bytes),
        next: start + first,
        stop,
        done: false,
    });
    Ok((status, headers, body).into_response())
}

#[cfg(test)]
#[path = "artifact_range_tests.rs"]
mod tests;
