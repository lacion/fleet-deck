//! Jira Cloud and Linear: one handle per call that holds the token for that
//! call only. The token comes from the `[[trackers]]` entry (an environment
//! variable or a 0600 file) and goes to `curl` on stdin.

use fleetdeck_core::Secret;
use fleetdeck_core::config::TrackerConfig;
use fleetdeck_proto::integrations::{
    Asset, IssueDetail, IssueSearch, IssueSummary, TrackerKind, code,
};
use serde_json::Value;

use super::exec::{Fail, Programs};
use super::http::{self, MAX_JSON, Request};
use super::{asset, jira, linear};

/// A tracker with its token, for one call.
pub struct Tracker<'a> {
    pub programs: &'a Programs,
    pub cfg: &'a TrackerConfig,
    token: Secret<String>,
    /// The projects of the entry's binding.
    pub key: &'a str,
}

impl<'a> Tracker<'a> {
    /// # Errors
    /// [`Fail`] when the token cannot be read.
    pub fn open(
        programs: &'a Programs,
        cfg: &'a TrackerConfig,
        key: &'a str,
    ) -> Result<Self, Fail> {
        let token = cfg
            .read_token()
            .map_err(|e| Fail::unavailable(format!("tracker `{}`: {e}", cfg.id)))?;
        Ok(Self {
            programs,
            cfg,
            token,
            key,
        })
    }

    pub fn token(&self) -> &str {
        self.token.expose()
    }

    /// The site of a Jira entry.
    pub fn site(&self) -> &str {
        self.cfg.site.as_deref().unwrap_or_default()
    }

    /// The header line that authenticates a call, and every secret form of
    /// it (for the scrub).
    pub fn auth_header(&self) -> (String, Vec<String>) {
        match self.cfg.kind {
            TrackerKind::Jira => {
                use base64::Engine;
                let pair = format!(
                    "{}:{}",
                    self.cfg.email.as_deref().unwrap_or_default(),
                    self.token()
                );
                let encoded = base64::engine::general_purpose::STANDARD.encode(pair);
                (
                    format!("Authorization: Basic {encoded}"),
                    vec![self.token().to_owned(), encoded],
                )
            }
            TrackerKind::Linear => (
                format!("Authorization: {}", self.token()),
                vec![self.token().to_owned()],
            ),
        }
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
        let (header, secrets) = self.auth_header();
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
        match self.cfg.kind {
            TrackerKind::Jira => jira::search(self, q).await,
            TrackerKind::Linear => linear::search(self, q).await,
        }
    }

    pub async fn get(&self, key: &str) -> Result<IssueDetail, Fail> {
        match self.cfg.kind {
            TrackerKind::Jira => jira::get(self, key).await,
            TrackerKind::Linear => linear::get(self, key).await,
        }
    }

    pub async fn comment(&self, key: &str, body: &str) -> Result<String, Fail> {
        match self.cfg.kind {
            TrackerKind::Jira => jira::comment(self, key, body).await,
            TrackerKind::Linear => linear::comment(self, key, body).await,
        }
    }

    pub async fn transition(&self, key: &str, to: &str) -> Result<String, Fail> {
        match self.cfg.kind {
            TrackerKind::Jira => jira::transition(self, key, to).await,
            TrackerKind::Linear => linear::transition(self, key, to).await,
        }
    }

    /// The user that the token acts as.
    pub async fn whoami(&self) -> Result<String, Fail> {
        match self.cfg.kind {
            TrackerKind::Jira => jira::whoami(self).await,
            TrackerKind::Linear => linear::whoami(self).await,
        }
    }

    /// An image of an issue body. The token goes only to the tracker's own
    /// host.
    pub async fn asset(&self, url: &str) -> Result<Asset, Fail> {
        let (header, secrets) = self.auth_header();
        let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
        let (hosts, own) = match self.cfg.kind {
            TrackerKind::Jira => {
                let host = super::util::https_host(self.site()).ok_or_else(|| {
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
