//! The accounts of a station and the OAuth start and finish (DL-173,
//! design `settings-and-accounts.md` §2.2 to §2.5).
//!
//! - `GET /api/stations/{station}/accounts`, `POST .../accounts/forge/login`,
//!   `GET .../accounts/forge/login/{login_id}`, `POST
//!   .../accounts/{account_id}/logout`: owner only. The deck relays them to
//!   the station's `accounts.*` methods. A token or a member gets 403
//!   `owner_only`.
//! - `GET .../accounts/{account_id}/containers`: scope `read`, closed to
//!   members; the projects of Jira or the teams of Linear.
//! - `POST /api/accounts/oauth/start` and `POST /api/accounts/oauth/finish`:
//!   owner only, with the transactions of [`crate::oauth`].
//!
//! Every error answer has a fixed code and a fixed text: no station text,
//! no request body and no value reaches an error, a log or an audit row
//! (the credential relay rules of design §0). The audit rows hold the
//! action, the station and the error code only.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use fleetdeck_link::control::Control;
use fleetdeck_proto::accounts::{
    AccountKind, AccountRef, AccountView, AccountsList, Containers, ForgeLogin, ForgeLoginQuery,
    ForgeLoginStart, ForgeLoginState, OAuthStartParams, OAuthStarted,
};
use fleetdeck_proto::link::Outcome;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::oauth::{FinishError, OpenError, Transactions};
use crate::store::{AuditResult, NewAudit, Principal, StationRecord};

/// The longest the board waits for the station's answer.
pub const WAIT: Duration = Duration::from_secs(15);
/// The longest value of a field of these routes, in characters.
pub const MAX_FIELD_CHARS: usize = 4096;
/// The longest id in a path, in characters.
const MAX_ID_CHARS: usize = 128;

fn fixed(status: StatusCode, code: &'static str, text: &'static str) -> ApiError {
    ApiError::new(status, code, text)
}

/// 400 `invalid_field`.
pub(super) fn invalid_field() -> ApiError {
    fixed(
        StatusCode::BAD_REQUEST,
        "invalid_field",
        "a field of the request is empty, too long or not allowed",
    )
}

/// The error for a body that does not parse. The text is fixed: a parse
/// error can quote the value that failed.
pub(super) fn body_error(e: &JsonRejection) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return fixed(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "the request body is too large",
        );
    }
    fixed(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        "the request body is not valid",
    )
}

/// True for a non-empty value of at most [`MAX_FIELD_CHARS`] characters
/// without a control character.
pub(super) fn text_ok(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_FIELD_CHARS
        && !value.chars().any(char::is_control)
}

/// True for an id of a path: 1 to 128 characters of `[A-Za-z0-9_.:@-]`.
fn id_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_CHARS
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'@' | b'-'))
}

/// True for a bare host name: `[a-z0-9.-]`, 1 to 253 characters.
pub(super) fn host_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
}

/// The station `name` with a live link.
pub(super) async fn station(
    board: &Board,
    name: &str,
) -> Result<(StationRecord, Control), ApiError> {
    let rec = board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            fixed(
                StatusCode::NOT_FOUND,
                "unknown_station",
                "no station has this name",
            )
        })?;
    let control = board.link.control_for(&rec.station_id).ok_or_else(|| {
        fixed(
            StatusCode::CONFLICT,
            "station_unreachable",
            "the station has no live link to this deck",
        )
    })?;
    Ok((rec, control))
}

/// The HTTP error for a station error code. The text of the station error
/// is never used.
fn station_error(code: &str) -> ApiError {
    match code {
        "invalid_params" => fixed(
            StatusCode::BAD_REQUEST,
            "invalid_field",
            "the station refused a field of the request",
        ),
        "unsupported" => fixed(
            StatusCode::CONFLICT,
            "unsupported",
            "the station does not support this",
        ),
        "forbidden" => fixed(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the station refused this request",
        ),
        "env_token_present" => fixed(
            StatusCode::CONFLICT,
            "env_token_present",
            "a token in the station environment overrides the login",
        ),
        "device_flow_unsupported" => fixed(
            StatusCode::CONFLICT,
            "device_flow_unsupported",
            "this CLI or server has no device login; use a token",
        ),
        "unexpected_cli_output" => fixed(
            StatusCode::BAD_GATEWAY,
            "unexpected_cli_output",
            "the CLI answered in a form that the station does not accept",
        ),
        _ => fixed(
            StatusCode::BAD_GATEWAY,
            "station_error",
            "the station could not do this",
        ),
    }
}

