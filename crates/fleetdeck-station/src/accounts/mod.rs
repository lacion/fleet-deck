//! The accounts of a station (DL-173, design `settings-and-accounts.md`
//! §2): the `accounts.*` link methods.
//!
//! - [`store`]: the credential directory `<station data>/accounts/`.
//! - [`forge`]: GitHub and GitLab through the `gh` and `glab` CLIs (device
//!   login, token login, logout, status).
//! - [`oauth`]: the Jira and Linear OAuth apps, the authorize URL, the code
//!   exchange and the token refresh.
//! - [`tokens`]: the Jira and Linear API tokens.
//! - [`http`]: the HTTPS calls of this module.
//!
//! The station is the only holder of a credential. A value that crosses the
//! link (an app secret, an API token, an authorization code) is used in
//! memory and never reaches argv, a log, an error or a `Debug` text: every
//! error of this module has a fixed code and a fixed message, and an
//! upstream error is reduced to its HTTP status. The integration service
//! reads an account through [`Accounts::meta`] and [`Accounts::fresh`].

pub mod forge;
pub mod forge_parse;
pub mod http;
pub mod oauth;
pub mod oauth_app;
pub mod store;
pub mod tokens;

#[cfg(test)]
mod forge_tests;
#[cfg(test)]
mod oauth_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use fleetdeck_core::config::TrackerConfig;
use fleetdeck_proto::accounts::{
    AccountKind, AccountMethod, AccountRef, AccountStatus, AccountView, AccountsList, AppSet,
    AppView, ForgeLoginQuery, ForgeLoginStart, OAuthFinishParams, OAuthStartParams, TokenSet,
};
use fleetdeck_proto::integrations::{TrackerKind, code};
use ring::rand::{SecureRandom, SystemRandom};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::integrations::exec::{Fail, Programs};
use store::{AccountStore, StoreError, StoredAccount};

/// The error codes of this module that the integration codes lack.
pub mod codes {
    /// A token variable of the station environment overrides a CLI login.
    pub const ENV_TOKEN_PRESENT: &str = "env_token_present";
    /// The CLI or the server has no device login.
    pub const DEVICE_FLOW_UNSUPPORTED: &str = "device_flow_unsupported";
    /// The CLI printed something that the station does not accept.
    pub const UNEXPECTED_CLI_OUTPUT: &str = "unexpected_cli_output";
    /// The station already runs three device logins.
    pub const LOGIN_BUSY: &str = "login_busy";
    /// The built-in Linear app is used from an address it does not allow
    /// (DL-185).
    pub const BUILTIN_NEEDS_LOCALHOST: &str = "builtin_needs_localhost";
    /// The Linear app of an account is not the app that the station has
    /// now (DL-185).
    pub const APP_CHANGED: &str = "app_changed";
}

/// The link methods of this service.
pub const METHODS: &[&str] = &[
    "accounts.list",
    "accounts.forge_login",
    "accounts.forge_login_status",
    "accounts.logout",
    "accounts.app_set",
    "accounts.token_set",
    "accounts.oauth_start",
    "accounts.oauth_finish",
];

/// True for a method of this service.
#[must_use]
pub fn serves(method: &str) -> bool {
    METHODS.contains(&method)
}

/// The times of the service. Tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// How long a device login lives.
    pub login_ttl: Duration,
    /// How long the start of a device login waits for the code.
    pub start_wait: Duration,
    /// How long an OAuth transaction lives.
    pub oauth_ttl: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            login_ttl: forge::LOGIN_TTL,
            start_wait: forge::START_WAIT,
            oauth_ttl: oauth::TTL,
        }
    }
}

/// What an account is, without its credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMeta {
    pub kind: AccountKind,
    pub method: AccountMethod,
    pub site: Option<String>,
    pub cloud_id: Option<String>,
    pub email: Option<String>,
}

