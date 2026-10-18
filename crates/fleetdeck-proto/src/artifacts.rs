//! Artifacts (FLEETDECK-V2.md R12, DL-155, DL-165): immutable, versioned
//! files that a session or a person publishes. The station copies the bytes
//! into its data directory; the deck keeps an index and a byte cache.
//!
//! The link methods (feature `artifacts.v1`, deck → station):
//!
//! - `artifacts.publish` ([`ArtifactPublish`] → [`ArtifactPublished`], scope
//!   `drive`);
//! - `artifacts.list` ([`ArtifactListQuery`] → [`ArtifactList`], scope
//!   `read`);
//! - `artifacts.get` ([`ArtifactGetQuery`] → [`ArtifactDetail`], scope
//!   `read`);
//! - `artifacts.file` ([`ArtifactFileQuery`] → [`ArtifactFile`], scope
//!   `read`).
//!
//! None of them is a receipt mutation: a publish of identical content to an
//! existing artifact returns the stored latest version and makes no new one,
//! so a retry is safe.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::capture::CaptureProvenance;
use crate::ids::SessionId;

/// The longest title in characters; the station cuts a longer one.
pub const MAX_TITLE_CHARS: usize = 200;
/// The longest `set_focus` text in characters (DL-165).
pub const MAX_FOCUS_CHARS: usize = 120;
/// The most raw bytes in one `artifacts.file` answer: 700 KiB, so that the
/// base64 form stays below the 1 MiB control frame.
pub const MAX_CHUNK_BYTES: u64 = 700 * 1024;
/// The longest author name in characters.
pub const MAX_AUTHOR_CHARS: usize = 64;
/// The author of an artifact that an agent published through MCP.
pub const AUTHOR_AGENT: &str = "agent";
/// The author of a publish through the link when the caller names none.
pub const AUTHOR_OWNER: &str = "owner";

/// Wire error codes of the artifact methods.
pub mod code {
    /// The path is empty, has `..`, NUL or `\`, or is too long.
    pub const INVALID_PATH: &str = "invalid_path";
    /// A parameter is out of range (title, author, offset, length).
    pub const INVALID_PARAMS: &str = "invalid_params";
    /// The file is not found, is a symlink, or is not a regular file.
    pub const NOT_FOUND: &str = "not_found";
    /// The session is unknown.
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    /// The path is outside the session workspace and the allowed roots.
    pub const OUTSIDE_ROOTS: &str = "outside_roots";
    /// The artifact or the version is unknown.
    pub const UNKNOWN_ARTIFACT: &str = "unknown_artifact";
    /// This station has no artifact directory.
    pub const UNSUPPORTED: &str = "unsupported";
}

/// What an artifact is, by its main file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "artifacts/")]
pub enum ArtifactKind {
    Html,
    Markdown,
    Image,
    Svg,
    Pdf,
    /// R13 P5b: a trusted desktop recording (`capture.mp4`). Only the
    /// recording import produces it; `of_path` never does.
    Video,
    Other,
}

impl ArtifactKind {
    /// The kind of a file name, by its extension (case-insensitive).
    #[must_use]
    pub fn of_path(path: &str) -> Self {
        let ext = path
            .rsplit('/')
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "html" | "htm" => Self::Html,
            "md" | "markdown" => Self::Markdown,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "ico" => Self::Image,
            "svg" => Self::Svg,
            "pdf" => Self::Pdf,
            _ => Self::Other,
        }
    }

    /// The snake_case name, as stored.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "markdown",
            Self::Image => "image",
            Self::Svg => "svg",
            Self::Pdf => "pdf",
            Self::Video => "video",
            Self::Other => "other",
        }
    }

    /// The kind of a stored name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "html" => Self::Html,
            "markdown" => Self::Markdown,
            "image" => Self::Image,
            "svg" => Self::Svg,
            "pdf" => Self::Pdf,
            "video" => Self::Video,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// One artifact: the latest state of an id with its versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct Artifact {
    pub artifact_id: String,
    /// The station that holds the bytes. A station fills in its name; the
    /// deck replaces it with its own station id.
    pub station_id: String,
    /// The session that made it; absent for an artifact of a person.
    pub session_id: Option<SessionId>,
    pub title: String,
    pub kind: ArtifactKind,
    pub latest_version: u32,
    /// Unix ms of the first version.
    #[ts(type = "number")]
    pub created_at: i64,
    /// Unix ms of the latest version.
    #[ts(type = "number")]
    pub updated_at: i64,
    /// `agent`, or the name of the owner or member that published it.
    pub author: String,
}

/// One immutable version of an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactVersion {
    pub artifact_id: String,
    pub version: u32,
    /// The bytes of all files of the bundle.
    #[ts(type = "number")]
    pub size: u64,
    /// The lowercase hex SHA-256 of the main file.
    pub sha256: String,
    /// The bundle paths, relative to the bundle root and `/`-separated; the
    /// first one is the main file.
    pub files: Vec<String>,
    /// Unix ms.
    #[ts(type = "number")]
    pub created_at: i64,
    /// The screenshot provenance (R13 P5a); never taken from publish params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capture: Option<CaptureProvenance>,
}

/// `artifacts.publish` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactPublish {
    /// The session whose workspace holds the file; a relative `path` is
    /// relative to its working directory.
    #[serde(default)]
    pub session_id: Option<SessionId>,
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Makes a new version of this artifact.
    #[serde(default)]
    pub artifact_id: Option<String>,
    /// The name of the person that publishes; absent: `owner`.
    #[serde(default)]
    pub author: Option<String>,
}

/// The answer of `artifacts.publish`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactPublished {
    pub artifact: Artifact,
    pub version: ArtifactVersion,
    /// False when the content equals the latest version: no new version.
    pub created: bool,
}