/// Sends `method` with `params` and decodes the answer. Every failure has a
/// fixed code and a fixed text.
pub(super) async fn ask<T: DeserializeOwned>(
    control: &Control,
    method: &str,
    params: Value,
) -> Result<T, ApiError> {
    let unreachable = || {
        fixed(
            StatusCode::CONFLICT,
            "station_unreachable",
            "the link to the station ended",
        )
    };
    let outcome = tokio::time::timeout(WAIT, control.request(method, None, params))
        .await
        .map_err(|_| {
            fixed(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable())?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| {
            fixed(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station could not do this",
            )
        }),
        Outcome::Err(e) => Err(station_error(&e.code)),
    }
}

/// Runs `work` and writes its audit row: the action, the station and the
/// error code. A refusal before the station is `refused`; a failure after
/// it is `failed`.
pub(super) async fn audited<T>(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    station: &str,
    work: impl Future<Output = Result<T, ApiError>>,
) -> Result<T, ApiError> {
    let result = work.await;
    audit_row(board, principal, action, station, result.as_ref().err()).await;
    result
}

/// The audit row of one call: the action, the station and the error code.
async fn audit_row(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    station: &str,
    error: Option<&ApiError>,
) {
    let row = match error {
        None => NewAudit::new(principal.actor(), action, AuditResult::Ok),
        Some(e) => {
            let kind = if matches!(
                e.status,
                StatusCode::BAD_REQUEST
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
                    | StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::PAYLOAD_TOO_LARGE
            ) {
                AuditResult::Refused
            } else {
                AuditResult::Failed
            };
            NewAudit::new(principal.actor(), action, kind).detail(e.code)
        }
    };
    // The target is a station name from the path or the body: keep it out
    // of the log when it is not a plain name.
    let target = if station.len() <= MAX_ID_CHARS && !station.chars().any(char::is_control) {
        station
    } else {
        ""
    };
    board.store.audit_or_log(row.target(target)).await;
}

fn owner(principal: &Principal) -> Result<(), ApiError> {
    owner_only(principal, "manages the accounts of a station")
}

/// `GET /api/stations/{station}/accounts`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 502
/// `station_error`; 504 `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<AccountsList>, ApiError> {
    owner(&principal)?;
    let (_, control) = station(&board, &name).await?;
    ask(&control, "accounts.list", json!({})).await.map(Json)
}

fn forge(kind: AccountKind) -> bool {
    matches!(kind, AccountKind::Github | AccountKind::Gitlab)
}

fn invalid_kind() -> ApiError {
    fixed(
        StatusCode::BAD_REQUEST,
        "invalid_kind",
        "this kind does not fit this request",
    )
}

/// `POST /api/stations/{station}/accounts/forge/login { kind, host }`.
///
/// # Errors
/// 400 `invalid_body`, `invalid_kind`, `invalid_field`; 403 `owner_only`; 404
/// `unknown_station`; 409 `station_unreachable`, `env_token_present`,
/// `device_flow_unsupported`, `unsupported`; 502; 504.
pub async fn forge_login(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    body: Result<Json<ForgeLoginStart>, JsonRejection>,
) -> Result<Json<ForgeLogin>, ApiError> {
    let work = async {
        owner(&principal)?;
        let Json(body) = body.map_err(|e| body_error(&e))?;
        if !forge(body.kind) {
            return Err(invalid_kind());
        }
        if !host_ok(&body.host) {
            return Err(invalid_field());
        }
        let (_, control) = station(&board, &name).await?;
        ask(&control, "accounts.forge_login", json!(body)).await
    };
    audited(&board, &principal, "accounts.forge_login", &name, work)
        .await
        .map(Json)
}

/// `GET /api/stations/{station}/accounts/forge/login/{login_id}`.
///
/// # Errors
/// 400 `invalid_field`; 403 `owner_only`; 404 `unknown_station`; 409; 502;
/// 504.
pub async fn forge_login_status(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((name, login_id)): Path<(String, String)>,
) -> Result<Json<ForgeLoginState>, ApiError> {
    owner(&principal)?;
    if !id_ok(&login_id) {
        return Err(invalid_field());
    }
    let (_, control) = station(&board, &name).await?;
    ask(
        &control,
        "accounts.forge_login_status",
        json!(ForgeLoginQuery { login_id }),
    )
    .await
    .map(Json)
}

