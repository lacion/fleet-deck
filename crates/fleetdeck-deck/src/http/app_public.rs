//! The public document route of MCP Apps (B15, design `mcp-apps.md` §4.7
//! and §4.8): the view tokens, the rate limit of the call route, the bootstrap
//! script and `GET /app/{token}`.
//!
//! `/app/{token}` is outside the guard: the token is the credential ("anyone
//! with the token"). It lives 10 minutes and loads its document as often as
//! the frame reloads.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::StationId;
use fleetdeck_proto::apps::{AppCsp, AppDocumentChunk, AppDocumentQuery, MAX_APP_DOC_BYTES};
use fleetdeck_proto::link::Outcome;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};

use super::Board;
use super::app_csp;
use super::guard::ApiError;
use crate::store::auth::secret_hash;

/// How long a view token lives.
pub(super) const VIEW_TOKEN_MS: i64 = 10 * 60 * 1000;
/// The most view tokens in memory (the oldest goes first).
pub(super) const MAX_VIEW_TOKENS: usize = 512;
/// The most calls of one login in one window.
pub(super) const CALLS_PER_WINDOW: usize = 60;
/// The window of the call rate limit.
pub(super) const CALL_WINDOW_MS: i64 = 60 * 1000;
/// The total wait of the deck for the chunks of one document.
const DOC_WAIT: Duration = Duration::from_secs(30);
/// The text of a document that the deck cannot give.
const DOC_FAILED: &str = "the station could not give the app document";

/// The bootstrap script that the deck puts first in an app document
/// (design §4.8, exactly).
pub(super) const APP_BOOTSTRAP: &str = r#"<script>(()=>{"use strict";
try{void document.cookie}catch(e){if(e&&e.name==="SecurityError"){Object.defineProperty(document,"cookie",{configurable:true,get:()=>"",set:()=>{}})}}
const host=window.parent,ch=new MessageChannel(),port=ch.port1,send=MessagePort.prototype.postMessage;
port.postMessage=function(m,_o,t){send.call(port,m,Array.isArray(t)?t:[])};
Object.defineProperty(window,"parent",{configurable:true,enumerable:true,get:()=>port});
port.onmessage=(ev)=>{const d=ev.data;
const c=d&&((d.result&&d.result.hostContext)||(d.method==="ui/notifications/host-context-changed"&&d.params));
if(c&&(c.theme==="light"||c.theme==="dark"))document.documentElement.style.colorScheme=c.theme;
window.dispatchEvent(new MessageEvent("message",{data:d,source:port}))};
port.start();
host.postMessage({"fleet-deck-app-hello":1},"*",[ch.port2]);
})();</script>"#;

/// What a view token stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewEntry {
    pub station_id: StationId,
    pub session_id: String,
    pub app_id: String,
    pub csp: AppCsp,
    /// The lowercase hex SHA-256 of the document, from `AppMeta`.
    pub doc_sha256: String,
    /// The login that made the token; kept for the record only.
    pub login_session_hash: String,
    /// Unix milliseconds.
    pub expires_at: i64,
}

#[derive(Default)]
struct Inner {
    /// `sha256(token)` to the entry and its insert number.
    views: HashMap<String, (u64, ViewEntry)>,
    next: u64,
    /// The call times (Unix ms) of each login, oldest first.
    calls: HashMap<String, VecDeque<i64>>,
}

/// The view tokens and the call rate limit, in memory. One value for the
/// router, given to the routes with `Extension` (like the OAuth
/// transactions). A deck restart forgets them.
#[derive(Default)]
pub struct AppTokens {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for AppTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppTokens(..)")
    }
}

impl AppTokens {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keeps `entry` under `hash`. Expired entries go first; over
    /// [`MAX_VIEW_TOKENS`] the oldest entry goes.
    pub(super) fn insert(&self, hash: String, entry: ViewEntry, now_ms: i64) {
        let mut inner = self.lock();
        inner.views.retain(|_, (_, e)| e.expires_at > now_ms);
        let n = inner.next;
        inner.next += 1;
        inner.views.insert(hash, (n, entry));
        while inner.views.len() > MAX_VIEW_TOKENS {
            let Some(oldest) = inner
                .views
                .iter()
                .min_by_key(|(_, (n, _))| *n)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            inner.views.remove(&oldest);
        }
    }

    /// The entry under `hash`, when it has not expired.
    pub(super) fn lookup(&self, hash: &str, now_ms: i64) -> Option<ViewEntry> {
        let mut inner = self.lock();
        match inner.views.get(hash) {
            Some((_, e)) if e.expires_at > now_ms => Some(e.clone()),
            Some(_) => {
                inner.views.remove(hash);
                None
            }
            None => None,
        }
    }

