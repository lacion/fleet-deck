//! Jira Cloud and Linear: one handle per call. A handle gets its credential
//! from a `[[trackers]]` entry (an environment variable or a 0600 file), or
//! from an account of the station store (DL-173). The credential goes to
//! `curl` on stdin.
//!
//! The auth scheme follows the kind and the method of the account:
//!
//! | kind   | method | header                                    | API origin                                  |
//! |--------|--------|-------------------------------------------|---------------------------------------------|
//! | Jira   | token  | `Authorization: Basic base64(email:token)` | the site URL                                |
//! | Jira   | OAuth  | `Authorization: Bearer <access>`           | `https://api.atlassian.com/ex/jira/<cloud>` |
//! | Linear | key    | `Authorization: <key>`                     | `https://api.linear.app`                    |
//! | Linear | OAuth  | `Authorization: Bearer <access>`           | `https://api.linear.app`                    |

use fleetdeck_core::Secret;
use fleetdeck_core::config::TrackerConfig;
use fleetdeck_proto::accounts::{AccountMethod, Container};
use fleetdeck_proto::integrations::{
    Asset, IssueDetail, IssueSearch, IssueSummary, TrackerKind, code,
};
use serde_json::Value;

use super::exec::{Fail, Programs};
use super::http::{self, MAX_JSON, Request};
use super::{asset, jira, linear};
use crate::accounts::{AccountMeta, Accounts};

/// Where the credential of a handle comes from.
enum Cred<'a> {
    /// A token that the handle holds for this call.
    Token(Secret<String>),
    /// An account of the store: the access token is read (and refreshed)
    /// when a call needs it.
    Account { accounts: &'a Accounts, id: String },
}

/// A tracker with its credential, for one call.
pub struct Tracker<'a> {
    pub programs: &'a Programs,
    kind: TrackerKind,
    method: AccountMethod,
    /// Jira only: the site URL, for display and for a token account.
    site: Option<String>,
    /// Jira OAuth only.
    cloud_id: Option<String>,
    /// Jira only: the e-mail of the Basic pair.
    email: Option<String>,
    cred: Cred<'a>,
    /// The project of the entry's binding.
    pub key: &'a str,
}

/// The header line that authenticates a call, and every secret form of it
/// (for the scrub).
#[must_use]
pub fn header_for(
    kind: TrackerKind,
    method: AccountMethod,
    email: Option<&str>,
    token: &str,
) -> (String, Vec<String>) {
    match (kind, method) {
        (_, AccountMethod::Oauth) => (
            format!("Authorization: Bearer {token}"),
            vec![token.to_owned()],
        ),
        (TrackerKind::Jira, _) => {
            use base64::Engine;
            let pair = format!("{}:{token}", email.unwrap_or_default());
            let encoded = base64::engine::general_purpose::STANDARD.encode(pair);
            (
                format!("Authorization: Basic {encoded}"),
                vec![token.to_owned(), encoded],
            )
        }
        (TrackerKind::Linear, _) => (format!("Authorization: {token}"), vec![token.to_owned()]),
    }
}

impl<'a> Tracker<'a> {
    /// A handle for a `[[trackers]]` entry.
    ///
    /// # Errors
    /// [`Fail`] when the token cannot be read.
    pub fn open(programs: &'a Programs, cfg: &TrackerConfig, key: &'a str) -> Result<Self, Fail> {
        let token = cfg
            .read_token()
            .map_err(|e| Fail::unavailable(format!("tracker `{}`: {e}", cfg.id)))?;
        Ok(Self {
            programs,
            kind: cfg.kind,
            method: AccountMethod::Config,
            site: cfg.site.clone(),
            cloud_id: None,
            email: cfg.email.clone(),
            cred: Cred::Token(token),
            key,
        })
    }

    /// A handle for the account `id` of the store.
    #[must_use]
    pub fn for_account(
        programs: &'a Programs,
        accounts: &'a Accounts,
        id: &str,
        meta: AccountMeta,
        kind: TrackerKind,
        key: &'a str,
    ) -> Self {
        Self {
            programs,
            kind,
            method: meta.method,
            site: meta.site,
            cloud_id: meta.cloud_id,
            email: meta.email,
            cred: Cred::Account {
                accounts,
                id: id.to_owned(),
            },
            key,
        }
    }

    /// A handle for a token that is not stored yet (the check of
    /// `accounts.token_set`).
    #[must_use]
    pub fn stateless(
        programs: &'a Programs,
        kind: TrackerKind,
        method: AccountMethod,
        site: Option<String>,
        email: Option<String>,
        token: Secret<String>,
    ) -> Self {
        Self {
            programs,
            kind,
            method,
            site,
            cloud_id: None,
            email,
            cred: Cred::Token(token),
            key: "",
        }
    }

    /// The site of a Jira entry, for display URLs.
    pub fn site(&self) -> &str {
        self.site.as_deref().unwrap_or_default()
    }