/// `POST /api/stations/{station}/accounts/{account_id}/logout`.
///
/// # Errors
/// 400 `invalid_field`; 403 `owner_only`; 404 `unknown_station`; 409; 502;
/// 504.
pub async fn logout(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((name, account_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let work = async {
        owner(&principal)?;
        if !id_ok(&account_id) {
            return Err(invalid_field());
        }
        let (_, control) = station(&board, &name).await?;
        ask::<Value>(
            &control,
            "accounts.logout",
            json!(AccountRef { account_id }),
        )
        .await
    };
    audited(&board, &principal, "accounts.logout", &name, work)
        .await
        .map(|_| Json(json!({})))
}

/// `GET /api/stations/{station}/accounts/{account_id}/containers`: scope
/// `read`, closed to members (the guard). The Jira projects or Linear teams
/// of an account.
///
/// # Errors
/// 400 `invalid_field`; 404 `unknown_station`; 409; 502; 504.
pub async fn containers(
    State(board): State<Arc<Board>>,
    Path((name, account_id)): Path<(String, String)>,
) -> Result<Json<Containers>, ApiError> {
    if !id_ok(&account_id) {
        return Err(invalid_field());
    }
    let (_, control) = station(&board, &name).await?;
    ask(
        &control,
        "issues.containers",
        json!(AccountRef { account_id }),
    )
    .await
    .map(Json)
}

/// `POST /api/accounts/oauth/start` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthStartBody {
    pub station: String,
    pub kind: AccountKind,
}

/// The only hosts of an authorize URL: Atlassian for Jira, Linear for Linear.
fn authorize_host(kind: AccountKind) -> Option<&'static str> {
    match kind {
        AccountKind::Jira => Some("auth.atlassian.com"),
        AccountKind::Linear => Some("linear.app"),
        AccountKind::Github | AccountKind::Gitlab => None,
    }
}

/// True when `url` is `https` with exactly the host `host`: no userinfo, no
/// port, no whitespace or control character, no backslash.
#[must_use]
pub fn authorize_url_ok(url: &str, host: &str) -> bool {
    if url
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.contains('@') && authority.eq_ignore_ascii_case(host)
}

fn bad_origin() -> ApiError {
    fixed(
        StatusCode::BAD_REQUEST,
        "bad_origin",
        "the request needs the Origin of this deck's board",
    )
}

/// The redirect URI of this request: `<Origin>/oauth/callback`, when the
/// `Origin` is a board origin.
fn redirect_uri(board: &Board, headers: &HeaderMap) -> Result<String, ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| board.origins.allows(o))
        .ok_or_else(bad_origin)?;
    Ok(format!("{}/oauth/callback", origin.to_ascii_lowercase()))
}

/// `POST /api/accounts/oauth/start { station, kind }`: opens a transaction
/// and answers the authorize URL.
///
/// # Errors
/// 400 `invalid_body`, `invalid_kind`, `bad_origin`; 403 `owner_only`; 404
/// `unknown_station`; 409 `station_unreachable`; 429 `too_many_oauth`; 502
/// `bad_authorize_url`, `station_error`; 504 `timeout`.
pub async fn oauth_start(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Extension(transactions): Extension<Arc<Transactions>>,
    headers: HeaderMap,
    body: Result<Json<OAuthStartBody>, JsonRejection>,
) -> Result<Json<OAuthStarted>, ApiError> {
    let target = match &body {
        Ok(Json(b)) => b.station.clone(),
        Err(_) => String::new(),
    };
    let work = async {
        owner(&principal)?;
        let Json(body) = body.map_err(|e| body_error(&e))?;
        let host = authorize_host(body.kind).ok_or_else(invalid_kind)?;
        let redirect_uri = redirect_uri(&board, &headers)?;
        let Principal::Owner(session) = &principal else {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "owner_only",
                "only the owner's board login starts a sign-in",
            ));
        };
        let (rec, control) = station(&board, &body.station).await?;
        let opened = transactions
            .open(
                board.store.now_ms(),
                &session.session_hash,
                &rec.name,
                body.kind,
                &redirect_uri,
            )
            .map_err(|e| match e {
                OpenError::TooMany => fixed(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too_many_oauth",
                    "this login has four sign-ins open; finish one or wait",
                ),
                OpenError::Random => ApiError::internal("the system random source failed"),
            })?;
        let params = OAuthStartParams {
            kind: body.kind,
            state: opened.state,
            redirect_uri,
        };
        let started: Result<OAuthStarted, ApiError> =
            ask(&control, "accounts.oauth_start", json!(params)).await;
        let started = match started {
            Ok(s) if authorize_url_ok(&s.authorize_url, host) => s,
            Ok(_) => {
                transactions.discard(&opened.transaction);
                return Err(fixed(
                    StatusCode::BAD_GATEWAY,
                    "bad_authorize_url",
                    "the station returned a sign-in address that the deck does not accept",
                ));
            }
            Err(e) => {
                transactions.discard(&opened.transaction);
                return Err(e);
            }
        };
        Ok(started)
    };
    audited(&board, &principal, "accounts.oauth_start", &target, work)
        .await
        .map(Json)
}