    /// Counts one call of the login `login_hash`. `false`: the login made
    /// [`CALLS_PER_WINDOW`] calls in the last [`CALL_WINDOW_MS`].
    pub(super) fn allow_call(&self, login_hash: &str, now_ms: i64) -> bool {
        let mut inner = self.lock();
        let times = inner.calls.entry(login_hash.to_owned()).or_default();
        while times.front().is_some_and(|t| *t <= now_ms - CALL_WINDOW_MS) {
            times.pop_front();
        }
        if times.len() >= CALLS_PER_WINDOW {
            return false;
        }
        times.push_back(now_ms);
        true
    }

    /// The number of view tokens in memory.
    #[cfg(test)]
    pub(super) fn view_count(&self) -> usize {
        self.lock().views.len()
    }
}

/// A new view token: 32 random bytes as base64url.
pub(super) fn new_token() -> Result<String, ApiError> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ApiError::internal("the system random source failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The key of a view token in the map.
pub(super) fn token_key(token: &str) -> String {
    secret_hash(token)
}

/// True for the text form of a token that [`new_token`] makes.
fn token_shape_ok(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// `html` with [`APP_BOOTSTRAP`] after its leading doctype, if it has one, so
/// the bootstrap is the first `<script>` of the document. A leading BOM and
/// white space stay in front. Without a doctype the bootstrap goes in front
/// of the text (after the BOM and the white space).
pub(super) fn with_bootstrap(html: &str) -> String {
    let lead = html
        .find(|c: char| c != '\u{feff}' && !c.is_ascii_whitespace())
        .unwrap_or(html.len());
    let rest = &html[lead..];
    let at = if rest
        .get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("<!doctype"))
    {
        rest.find('>').map_or(lead, |i| lead + i + 1)
    } else {
        lead
    };
    let mut out = String::with_capacity(html.len() + APP_BOOTSTRAP.len());
    out.push_str(&html[..at]);
    out.push_str(APP_BOOTSTRAP);
    out.push_str(&html[at..]);
    out
}

fn gone() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn failed() -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "station_error", DOC_FAILED)
}

/// Reads every chunk of the document from the station (at most
/// [`MAX_APP_DOC_BYTES`]) and checks the SHA-256.
async fn fetch(board: &Board, entry: &ViewEntry) -> Result<Vec<u8>, ApiError> {
    let control = board
        .link
        .control_for(&entry.station_id)
        .ok_or_else(failed)?;
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let params = serde_json::to_value(AppDocumentQuery {
            session_id: SessionId::from(entry.session_id.as_str()),
            app_id: entry.app_id.clone(),
            offset: bytes.len() as u64,
        })
        .map_err(ApiError::internal)?;
        let outcome = control
            .request("session.app_document", None, params)
            .await
            .map_err(|_| failed())?;
        let Outcome::Ok(v) = outcome else {
            return Err(failed());
        };
        let chunk: AppDocumentChunk = serde_json::from_value(v).map_err(|_| failed())?;
        let raw = STANDARD.decode(&chunk.data_b64).map_err(|_| failed())?;
        if chunk.total > MAX_APP_DOC_BYTES as u64
            || bytes.len().saturating_add(raw.len()) > MAX_APP_DOC_BYTES
        {
            return Err(failed());
        }
        if raw.is_empty() && !chunk.eof {
            return Err(failed());
        }
        bytes.extend_from_slice(&raw);
        if chunk.eof {
            break;
        }
    }
    if hex_of(digest(&SHA256, &bytes).as_ref()) != entry.doc_sha256 {
        return Err(failed());
    }
    Ok(bytes)
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// `GET` and `HEAD /app/{token}`: the app document with the bootstrap and
/// the CSP of its app.
pub async fn document(
    State(board): State<Arc<Board>>,
    Extension(tokens): Extension<Arc<AppTokens>>,
    Path(token): Path<String>,
) -> Response {
    if !token_shape_ok(&token) {
        return gone();
    }
    let Some(entry) = tokens.lookup(&token_key(&token), board.store.now_ms()) else {
        return gone();
    };
    let bytes = match tokio::time::timeout(DOC_WAIT, fetch(&board, &entry)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => return e.into_response(),
        Err(_) => return failed().into_response(),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return failed().into_response();
    };
    let Ok(csp) = HeaderValue::from_str(&app_csp::header(&entry.csp)) else {
        return failed().into_response();
    };
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        with_bootstrap(&text),
    )
        .into_response()
}
