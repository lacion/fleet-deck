//! The owner-auth requests of `deck.sock` (M2 P3-8; PLAN §9, A13): the
//! one-time login link of `fleetdeck open`, the owner password, API tokens
//! for bearer clients, and the audit log. Every mutation is audited with
//! the actor `local` (the socket accepts only the node's own user).

use std::collections::BTreeSet;

use fleetdeck_core::Secret;
use fleetdeck_core::localsock::InvitationText;
use fleetdeck_proto::link::Scope;
use serde::{Deserialize, Serialize};

use crate::local::DeckResponse;
use crate::store::auth::LOGIN_LINK_TTL_MS;
use crate::store::{ApiToken, AuditEntry, AuditResult, DeckStore, NewAudit};

/// The actor of audit rows for `deck.sock` requests.
pub const LOCAL_ACTOR: &str = "local";
/// A day in ms (API token lifetimes are given in days).
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The auth requests (tagged by `op` inside [`crate::local::DeckRequest`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AuthRequest {
    /// `fleetdeck open`: a one-time login link for the board.
    LoginLink,
    /// `deck password`: sets the owner password and ends every login.
    SetPassword { password: InvitationText },
    /// `deck token create`.
    CreateToken {
        name: String,
        scopes: BTreeSet<Scope>,
        ttl_days: u32,
    },
    /// `deck token list`.
    Tokens,
    /// `deck token revoke`.
    RevokeToken { name: String },
    /// `deck audit`: the newest rows first.
    Audit { limit: u32 },
}

/// The answers to [`AuthRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AuthResponse {
    LoginLink {
        url: InvitationText,
        expires_at: i64,
        /// The SHA-256 of the board certificate, for the browser check.
        certificate_sha256: String,
        /// The certificate is the deck's self-signed one.
        self_signed: bool,
    },
    PasswordSet,
    /// A new token; its value is shown once.
    Token {
        token: ApiToken,
        value: InvitationText,
    },
    Tokens {
        tokens: Vec<ApiToken>,
    },
    TokenRevoked {
        name: String,
    },
    Audit {
        entries: Vec<AuditEntry>,
    },
}

/// The board URL of a login link: the token travels in the fragment, so
/// it never reaches a server log or a `Referer` header.
#[must_use]
pub fn login_url(board_url: &str, token: &Secret<String>) -> Secret<String> {
    Secret::new(format!(
        "{}/#login={}",
        board_url.trim_end_matches('/'),
        token.expose()
    ))
}

async fn audited<T>(
    store: &DeckStore,
    action: &str,
    target: Option<&str>,
    r: Result<T, crate::store::DeckError>,
) -> Result<T, String> {
    let mut row = NewAudit::new(
        LOCAL_ACTOR,
        action,
        if r.is_ok() {
            AuditResult::Ok
        } else {
            AuditResult::Failed
        },
    );
    if let Some(t) = target {
        row = row.target(t);
    }
    if let Err(e) = &r {
        row = row.detail(e.to_string());
    }
    store.audit_or_log(row).await;
    r.map_err(|e| e.to_string())
}

/// The first board listener, for login links.
#[derive(Debug, Clone)]
pub struct BoardEndpoint {
    /// `https://<addr>`.
    pub url: String,
    pub certificate_sha256: String,
    pub self_signed: bool,
}

/// Handles one auth request.
pub async fn handle(
    store: &DeckStore,
    board: Option<&BoardEndpoint>,
    request: AuthRequest,
) -> Result<AuthResponse, String> {
    match request {
        AuthRequest::LoginLink => {
            let Some(board) = board else {
                return Err("the deck serves no board listener".to_owned());
            };
            let token = audited(
                store,
                "auth.login_link",
                None,
                store.create_login_link().await,
            )
            .await?;
            Ok(AuthResponse::LoginLink {
                url: InvitationText(login_url(&board.url, &token).expose().clone()),
                expires_at: store.now_ms() + LOGIN_LINK_TTL_MS,
                certificate_sha256: board.certificate_sha256.clone(),
                self_signed: board.self_signed,
            })
        }
        AuthRequest::SetPassword { password } => {
            let r = store.set_password(Secret::new(password.0)).await;
            audited(store, "auth.set_password", None, r).await?;
            Ok(AuthResponse::PasswordSet)
        }
        AuthRequest::CreateToken {
            name,
            scopes,
            ttl_days,
        } => {
            let ttl = i64::from(ttl_days).saturating_mul(DAY_MS);
            let r = store.create_token(name.clone(), scopes, ttl).await;
            let (token, value) = audited(store, "auth.token_create", Some(&name), r).await?;
            Ok(AuthResponse::Token {
                token,
                value: InvitationText(value.expose().clone()),
            })
        }
        AuthRequest::Tokens => store
            .tokens()
            .await
            .map(|tokens| AuthResponse::Tokens { tokens })
            .map_err(|e| e.to_string()),
        AuthRequest::RevokeToken { name } => {
            let r = store.revoke_token(name.clone()).await.and_then(|found| {
                if found {
                    Ok(())
                } else {
                    Err(crate::store::DeckError::Invalid(format!(
                        "no token is named `{name}`"
                    )))
                }
            });
            audited(store, "auth.token_revoke", Some(&name), r).await?;
            Ok(AuthResponse::TokenRevoked { name })
        }
        AuthRequest::Audit { limit } => store
            .audit_log(limit)
            .await
            .map(|entries| AuthResponse::Audit { entries })
            .map_err(|e| e.to_string()),
    }
}

/// Wraps an auth answer for the socket.
#[must_use]
pub fn respond(r: Result<AuthResponse, String>) -> DeckResponse {
    match r {
        Ok(auth) => DeckResponse::Auth { auth },
        Err(message) => DeckResponse::Error { message },
    }
}