/// `artifacts.delete` params (R13 P5a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactDeleteParams {
    pub artifact_id: String,
}

/// The state of a deleted artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "artifacts/")]
pub enum ArtifactDeletedState {
    Deleted,
}

/// The answer of `artifacts.delete` (R13 P5a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactDeleted {
    pub artifact_id: String,
    pub state: ArtifactDeletedState,
}

/// `artifacts.list` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactListQuery {
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// The answer of `artifacts.list`, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactList {
    pub artifacts: Vec<Artifact>,
}

/// `artifacts.get` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactGetQuery {
    pub artifact_id: String,
    /// Absent: the latest version.
    #[serde(default)]
    pub version: Option<u32>,
}

/// The answer of `artifacts.get`: the metadata, the requested version with
/// its file list, and every version, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactDetail {
    pub artifact: Artifact,
    pub version: ArtifactVersion,
    pub versions: Vec<ArtifactVersion>,
}

/// `artifacts.file` params. A file larger than [`MAX_CHUNK_BYTES`] comes in
/// chunks: the caller repeats the call with the next `offset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactFileQuery {
    pub artifact_id: String,
    pub version: u32,
    /// A path of the version's file list.
    pub path: String,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub offset: Option<u64>,
    /// At most [`MAX_CHUNK_BYTES`]; absent: the maximum.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub length: Option<u64>,
}

/// The answer of `artifacts.file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactFile {
    pub path: String,
    /// The size of the whole file.
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub offset: u64,
    /// The bytes in `data_base64`.
    #[ts(type = "number")]
    pub length: u64,
    pub data_base64: String,
    /// True when `offset + length == size`.
    pub eof: bool,
}

/// The body of `POST /api/artifacts/publish` (the deck's relay of
/// `artifacts.publish`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ArtifactPublishRequest {
    /// The station that holds the workspace.
    pub station: String,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artifact_id: Option<String>,
}

/// An artifact in the deck's index (`GET /api/artifacts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct DeckArtifact {
    pub station_id: String,
    pub station_name: String,
    pub artifact_id: String,
    pub session_id: Option<SessionId>,
    pub title: String,
    pub kind: ArtifactKind,
    pub latest_version: u32,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// The body of `POST /api/artifacts/{station}/{id}/shares`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ShareRequest {
    /// Absent: always the latest version.
    #[serde(default)]
    pub version: Option<u32>,
    pub expires_in_days: u32,
}

/// A share link (`POST .../shares`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ShareLink {
    /// `https://<deck>/a/<token>/`. Anyone who can reach the deck and holds
    /// the URL can read the artifact.
    pub url: String,
    /// The id that lists and revokes the share; it is not the secret.
    pub share_id: String,
    pub version: Option<u32>,
    #[ts(type = "number")]
    pub expires_at: i64,
}

/// A share as the owner lists it. The URL is shown once, at creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ShareView {
    pub share_id: String,
    /// Absent: always the latest version.
    pub version: Option<u32>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub expires_at: i64,
    pub revoked: bool,
    /// How many times the document was opened through the share.
    #[ts(type = "number")]
    pub views: u64,
}

/// The body of `POST /api/artifacts/{station}/{id}/view`: a link for the
/// board's own viewer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "artifacts/")]
pub struct ViewRequest {
    /// Absent: the latest version.
    #[serde(default)]
    pub version: Option<u32>,
}

/// A short-lived link for the board's viewer. The viewer loads it in a
/// sandboxed frame: the page and its bundled files then load without the
/// board cookie, which a sandboxed frame never sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct ViewLink {
    /// A path on the deck: `/a/<token>/`.
    pub url: String,
    #[ts(type = "number")]
    pub expires_at: i64,
}

/// The answer of `GET /api/artifacts/{station}/{id}`: the index row, every
/// version, and the shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "artifacts/")]
pub struct DeckArtifactDetail {
    pub artifact: DeckArtifact,
    pub versions: Vec<ArtifactVersion>,
    pub shares: Vec<ShareView>,
}

/// The content type of a bundle file, by its extension. Unknown types are
/// `application/octet-stream`, which a browser downloads.
#[must_use]
pub fn content_type_of(path: &str) -> &'static str {
    let ext = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "txt" | "csv" | "log" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type_of("a/B.HTML"), "text/html; charset=utf-8");
        assert_eq!(content_type_of("x.svg"), "image/svg+xml");
        assert_eq!(content_type_of("x.woff2"), "font/woff2");
        assert_eq!(content_type_of("x.exe"), "application/octet-stream");
        assert_eq!(content_type_of("noext"), "application/octet-stream");
    }

    #[test]
    fn the_kind_follows_the_extension() {
        for (path, kind) in [
            ("out/report.HTML", ArtifactKind::Html),
            ("a.htm", ArtifactKind::Html),
            ("notes.md", ArtifactKind::Markdown),
            ("shot.PNG", ArtifactKind::Image),
            ("logo.svg", ArtifactKind::Svg),
            ("paper.pdf", ArtifactKind::Pdf),
            ("data.csv", ArtifactKind::Other),
            ("capture.mp4", ArtifactKind::Other),
            ("README", ArtifactKind::Other),
            ("dir.html/file", ArtifactKind::Other),
        ] {
            assert_eq!(ArtifactKind::of_path(path), kind, "{path}");
        }
        for kind in [
            ArtifactKind::Html,
            ArtifactKind::Markdown,
            ArtifactKind::Image,
            ArtifactKind::Svg,
            ArtifactKind::Pdf,
            ArtifactKind::Video,
            ArtifactKind::Other,
        ] {
            assert_eq!(ArtifactKind::parse(kind.as_str()), Some(kind));
        }
    }
}