    /// The origin of Jira API calls: the site, or the Atlassian gateway of
    /// an OAuth account.
    pub fn base(&self) -> String {
        match (&self.cloud_id, self.method) {
            (Some(cloud), AccountMethod::Oauth) => {
                format!("https://api.atlassian.com/ex/jira/{cloud}")
            }
            _ => self.site().to_owned(),
        }
    }

    /// The header line that authenticates a call, and every secret form of
    /// it. An account token is refreshed first when it is due.
    async fn auth(&self) -> Result<(String, Vec<String>), Fail> {
        let token = match &self.cred {
            Cred::Token(t) => t.expose().clone(),
            Cred::Account { accounts, id } => accounts.fresh(id).await?.into_exposed(),
        };
        Ok(header_for(
            self.kind,
            self.method,
            self.email.as_deref(),
            &token,
        ))
    }

    /// A JSON call. An empty answer (204) gives `null`.
    ///
    /// # Errors
    /// [`Fail`].
    pub async fn json(
        &self,
        method: &'static str,
        url: String,
        body: Option<&Value>,
    ) -> Result<Value, Fail> {
        let (header, secrets) = self.auth().await?;
        let request = Request {
            url,
            method,
            headers: vec![
                header,
                "Accept: application/json".to_owned(),
                "Content-Type: application/json".to_owned(),
            ],
            body: body.map(Value::to_string),
            ..Request::default()
        };
        let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
        let bytes = http::send(self.programs, &request, &secrets, MAX_JSON).await?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Fail::bad_answer("the tracker answer is not JSON"))
    }

    pub async fn search(&self, q: &IssueSearch) -> Result<Vec<IssueSummary>, Fail> {
        match self.kind {
            TrackerKind::Jira => jira::search(self, q).await,
            TrackerKind::Linear => linear::search(self, q).await,
        }
    }

    pub async fn get(&self, key: &str) -> Result<IssueDetail, Fail> {
        match self.kind {
            TrackerKind::Jira => jira::get(self, key).await,
            TrackerKind::Linear => linear::get(self, key).await,
        }
    }

    pub async fn comment(&self, key: &str, body: &str) -> Result<String, Fail> {
        match self.kind {
            TrackerKind::Jira => jira::comment(self, key, body).await,
            TrackerKind::Linear => linear::comment(self, key, body).await,
        }
    }

    pub async fn transition(&self, key: &str, to: &str) -> Result<String, Fail> {
        match self.kind {
            TrackerKind::Jira => jira::transition(self, key, to).await,
            TrackerKind::Linear => linear::transition(self, key, to).await,
        }
    }

    /// The user that the token acts as.
    pub async fn whoami(&self) -> Result<String, Fail> {
        match self.kind {
            TrackerKind::Jira => jira::whoami(self).await,
            TrackerKind::Linear => linear::whoami(self).await,
        }
    }

    /// The Jira projects or the Linear teams that the account can see.
    pub async fn containers(&self) -> Result<Vec<Container>, Fail> {
        match self.kind {
            TrackerKind::Jira => jira::containers(self).await,
            TrackerKind::Linear => linear::containers(self).await,
        }
    }

    /// The name and the workspace key of a Linear key (the check of
    /// `accounts.token_set`).
    pub async fn linear_identity(&self) -> Result<(String, Option<String>), Fail> {
        linear::identity(self).await
    }

    /// An image of an issue body. The credential goes only to the tracker's
    /// own host: the site of a token account, the Atlassian gateway of a Jira
    /// OAuth account, or `uploads.linear.app`.
    pub async fn asset(&self, url: &str) -> Result<Asset, Fail> {
        let (header, secrets) = self.auth().await?;
        let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
        let (hosts, own) = match self.kind {
            TrackerKind::Jira => {
                let host = super::util::https_host(&self.base()).ok_or_else(|| {
                    Fail::new(
                        code::UNAVAILABLE,
                        "the tracker site is not a valid https URL",
                    )
                })?;
                (vec![host.clone()], host)
            }
            TrackerKind::Linear => (
                vec!["uploads.linear.app".to_owned()],
                "uploads.linear.app".to_owned(),
            ),
        };
        asset::fetch(self.programs, url, &hosts, Some((&header, &own)), &secrets).await
    }
}

/// A Jira key or a Linear identifier, `KEY-123`: its number.
#[must_use]
pub fn issue_number(key: &str) -> Option<u64> {
    key.rsplit_once('-').and_then(|(_, n)| n.parse().ok())
}

/// True when `text` is only an issue key (`PROJ-123`).
#[must_use]
pub fn is_issue_key(text: &str) -> bool {
    let t = text.trim();
    fleetdeck_proto::integrations::valid_issue_key(
        &fleetdeck_proto::integrations::Binding::Tracker(
            fleetdeck_proto::integrations::TrackerBinding {
                tracker: "x".into(),
                kind: TrackerKind::Jira,
                key: "X".into(),
            },
        ),
        t,
    )
}

/// The estimate text of a number of story points.
#[must_use]
pub fn points(n: f64) -> String {
    if (n - n.round()).abs() < f64::EPSILON {
        format!("{} points", n as i64)
    } else {
        format!("{n} points")
    }
}
