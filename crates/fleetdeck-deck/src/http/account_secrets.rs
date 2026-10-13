//! The credential relay for accounts (DL-173, design §0 and §2.4).
//!
//! - `POST /api/secrets/stations/{station}/accounts/apps`: the OAuth client
//!   id and secret of a tracker.
//! - `POST /api/secrets/stations/{station}/accounts/token`: an API token.
//!
//! Both are owner only, with CSRF and an audit row. The value crosses the
//! deck once, in memory, inside one request and its answer. The handler
//! holds it in a [`Secret`], forwards it to the station's `accounts.*`
//! method and drops it. It never goes through the command store, the outbox
//! or a receipt, and no error, log line or audit row holds it. The body is
//! limited to [`MAX_BODY`]. Every error has a fixed code and a fixed text;
//! `ApiError::internal` is not used here, because it logs the error.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_core::Secret;
use fleetdeck_proto::accounts::{AccountKind, AccountView, AppSet, AppView, TokenSet};
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Board;
use super::account_routes::{ask, audited, host_ok, invalid_field, station, text_ok};
use super::guard::{ApiError, owner_only};
use crate::store::Principal;

/// The largest request body of the relay routes: 16 KiB.
pub const MAX_BODY: usize = 16 * 1024;

/// Reads the body as bytes and parses it here. The default JSON rejection is
/// not used: a serde error text can quote the input, and the input can be
/// the secret. Every parse error is a fixed 400 `invalid_body`; a body above
/// the limit is a fixed 413 `too_large`.
fn parse<T: DeserializeOwned>(body: Result<Bytes, BytesRejection>) -> Result<T, ApiError> {
    let bytes = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "the request body is too large",
            )
        } else {
            invalid_body()
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_body())
}

fn invalid_body() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        "the request body is not valid",
    )
}

fn invalid_kind() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_kind",
        "this kind does not fit this request",
    )
}

/// Checks an app: Jira or Linear, and both values allowed.
fn check_app(app: &AppSet) -> Result<(), ApiError> {
    if !matches!(app.kind, AccountKind::Jira | AccountKind::Linear) {
        return Err(invalid_kind());
    }
    if text_ok(&app.client_id) && text_ok(&app.client_secret) {
        Ok(())
    } else {
        Err(invalid_field())
    }
}

/// Checks a token and fills the default host of a forge. A forge `site` is
/// a bare host; Jira needs an email.
fn check_token(token: &mut TokenSet) -> Result<(), ApiError> {
    if !text_ok(&token.token) {
        return Err(invalid_field());
    }
    if token.email.as_deref().is_some_and(|e| !text_ok(e))
        || token.site.as_deref().is_some_and(|s| !text_ok(s))
    {
        return Err(invalid_field());
    }
    match token.kind {
        AccountKind::Github | AccountKind::Gitlab => {
            let default = if token.kind == AccountKind::Github {
                "github.com"
            } else {
                "gitlab.com"
            };
            let site = token.site.get_or_insert_with(|| default.to_owned());
            if !host_ok(site) {
                return Err(invalid_field());
            }
        }
        AccountKind::Jira => {
            if token.email.is_none() {
                return Err(invalid_field());
            }
        }
        AccountKind::Linear => {}
    }
    Ok(())
}

/// `POST /api/secrets/stations/{station}/accounts/apps { kind, client_id,
/// client_secret }`: answers the app without its secret.
///
/// # Errors
/// 400 `invalid_body`, `invalid_kind`, `invalid_field`; 403 `owner_only`; 404
/// `unknown_station`; 409 `station_unreachable`, `unsupported`; 413
/// `too_large`; 502 `station_error`; 504 `timeout`.
pub async fn app_set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<AppView>, ApiError> {
    let work = async {
        owner_only(&principal, "sets an OAuth app")?;
        let app: AppSet = parse(body)?;
        check_app(&app)?;
        let AppSet {
            kind,
            client_id,
            client_secret,
        } = app;
        let secret = Secret::new(client_secret);
        let (_, control) = station(&board, &name).await?;
        let params = json!({
            "kind": kind,
            "client_id": client_id,
            "client_secret": secret.expose(),
        });
        ask(&control, "accounts.app_set", params).await
    };
    audited(&board, &principal, "accounts.app_set", &name, work)
        .await
        .map(Json)
}

/// `POST /api/secrets/stations/{station}/accounts/token { kind, site?,
/// email?, token }`: answers the account without its token.
///
/// # Errors
/// As [`app_set`].
pub async fn token_set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<AccountView>, ApiError> {
    let work = async {
        owner_only(&principal, "sets an account token")?;
        let mut token: TokenSet = parse(body)?;
        check_token(&mut token)?;
        let TokenSet {
            kind,
            site,
            email,
            token,
        } = token;
        let secret = Secret::new(token);
        let (_, control) = station(&board, &name).await?;
        let params = json!({
            "kind": kind,
            "site": site,
            "email": email,
            "token": secret.expose(),
        });
        ask(&control, "accounts.token_set", params).await
    };
    audited(&board, &principal, "accounts.token_set", &name, work)
        .await
        .map(Json)
}
