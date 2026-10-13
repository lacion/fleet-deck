//! Accounts on the wire (DL-173): the connected GitHub, GitLab, Jira and
//! Linear accounts of a station, the OAuth apps, the CLI device login and
//! the OAuth start and finish.
//!
//! The station stores every credential. A value that the owner types in the
//! board (an app secret, an API token) or an OAuth authorization code
//! crosses the deck once through the credential relay; the types that carry
//! one have a hand-written `Debug` that hides it. A view never holds a
//! credential.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The systems that an account connects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "accounts/")]
pub enum AccountKind {
    Github,
    Gitlab,
    Jira,
    Linear,
}

/// How an account signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "accounts/")]
pub enum AccountMethod {
    Oauth,
    Token,
    /// The login of the `gh` or `glab` CLI.
    Cli,
    /// A `[[trackers]]` entry of `station.toml`.
    Config,
}

/// The state of an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "accounts/")]
pub enum AccountStatus {
    Ok,
    NeedsLogin,
    Error,
}

/// One connected account. It holds metadata only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "accounts/")]
pub struct AccountView {
    pub account_id: String,
    pub kind: AccountKind,
    /// The site URL (Jira) or the host (a forge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user: Option<String>,
    pub method: AccountMethod,
    pub status: AccountStatus,
    /// A fixed text for the owner; never an upstream text.
    pub message: String,
}

/// The OAuth app of a tracker, without its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "accounts/")]
pub struct AppView {
    pub kind: AccountKind,
    pub client_id: String,
    pub has_secret: bool,
    /// Increases on each change of the app.
    pub generation: u32,
}

/// The OAuth app that the owner registered. `Debug` hides the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "accounts/")]
pub struct AppSet {
    pub kind: AccountKind,
    pub client_id: String,
    pub client_secret: String,
}

impl fmt::Debug for AppSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppSet")
            .field("kind", &self.kind)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

/// An API token that the owner typed. `Debug` hides the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "accounts/")]
pub struct TokenSet {
    pub kind: AccountKind,
    /// The Jira site URL, or the forge host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub site: Option<String>,
    /// The Jira account email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub email: Option<String>,
    pub token: String,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("kind", &self.kind)
            .field("site", &self.site)
            .field("email", &self.email)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Starts the CLI device login of a forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "accounts/")]
pub struct ForgeLoginStart {
    pub kind: AccountKind,
    pub host: String,
}

/// The code and the page of a device login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "accounts/")]
pub struct ForgeLogin {
    pub login_id: String,
    pub code: String,
    pub url: String,
    /// Seconds until the login ends.
    pub expires_in: u32,
}

/// The state of a device login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "accounts/")]
pub enum ForgeLoginState {
    Pending,
    Done { user: String },
    Failed { code: String },
    Expired,
}

/// The status of a device login, by id (a link method param).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "accounts/")]
pub struct ForgeLoginQuery {
    pub login_id: String,
}

/// An account, by id (a link method param).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "accounts/")]
pub struct AccountRef {
    pub account_id: String,
}

/// The deck asks the station for the authorize URL of a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "accounts/")]
pub struct OAuthStartParams {
    pub kind: AccountKind,
    pub state: String,
    pub redirect_uri: String,
}

/// The page where the owner grants access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "accounts/")]
pub struct OAuthStarted {
    pub authorize_url: String,
}

/// The deck gives the station the authorization code. `Debug` hides it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "accounts/")]
pub struct OAuthFinishParams {
    pub state: String,
    pub code: String,
}

impl fmt::Debug for OAuthFinishParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthFinishParams")
            .field("state", &"<redacted>")
            .field("code", &"<redacted>")
            .finish()
    }
}

/// A Jira project or a Linear team that a project can bind to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "accounts/")]
pub struct Container {
    pub key: String,
    pub name: String,
}

/// The answer of `issues.containers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "accounts/")]
pub struct Containers {
    pub containers: Vec<Container>,
}

/// The accounts of a station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "accounts/")]
pub struct AccountsList {
    pub accounts: Vec<AccountView>,
    pub apps: Vec<AppView>,
    /// The names of the token variables found in the station environment
    /// (`GH_TOKEN`, ...). Never their values.
    pub env_tokens: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_of_the_relay_types_hides_every_secret() {
        let app = AppSet {
            kind: AccountKind::Jira,
            client_id: "client-1".into(),
            client_secret: "canary-secret-1".into(),
        };
        let token = TokenSet {
            kind: AccountKind::Linear,
            site: None,
            email: Some("a@b.c".into()),
            token: "canary-token-2".into(),
        };
        let finish = OAuthFinishParams {
            state: "canary-state-3".into(),
            code: "canary-code-4".into(),
        };
        let text = format!("{app:?} {app:#?} {token:?} {token:#?} {finish:?} {finish:#?}");
        for canary in [
            "canary-secret-1",
            "canary-token-2",
            "canary-state-3",
            "canary-code-4",
        ] {
            assert!(!text.contains(canary), "{text}");
        }
        assert!(text.contains("client-1") && text.contains("a@b.c"));
    }

    #[test]
    fn a_login_state_has_the_state_tag() {
        let done = serde_json::to_value(ForgeLoginState::Done { user: "ana".into() }).unwrap();
        assert_eq!(done, serde_json::json!({"state": "done", "user": "ana"}));
        let pending = serde_json::to_value(ForgeLoginState::Pending).unwrap();
        assert_eq!(pending, serde_json::json!({"state": "pending"}));
        let failed: ForgeLoginState =
            serde_json::from_str(r#"{"state":"failed","code":"env_token_present"}"#).unwrap();
        assert_eq!(
            failed,
            ForgeLoginState::Failed {
                code: "env_token_present".into()
            }
        );
    }

    #[test]
    fn an_account_view_leaves_out_missing_options() {
        let v = AccountView {
            account_id: "a1".into(),
            kind: AccountKind::Github,
            site: None,
            user: None,
            method: AccountMethod::Cli,
            status: AccountStatus::NeedsLogin,
            message: "sign in".into(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert!(json.get("site").is_none() && json.get("user").is_none());
        assert_eq!(json["status"], "needs_login");
    }

    #[test]
    fn the_relay_requests_refuse_unknown_fields() {
        assert!(
            serde_json::from_str::<TokenSet>(r#"{"kind":"jira","token":"t","extra":1}"#).is_err()
        );
        assert!(
            serde_json::from_str::<AppSet>(
                r#"{"kind":"jira","client_id":"c","client_secret":"s","x":1}"#
            )
            .is_err()
        );
    }
}