/// `POST /api/accounts/oauth/finish` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthFinishBody {
    pub transaction: String,
}

/// `POST /api/accounts/oauth/finish { transaction }`: the same owner login
/// that started the transaction gives its code to the station.
///
/// # Errors
/// 400 `invalid_body`; 403 `owner_only`, `wrong_session`; 404
/// `unknown_transaction`; 409 `not_ready`, `station_unreachable`; 502; 504.
pub async fn oauth_finish(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Extension(transactions): Extension<Arc<Transactions>>,
    body: Result<Json<OAuthFinishBody>, JsonRejection>,
) -> Result<Json<AccountView>, ApiError> {
    let mut target = String::new();
    let result = finish(&board, &principal, &transactions, body, &mut target).await;
    audit_row(
        &board,
        &principal,
        "accounts.oauth_finish",
        &target,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}

async fn finish(
    board: &Board,
    principal: &Principal,
    transactions: &Transactions,
    body: Result<Json<OAuthFinishBody>, JsonRejection>,
    target: &mut String,
) -> Result<AccountView, ApiError> {
    owner(principal)?;
    let Json(body) = body.map_err(|e| body_error(&e))?;
    let Principal::Owner(session) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            "only the owner's board login finishes a sign-in",
        ));
    };
    let finish = transactions
        .take(
            board.store.now_ms(),
            &body.transaction,
            &session.session_hash,
        )
        .map_err(|e| match e {
            FinishError::Unknown => fixed(
                StatusCode::NOT_FOUND,
                "unknown_transaction",
                "this sign-in is unknown, used or expired",
            ),
            FinishError::WrongSession => fixed(
                StatusCode::FORBIDDEN,
                "wrong_session",
                "this sign-in was started by another login",
            ),
            FinishError::NotReady => fixed(
                StatusCode::CONFLICT,
                "not_ready",
                "the provider has not returned to the board yet, or the answer expired",
            ),
        })?;
    target.clone_from(&finish.station);
    let (_, control) = station(board, &finish.station).await?;
    let params = json!({ "state": finish.state, "code": finish.code.expose() });
    ask(&control, "accounts.oauth_finish", params).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_authorize_url_must_be_https_on_the_exact_host() {
        let ok = "https://auth.atlassian.com/authorize?audience=api.atlassian.com&state=x";
        assert!(authorize_url_ok(ok, "auth.atlassian.com"));
        assert!(authorize_url_ok(
            "https://linear.app/oauth/authorize",
            "linear.app"
        ));
        for bad in [
            "http://auth.atlassian.com/authorize",
            "https://auth.atlassian.com.evil.example/authorize",
            "https://evil.example/auth.atlassian.com",
            "https://user@auth.atlassian.com/authorize",
            "https://auth.atlassian.com@evil.example/authorize",
            "https://auth.atlassian.com:444/authorize",
            "https://auth.atlassian.com\\@evil.example/",
            "https://auth.atlassian.com/ a",
            "https://auth.atlassian.com/\nx",
            "//auth.atlassian.com/authorize",
            "",
        ] {
            assert!(!authorize_url_ok(bad, "auth.atlassian.com"), "{bad}");
        }
        assert!(!authorize_url_ok(ok, "linear.app"));
    }

    #[test]
    fn hosts_ids_and_texts_are_checked() {
        assert!(host_ok("github.com") && host_ok("git.example-corp.io"));
        for bad in ["", "GitHub.com", "a b", "a/b", "a:443", &"a".repeat(254)] {
            assert!(!host_ok(bad), "{bad}");
        }
        assert!(id_ok("acc-1:jira") && !id_ok("") && !id_ok("a b") && !id_ok(&"a".repeat(129)));
        assert!(text_ok("x") && !text_ok("") && !text_ok("a\nb") && !text_ok(&"a".repeat(4097)));
    }

    #[test]
    fn a_station_error_text_never_reaches_the_answer() {
        for code in [
            "invalid_params",
            "unsupported",
            "forbidden",
            "env_token_present",
            "device_flow_unsupported",
            "unexpected_cli_output",
            "internal",
            "anything",
        ] {
            let e = station_error(code);
            assert!(!e.message.contains(code) || e.code == code, "{code}");
        }
        assert_eq!(station_error("weird").code, "station_error");
        assert_eq!(
            station_error("invalid_params").status,
            StatusCode::BAD_REQUEST
        );
    }
}
