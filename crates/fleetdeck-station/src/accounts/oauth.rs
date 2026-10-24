//! The OAuth apps of Jira and Linear (DL-173, design §2.3): the app, the
//! authorize URL, the code exchange and the token refresh.
//!
//! - Jira is a confidential client (a client secret and a bound `state`; no
//!   PKCE). Linear adds PKCE (S256).
//! - A token request goes to `curl` on stdin: the client secret, the code
//!   and the tokens are never in argv.
//! - The transactions live in memory for [`TTL`]. A transaction is used
//!   once, and only while the app has the generation that it started with.
//! - Atlassian rotates the refresh token. The new pair is written to the
//!   store before the access token is used, and every account of the same
//!   grant (one Jira sign-in gives one account for each site) gets it.
//! - An upstream error is a fixed code with the HTTP status; no upstream
//!   text reaches an error or a log.

use std::sync::PoisonError;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fleetdeck_core::Secret;
use fleetdeck_core::secret::ct_eq;
use fleetdeck_proto::accounts::{
    AccountKind, AccountMethod, AccountView, AppSet, AppView, OAuthFinishParams, OAuthStartParams,
    OAuthStarted,
};
use fleetdeck_proto::integrations::{code, valid_host};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};

use super::http::{self, pct};
use super::oauth_app::{AppSource, BUILTIN_NEEDS_LOCALHOST_TEXT, EffectiveApp, app_changed};
use super::store::{AppSourceDisk, StoredAccount, StoredApp};
use super::{Accounts, codes, invalid, now_secs, store_fail};
use crate::integrations::exec::Fail;

/// How long an OAuth transaction lives.
pub const TTL: Duration = Duration::from_secs(10 * 60);
/// The most open transactions.
const MAX_OPEN: usize = 32;
/// The access token is refreshed when it ends in less than this.
const REFRESH_AHEAD: i64 = 60;
/// The most Jira sites that one sign-in makes accounts for.
const MAX_SITES: usize = 10;

const JIRA_AUTHORIZE: &str = "https://auth.atlassian.com/authorize";
const JIRA_TOKEN: &str = "https://auth.atlassian.com/oauth/token";
const JIRA_RESOURCES: &str = "https://api.atlassian.com/oauth/token/accessible-resources";
const JIRA_API: &str = "https://api.atlassian.com/ex/jira";
const LINEAR_AUTHORIZE: &str = "https://linear.app/oauth/authorize";
pub(super) const LINEAR_TOKEN: &str = "https://api.linear.app/oauth/token";
const LINEAR_GRAPHQL: &str = "https://api.linear.app/graphql";
/// The public client id of the built-in Fleet Deck app of Linear (DL-185).
/// A client id is public; the app has no client secret.
pub const LINEAR_BUILTIN_CLIENT_ID: &str = "66edc8f79eb0603f2aded18705bdfb99";
/// The only redirect URIs of the built-in Linear app (DL-185).
pub const LINEAR_BUILTIN_REDIRECTS: [&str; 2] = [
    "https://localhost:7420/oauth/callback",
    "https://127.0.0.1:7420/oauth/callback",
];
/// The owner flips this after the live checks of design §4 pass and records the result in the design file.
pub const LINEAR_BUILTIN_VERIFIED: bool = false;
/// The variable that turns the built-in Linear app on for the live checks.
const LINEAR_BUILTIN_ENV: &str = "FLEETDECK_LINEAR_BUILTIN";
const JIRA_SCOPE: &str = "read%3Ajira-work%20write%3Ajira-work%20read%3Ajira-user%20offline_access";

/// True when the built-in Linear app is on: the release gate is passed, or
/// the variable is `1`.
pub(super) fn builtin_flag(verified: bool, env: Option<&str>) -> bool {
    verified || env == Some("1")
}

/// [`builtin_flag`] with the constant and the station environment, read on
/// each call.
pub(super) fn linear_builtin_enabled() -> bool {
    builtin_flag(
        LINEAR_BUILTIN_VERIFIED,
        std::env::var(LINEAR_BUILTIN_ENV).ok().as_deref(),
    )
}

/// One open OAuth sign-in.
pub struct Transaction {
    kind: AccountKind,
    state: String,
    /// Linear only.
    verifier: Option<Secret<String>>,
    /// The app that started the sign-in.
    app: EffectiveApp,
    redirect_uri: String,
    started: Instant,
}

impl std::fmt::Debug for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("kind", &self.kind)
            .field("app", &self.app)
            .finish_non_exhaustive()
    }
}