/// The accounts service of one station. The integration service shares it
/// between its rebuilds.
pub struct Accounts {
    pub(crate) store: OnceLock<AccountStore>,
    pub(crate) programs: Programs,
    pub(crate) limits: Limits,
    /// True when the named variable is set in the station environment.
    pub(crate) env_probe: fn(&str) -> bool,
    /// True when the built-in Linear app is on (DL-185 release gate).
    pub(crate) builtin_probe: fn() -> bool,
    pub(crate) logins: forge::Logins,
    pub(crate) login_gate: tokio::sync::Mutex<()>,
    pub(crate) needs_login: Mutex<HashSet<String>>,
    /// The accounts whose Linear app changed (DL-185).
    pub(crate) needs_reconnect: Mutex<HashSet<String>>,
    pub(crate) transactions: Mutex<Vec<oauth::Transaction>>,
    /// The lifecycle locks of the stored accounts (PRE-PR r1): one for all
    /// the Jira accounts (key `jira`: one rotating refresh token serves all
    /// the sites of a sign-in) and one for each Linear account (its id).
    /// Logout, token replacement, a sign-in that replaces an account and the
    /// refresh take the lock of the account.
    pub(crate) refresh_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for Accounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Accounts")
            .field("attached", &self.store.get().is_some())
            .finish_non_exhaustive()
    }
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

impl Accounts {
    /// A service without a store. [`Accounts::attach`] gives it one.
    #[must_use]
    pub fn new(programs: Programs) -> Self {
        Self {
            store: OnceLock::new(),
            programs,
            limits: Limits::default(),
            env_probe: env_set,
            builtin_probe: oauth::linear_builtin_enabled,
            logins: forge::Logins::default(),
            login_gate: tokio::sync::Mutex::new(()),
            needs_login: Mutex::new(HashSet::new()),
            needs_reconnect: Mutex::new(HashSet::new()),
            transactions: Mutex::new(Vec::new()),
            refresh_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Gives the service its store (the station start).
    pub fn attach(&self, store: AccountStore) {
        if self.store.set(store).is_err() {
            tracing::warn!("the account store was attached twice");
        }
    }

    /// The lifecycle lock of the stored account `id` (see `refresh_locks`).
    pub(crate) fn lifecycle_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let key = if id.starts_with("jira-") { "jira" } else { id };
        let mut locks = self
            .refresh_locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(key.to_owned()).or_default())
    }

    pub(crate) fn runner(&self) -> forge::Runner {
        forge::Runner {
            programs: self.programs.clone(),
        }
    }

    pub(crate) fn store(&self) -> Result<&AccountStore, Fail> {
        self.store
            .get()
            .ok_or_else(|| Fail::unavailable("the account store is not available"))
    }

    /// What the account `id` is. `None` when the store has no such account.
    ///
    /// # Errors
    /// [`Fail`] when the store cannot be read.
    pub fn meta(&self, id: &str) -> Result<Option<AccountMeta>, Fail> {
        let Some(store) = self.store.get() else {
            return Ok(None);
        };
        if !store::valid_account_id(id) {
            return Ok(None);
        }
        Ok(store.account(id).map_err(store_fail)?.map(|a| AccountMeta {
            kind: a.kind,
            method: a.method,
            site: a.site,
            cloud_id: a.cloud_id,
            email: a.email,
        }))
    }

    /// Answers one `accounts.*` request. `trackers` are the `[[trackers]]`
    /// entries of the station config.
    ///
    /// # Errors
    /// [`Fail`] with a fixed code and a fixed message.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        trackers: &[TrackerConfig],
    ) -> Result<Value, Fail> {
        match method {
            "accounts.list" => {
                parse::<Empty>(params)?;
                to_value(&self.list(trackers).await?)
            }
            "accounts.forge_login" => {
                let p: ForgeLoginStart = parse(params)?;
                to_value(&self.forge_login(p.kind, &p.host).await?)
            }
            "accounts.forge_login_status" => {
                let p: ForgeLoginQuery = parse(params)?;
                to_value(&self.forge_login_status(&p.login_id))
            }
            "accounts.logout" => {
                let p: AccountRef = parse(params)?;
                self.logout(&p.account_id, trackers).await?;
                Ok(serde_json::json!({}))
            }
            "accounts.app_set" => {
                let p: AppSet = parse(params)?;
                to_value(&self.app_set(p)?)
            }
            "accounts.token_set" => {
                let p: TokenSet = parse(params)?;
                to_value(&self.token_set(p).await?)
            }
            "accounts.oauth_start" => {
                let p: OAuthStartParams = parse(params)?;
                to_value(&self.oauth_start(p)?)
            }
            "accounts.oauth_finish" => {
                let p: OAuthFinishParams = parse(params)?;
                to_value(&self.oauth_finish(p).await?)
            }
            _ => Err(Fail::new(
                code::UNAVAILABLE,
                "this station does not serve this method",
            )),
        }
    }

    /// `accounts.list`.
    async fn list(&self, trackers: &[TrackerConfig]) -> Result<AccountsList, Fail> {
        let mut accounts = Vec::new();
        let mut apps = Vec::new();
        if let Some(store) = self.store.get() {
            let ids = store.account_ids().map_err(store_fail)?;
            for id in ids {
                match store.account(&id) {
                    Ok(Some(a)) => accounts.push(self.view(&id, &a)),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(account = %id, error = %e, "unusable account file"),
                }
            }
            for kind in [AccountKind::Jira, AccountKind::Linear] {
                if let Some(app) = store.app(kind).map_err(store_fail)? {
                    apps.push(AppView {
                        kind,
                        client_id: app.client_id,
                        has_secret: true,
                        generation: app.generation,
                    });
                }
            }
        }
        accounts.extend(self.cli_accounts().await);
        accounts.extend(trackers.iter().map(config_view));
        Ok(AccountsList {
            accounts,
            apps,
            env_tokens: self.env_tokens(),
            linear_builtin: (self.builtin_probe)(),
        })
    }

    /// The view of a stored account.
    pub(crate) fn view(&self, id: &str, a: &StoredAccount) -> AccountView {
        let needs = self
            .needs_login
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id);
        let reconnect = self
            .needs_reconnect
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id);
        let (status, message) = if reconnect {
            (AccountStatus::NeedsReconnect, APP_CHANGED_TEXT)
        } else if needs {
            (AccountStatus::NeedsLogin, "Sign in again.")
        } else if a.method == AccountMethod::Oauth {
            (AccountStatus::Ok, "Connected with OAuth.")
        } else {
            (AccountStatus::Ok, "Connected with an API token.")
        };
        AccountView {
            account_id: id.to_owned(),
            kind: a.kind,
            site: a.site.clone(),
            user: Some(a.user.clone()),
            method: a.method,
            status,
            message: message.to_owned(),
        }
    }

    /// `accounts.logout`: a stored account is deleted (the provider is not
    /// told), a CLI account is signed out of its CLI, a config entry is
    /// refused.
    async fn logout(&self, id: &str, trackers: &[TrackerConfig]) -> Result<(), Fail> {
        if store::valid_account_id(id) {
            let lock = self.lifecycle_lock(id);
            let _lifecycle = lock.lock().await;
            let found = self.store()?.delete_account(id).map_err(store_fail)?;
            self.set_needs_login(id, false);
            return if found {
                Ok(())
            } else {
                Err(invalid("no account has this id"))
            };
        }
        if let Some((kind, host)) = forge::parse_cli_account_id(id) {
            return self.forge_logout(kind, &host).await;
        }
        if trackers.iter().any(|t| t.id == id) {
            return Err(invalid("this account comes from the config file"));
        }
        Err(invalid("no account has this id"))
    }

    /// Marks the account `id` as needing a new sign-in, or clears every
    /// mark of it (`needs` false clears `needs_reconnect` too).
    pub(crate) fn set_needs_login(&self, id: &str, needs: bool) {
        let mut set = self
            .needs_login
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut reconnect = self
            .needs_reconnect
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        reconnect.remove(id);
        if needs {
            set.insert(id.to_owned());
        } else {
            set.remove(id);
        }
    }

    /// Marks the account `id` as needing a new connection because its Linear
    /// app changed (DL-185). It replaces the needs-login mark.
    pub(crate) fn set_needs_reconnect(&self, id: &str) {
        let mut set = self
            .needs_login
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut reconnect = self
            .needs_reconnect
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        set.remove(id);
        reconnect.insert(id.to_owned());
    }

    /// Writes `account`, in the file of an account that has the same
    /// identity (the same kind, method and site) or in a new one. Returns
    /// the id. It holds the lifecycle lock of the account: for Jira the one
    /// lock of the family (also while it looks for the file), for Linear the
    /// lock of the account that it writes.
    pub(crate) async fn save_account(&self, account: &StoredAccount) -> Result<String, Fail> {
        let store = self.store()?;
        let family = (account.kind == AccountKind::Jira).then(|| self.lifecycle_lock("jira"));
        let _family = match &family {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        let mut found = None;
        for id in store.account_ids().map_err(store_fail)? {
            match store.account(&id) {
                Ok(Some(old))
                    if old.kind == account.kind
                        && old.method == account.method
                        && old.site == account.site
                        && old.cloud_id == account.cloud_id
                        && old.email == account.email
                        && same_linear_app(&old, account) =>
                {
                    found = Some(id);
                    break;
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(account = %id, error = %e, "unusable account file"),
            }
        }
        let id = match found {
            Some(id) => id,
            None => store::new_account_id(account.kind).map_err(store_fail)?,
        };
        let own = family.is_none().then(|| self.lifecycle_lock(&id));
        let _own = match &own {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        store.put_account(&id, account).map_err(store_fail)?;
        self.set_needs_login(&id, false);
        Ok(id)
    }
}

/// The fixed text of an account whose Linear app changed.
pub(crate) const APP_CHANGED_TEXT: &str =
    "The Linear app of this account changed. Connect the account again.";

/// A Linear OAuth account matches only an account made by the same app
/// (`app_source` and `app_client_id`), and a legacy record (no
/// `app_source`) never matches (DL-185). Every other account is unchanged.
fn same_linear_app(old: &StoredAccount, new: &StoredAccount) -> bool {
    if new.kind != AccountKind::Linear || new.method != AccountMethod::Oauth {
        return true;
    }
    old.app_source.is_some()
        && old.app_source == new.app_source
        && old.app_client_id == new.app_client_id
}

/// The view of a `[[trackers]]` entry. Its id is the tracker id that a
/// project binding names.
fn config_view(t: &TrackerConfig) -> AccountView {
    AccountView {
        account_id: t.id.clone(),
        kind: match t.kind {
            TrackerKind::Jira => AccountKind::Jira,
            TrackerKind::Linear => AccountKind::Linear,
        },
        site: t.site.clone(),
        user: None,
        method: AccountMethod::Config,
        status: AccountStatus::Ok,
        message: "From the config file.".to_owned(),
    }
}

/// The params of a method without params.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// `invalid_params` with a fixed message.
pub(crate) fn invalid(message: &'static str) -> Fail {
    Fail::new(code::INVALID_PARAMS, message)
}

/// Parses the params. The error is fixed: a serde error can quote a secret.
fn parse<T: DeserializeOwned>(params: Value) -> Result<T, Fail> {
    serde_json::from_value(params).map_err(|_| invalid("the params are not valid"))
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, Fail> {
    serde_json::to_value(value).map_err(|_| Fail::bad_answer("the answer cannot be encoded"))
}

/// The fixed failure of a store error. The log line has the error, which
/// never holds a credential.
pub(crate) fn store_fail(e: StoreError) -> Fail {
    tracing::warn!(error = %e, "account store");
    Fail::new(code::FAILED, "the account store could not be used")
}

/// `bytes` random bytes as hex.
pub(crate) fn random_id(bytes: usize) -> Result<String, Fail> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| Fail::new(code::FAILED, "the system random source failed"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The unix time in seconds.
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}