/// The URL of the app is `https` or `http` with the path
/// `/oauth/callback`, no userinfo, no query and no fragment.
fn redirect_ok(uri: &str) -> bool {
    let Some(rest) = uri
        .strip_prefix("https://")
        .or_else(|| uri.strip_prefix("http://"))
    else {
        return false;
    };
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    !authority.is_empty()
        && !authority.contains('@')
        && path == "oauth/callback"
        && !uri
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '\\' | '?' | '#'))
}

/// A state of the deck: 16 to 256 characters of `[A-Za-z0-9_-]`.
fn state_ok(state: &str) -> bool {
    (16..=256).contains(&state.len())
        && state
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn text_ok(value: &str, max: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

impl Accounts {
    /// `accounts.app_set`: stores the app and increases its generation.
    ///
    /// # Errors
    /// `invalid_params`; `failed` when the store cannot be written.
    pub fn app_set(&self, app: AppSet) -> Result<AppView, Fail> {
        if !matches!(app.kind, AccountKind::Jira | AccountKind::Linear)
            || !text_ok(&app.client_id, 4096)
            || !text_ok(&app.client_secret, 4096)
        {
            return Err(invalid("the app is not valid"));
        }
        let store = self.store()?;
        let generation = store
            .app(app.kind)
            .map_err(store_fail)?
            .map_or(1, |old| old.generation.saturating_add(1));
        let kind = app.kind;
        let client_id = app.client_id;
        store
            .set_app(
                kind,
                &StoredApp {
                    client_id: client_id.clone(),
                    client_secret: Secret::new(app.client_secret),
                    generation,
                },
            )
            .map_err(store_fail)?;
        // The old transactions of this kind are dead: their generation is old.
        self.transactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|t| t.kind != kind);
        Ok(AppView {
            kind,
            client_id,
            has_secret: true,
            generation,
        })
    }

    /// `accounts.oauth_start`.
    ///
    /// # Errors
    /// `invalid_params`; `unavailable` when the owner has set no app.
    pub fn oauth_start(&self, p: OAuthStartParams) -> Result<OAuthStarted, Fail> {
        if !matches!(p.kind, AccountKind::Jira | AccountKind::Linear) || !state_ok(&p.state) {
            return Err(invalid("the sign-in request is not valid"));
        }
        let app = self.effective_app(p.kind)?;
        // The built-in app has its own byte-equal list of redirects, checked
        // first; any other redirect gets the localhost error.
        if app.as_ref().is_some_and(|a| a.source == AppSource::Builtin) {
            if !LINEAR_BUILTIN_REDIRECTS.contains(&p.redirect_uri.as_str()) {
                return Err(Fail::new(
                    codes::BUILTIN_NEEDS_LOCALHOST,
                    BUILTIN_NEEDS_LOCALHOST_TEXT,
                ));
            }
        } else if !redirect_ok(&p.redirect_uri) {
            return Err(invalid("the sign-in request is not valid"));
        }
        let app = app.ok_or_else(|| {
            Fail::unavailable("the OAuth app of this tracker is not set; set it first")
        })?;
        let (url, verifier) = if p.kind == AccountKind::Jira {
            (
                format!(
                    "{JIRA_AUTHORIZE}?audience=api.atlassian.com&client_id={}&scope={JIRA_SCOPE}&redirect_uri={}&state={}&response_type=code&prompt=consent",
                    pct(&app.client_id),
                    pct(&p.redirect_uri),
                    pct(&p.state)
                ),
                None,
            )
        } else {
            let verifier = random_b64(32)?;
            let challenge = URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()));
            (
                format!(
                    "{LINEAR_AUTHORIZE}?client_id={}&redirect_uri={}&response_type=code&scope=read,write&state={}&code_challenge={challenge}&code_challenge_method=S256",
                    pct(&app.client_id),
                    pct(&p.redirect_uri),
                    pct(&p.state)
                ),
                Some(Secret::new(verifier)),
            )
        };
        let mut open = self
            .transactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let ttl = self.limits.oauth_ttl;
        open.retain(|t| t.started.elapsed() < ttl);
        if open.len() >= MAX_OPEN {
            open.remove(0);
        }
        open.push(Transaction {
            kind: p.kind,
            state: p.state,
            verifier,
            app,
            redirect_uri: p.redirect_uri,
            started: Instant::now(),
        });
        Ok(OAuthStarted { authorize_url: url })
    }

    /// `accounts.oauth_finish`: exchanges the code and stores the account.
    ///
    /// # Errors
    /// `invalid_params` for an unknown, used or expired state, an app that
    /// changed, or a bad code; `upstream`, `timeout` or `bad_answer` for the
    /// provider.
    pub async fn oauth_finish(&self, p: OAuthFinishParams) -> Result<AccountView, Fail> {
        if !text_ok(&p.code, 4096) || !state_ok(&p.state) {
            return Err(invalid("the sign-in answer is not valid"));
        }
        let tx = {
            let mut open = self
                .transactions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let at = open
                .iter()
                .position(|t| ct_eq(t.state.as_bytes(), p.state.as_bytes()));
            at.map(|i| open.remove(i))
        };
        let tx = tx
            .filter(|t| t.started.elapsed() < self.limits.oauth_ttl)
            .ok_or_else(|| invalid("this sign-in is unknown, used or expired"))?;
        self.effective_app(tx.kind)?
            .filter(|now| now.same_as(&tx.app))
            .ok_or_else(|| invalid("the OAuth app changed; start the sign-in again"))?;
        let ids = if tx.kind == AccountKind::Jira {
            self.finish_jira(&tx, &p.code).await?
        } else {
            self.finish_linear(&tx, &p.code).await?
        };
        let store = self.store()?;
        let first = ids.first().ok_or_else(|| Fail::bad_answer("no account"))?;
        let account = store
            .account(first)
            .map_err(store_fail)?
            .ok_or_else(|| Fail::bad_answer("no account"))?;
        Ok(self.view(first, &account))
    }

    async fn finish_jira(&self, tx: &Transaction, code_text: &str) -> Result<Vec<String>, Fail> {
        let app = &tx.app;
        let secret = app.client_secret.as_ref().ok_or_else(|| {
            Fail::unavailable("the OAuth app of this tracker is not set; set it first")
        })?;
        let body = json!({
            "grant_type": "authorization_code",
            "client_id": app.client_id,
            "client_secret": secret.expose(),
            "code": code_text,
            "redirect_uri": tx.redirect_uri,
        });
        let answer = http::post_json(&self.programs, JIRA_TOKEN, Vec::new(), &body).await?;
        let tokens = Tokens::parse(&answer)?;
        let bearer = tokens.bearer();
        let resources = http::get(&self.programs, JIRA_RESOURCES, vec![bearer.clone()]).await?;
        let sites: Vec<(String, String)> = resources
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let id = r["id"].as_str()?;
                let url = r["url"].as_str()?;
                (cloud_id_ok(id) && site_url_ok(url)).then(|| (id.to_owned(), url.to_owned()))
            })
            .take(MAX_SITES)
            .collect();
        let Some((first_id, _)) = sites.first() else {
            return Err(Fail::bad_answer(
                "this sign-in has no Jira site that the station can use",
            ));
        };
        let me = http::get(
            &self.programs,
            &format!("{JIRA_API}/{first_id}/rest/api/3/myself"),
            vec![bearer],
        )
        .await?;
        let user = me["displayName"]
            .as_str()
            .or(me["emailAddress"].as_str())
            .filter(|u| text_ok(u, 256))
            .ok_or_else(|| Fail::bad_answer("Jira gave no user name"))?
            .to_owned();
        let mut ids = Vec::new();
        for (cloud_id, url) in sites {
            let account = StoredAccount {
                kind: AccountKind::Jira,
                method: AccountMethod::Oauth,
                site: Some(url),
                cloud_id: Some(cloud_id),
                user: user.clone(),
                access_token: Secret::new(tokens.access.clone()),
                refresh_token: tokens.refresh.as_ref().map(|t| Secret::new(t.clone())),
                expires_at: tokens.expires_at,
                email: None,
                generation: 0,
                app_source: None,
                app_client_id: None,
            };
            ids.push(self.save_account(&account).await?);
        }
        Ok(ids)
    }

    async fn finish_linear(&self, tx: &Transaction, code_text: &str) -> Result<Vec<String>, Fail> {
        let app = &tx.app;
        let verifier = tx
            .verifier
            .as_ref()
            .ok_or_else(|| Fail::bad_answer("the sign-in has no verifier"))?;
        let mut fields = vec![
            ("grant_type", "authorization_code"),
            ("code", code_text),
            ("redirect_uri", tx.redirect_uri.as_str()),
            ("client_id", app.client_id.as_str()),
            ("code_verifier", verifier.expose().as_str()),
        ];
        // The built-in app is a public client: no secret.
        if let Some(secret) = &app.client_secret {
            fields.push(("client_secret", secret.expose().as_str()));
        }
        let answer = http::post_form(&self.programs, LINEAR_TOKEN, &fields).await?;
        let tokens = Tokens::parse(&answer)?;
        let viewer = http::post_json(
            &self.programs,
            LINEAR_GRAPHQL,
            vec![tokens.bearer()],
            &json!({"query": "query { viewer { name organization { name urlKey } } }"}),
        )
        .await?;
        let data = &viewer["data"]["viewer"];
        let user = data["name"]
            .as_str()
            .filter(|u| text_ok(u, 256))
            .ok_or_else(|| Fail::bad_answer("Linear gave no user name"))?;
        let key = data["organization"]["urlKey"]
            .as_str()
            .filter(|k| {
                (1..=64).contains(&k.len())
                    && k.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
            .ok_or_else(|| Fail::bad_answer("Linear gave no workspace"))?;
        let account = StoredAccount {
            kind: AccountKind::Linear,
            method: AccountMethod::Oauth,
            site: Some(format!("https://linear.app/{key}")),
            cloud_id: None,
            user: user.to_owned(),
            access_token: Secret::new(tokens.access),
            refresh_token: tokens.refresh.map(Secret::new),
            expires_at: tokens.expires_at,
            email: None,
            generation: 0,
            app_source: Some(app.source.into()),
            app_client_id: Some(app.client_id.clone()),
        };
        Ok(vec![self.save_account(&account).await?])
    }

    /// The access token of the account `id`, refreshed first when it ends in
    /// less than a minute. One refresh runs at a time for each account; the
    /// Jira accounts share one lock, because one rotating refresh token
    /// serves all the sites of a sign-in.
    ///
    /// # Errors
    /// `invalid_params` for an unknown account; `unavailable` when the
    /// account needs a new sign-in.
    pub async fn fresh(&self, id: &str) -> Result<Secret<String>, Fail> {
        let load = || {
            self.store()?
                .account(id)
                .map_err(store_fail)?
                .ok_or_else(|| invalid("no account has this id"))
        };
        let due = |a: &StoredAccount| {
            a.method == AccountMethod::Oauth
                && a.expires_at
                    .is_some_and(|at| at - now_secs() < REFRESH_AHEAD)
        };
        let account = load()?;
        if !due(&account) {
            return Ok(account.access_token);
        }
        let lock = self.lifecycle_lock(id);
        let _one_at_a_time = lock.lock().await;
        // Read again inside the lock: a refresh that just ended has stored
        // the new pair.
        let account = load()?;
        if !due(&account) {
            return Ok(account.access_token);
        }
        match self.refresh(id, &account).await {
            Ok(token) => {
                self.set_needs_login(id, false);
                Ok(token)
            }
            // The account was logged out or replaced during the refresh: it
            // does not need a sign-in, it is gone or new.
            Err(fail) if fail.code == code::INVALID_PARAMS => Err(fail),
            // The app of the account is gone or unknown: the owner connects
            // the account again (DL-185).
            Err(fail) if fail.code == codes::APP_CHANGED => {
                self.set_needs_reconnect(id);
                tracing::warn!(account = %id, "the Linear app of the account changed");
                Err(fail)
            }
            Err(fail) => {
                self.set_needs_login(id, true);
                tracing::warn!(account = %id, code = fail.code, "the token refresh failed");
                Err(Fail::unavailable(
                    "the account needs a new sign-in; sign in again in the settings",
                ))
            }
        }
    }

    async fn refresh(&self, id: &str, account: &StoredAccount) -> Result<Secret<String>, Fail> {
        let store = self.store()?;
        let Some(old) = account.refresh_token.as_ref() else {
            return Err(Fail::unavailable("the account has no refresh token or app"));
        };
        // For a legacy Linear account that Linear accepted: the client id
        // of the owner app that this write binds the account to.
        let mut bind = None;
        let answer = if account.kind == AccountKind::Jira {
            let Some(app) = store.app(AccountKind::Jira).map_err(store_fail)? else {
                return Err(Fail::unavailable("the account has no refresh token or app"));
            };
            http::post_json(
                &self.programs,
                JIRA_TOKEN,
                Vec::new(),
                &json!({
                    "grant_type": "refresh_token",
                    "client_id": app.client_id,
                    "client_secret": app.client_secret.expose(),
                    "refresh_token": old.expose(),
                }),
            )
            .await
            .map_err(|e| e.fail)?
        } else {
            let (answer, bound) = self.refresh_linear(account, old).await?;
            bind = bound;
            answer
        };
        // A legacy account binds only to an answer that holds tokens.
        let tokens = Tokens::parse(&answer)
            .map_err(|fail| if bind.is_some() { app_changed() } else { fail })?;
        let new_refresh = tokens
            .refresh
            .clone()
            .unwrap_or_else(|| old.expose().clone());
        // The file must be the one that was read: a logout or a replacement
        // that came while the request was open wins, and the answer is
        // dropped (PRE-PR r1).
        if store
            .account(id)
            .map_err(store_fail)?
            .is_none_or(|now| now.generation != account.generation)
        {
            return Err(invalid("the account changed during the refresh"));
        }
        // The new pair is written before the access token is used.
        let mut updated = account.clone();
        updated.access_token = Secret::new(tokens.access.clone());
        updated.refresh_token = Some(Secret::new(new_refresh.clone()));
        updated.expires_at = tokens.expires_at;
        if let Some(client_id) = bind {
            updated.app_source = Some(AppSourceDisk::Owner);
            updated.app_client_id = Some(client_id);
        }
        store.put_account(id, &updated).map_err(store_fail)?;
        // The other Jira sites of the same sign-in share the rotated pair.
        if account.kind == AccountKind::Jira {
            for other in store.account_ids().map_err(store_fail)? {
                if other == id {
                    continue;
                }
                let Ok(Some(mut sibling)) = store.account(&other) else {
                    continue;
                };
                let same = sibling
                    .refresh_token
                    .as_ref()
                    .is_some_and(|t| t.ct_eq(old.expose().as_bytes()));
                if sibling.kind == AccountKind::Jira && sibling.method == account.method && same {
                    sibling.access_token = Secret::new(tokens.access.clone());
                    sibling.refresh_token = Some(Secret::new(new_refresh.clone()));
                    sibling.expires_at = tokens.expires_at;
                    store.put_account(&other, &sibling).map_err(store_fail)?;
                    self.set_needs_login(&other, false);
                }
            }
        }
        Ok(Secret::new(tokens.access))
    }
}

/// The tokens of a token endpoint answer.
struct Tokens {
    access: String,
    refresh: Option<String>,
    expires_at: Option<i64>,
}

impl Tokens {
    fn parse(answer: &Value) -> Result<Self, Fail> {
        let token = |v: &Value| {
            v.as_str()
                .filter(|t| text_ok(t, 8192) && !t.contains(char::is_whitespace))
                .map(str::to_owned)
        };
        let access = token(&answer["access_token"])
            .ok_or_else(|| Fail::new(code::BAD_ANSWER, "the provider gave no access token"))?;
        Ok(Self {
            access,
            refresh: token(&answer["refresh_token"]),
            expires_at: answer["expires_in"]
                .as_i64()
                .filter(|s| *s > 0)
                .map(|s| now_secs().saturating_add(s)),
        })
    }

    fn bearer(&self) -> String {
        format!("Authorization: Bearer {}", self.access)
    }
}

fn cloud_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `https://<host>` without a path, a user or a query.
fn site_url_ok(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|h| valid_host(h) && !h.contains('/'))
}

fn random_b64(bytes: usize) -> Result<String, Fail> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| Fail::new(code::FAILED, "the system random source failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_redirect_and_the_state_are_checked() {
        assert!(redirect_ok("https://deck.example/oauth/callback"));
        assert!(redirect_ok("http://127.0.0.1:7700/oauth/callback"));
        for bad in [
            "https://deck.example/other",
            "https://u@deck.example/oauth/callback",
            "https://deck.example/oauth/callback?x=1",
            "https://deck.example/oauth/callback#f",
            "ftp://deck.example/oauth/callback",
            "https:///oauth/callback",
            "https://deck.example/oauth/callback ",
        ] {
            assert!(!redirect_ok(bad), "{bad}");
        }
        assert!(state_ok(&"a".repeat(43)));
        assert!(!state_ok("short"));
        assert!(!state_ok(&format!("{}&x", "a".repeat(20))));
    }

    #[test]
    fn a_site_url_is_https_without_a_path() {
        assert!(site_url_ok("https://acme.atlassian.net"));
        for bad in [
            "http://acme.atlassian.net",
            "https://acme.atlassian.net/x",
            "https://u@acme.atlassian.net",
            "https://",
        ] {
            assert!(!site_url_ok(bad), "{bad}");
        }
    }
}
