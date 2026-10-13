//! The station integration service (R3, DL-164): GitHub, GitLab, Jira and
//! Linear for the board's issue and pull request views.
//!
//! - The station does all forge and tracker I/O with its own credentials:
//!   `gh api` and `glab api` for the forges, `curl` with a stdin config for
//!   Jira Cloud and Linear ([`exec`], [`http`]). The deck relays answers and
//!   holds no token.
//! - Every third-party text that a session may read (issue and pull request
//!   bodies, comments, CI log tails) is returned inside the injection
//!   envelope of [`fleetdeck_proto::untrusted::fence`].
//! - A list call has a response cache of 60 s. A write clears it.
//! - A write carries a `command_id`: the station keeps the last 500 answers
//!   for one hour in memory, so a repeated id returns the stored answer and
//!   writes nothing (DL-164). A write records one `tracing` line with the
//!   action, the target and the result URL, never the text.

pub mod adf;
pub mod asset;
pub mod exec;
pub mod forge;
pub mod github;
pub mod gitlab;
pub mod http;
pub mod jira;
pub mod linear;
pub mod tracker;
pub mod util;

mod writes;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fleetdeck_core::config::{TokenSource, TrackerConfig};
use fleetdeck_proto::integrations::{
    Asset, AssetQuery, Binding, CliStatus, ForgeBinding, ForgeKind, IntegrationsStatus, IssueGet,
    IssueList, IssueSearch, PrRef, PrsList, StatusQuery, TrackerBinding, TrackerStatus, code,
};
use fleetdeck_proto::link::{Outcome, WireError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use exec::{Fail, Programs};
use forge::Forge;
use tracker::Tracker;

/// How long a list answer stays in the cache.
pub const CACHE_TTL: Duration = Duration::from_secs(60);
/// The most cached answers.
const CACHE_MAX: usize = 64;

/// The link methods of this service, with whether they write.
pub const METHODS: &[(&str, bool)] = &[
    ("integrations.status", false),
    ("issues.search", false),
    ("issues.get", false),
    ("issues.asset", false),
    ("prs.list", false),
    ("prs.get", false),
    ("prs.checks", false),
    ("prs.create", true),
    ("prs.comment", true),
    ("prs.review", true),
    ("issues.comment", true),
    ("issues.transition", true),
];

/// The service of one station.
#[derive(Debug)]
pub struct Integrations {
    programs: Programs,
    trackers: Vec<TrackerConfig>,
    /// The variables that name a tracker token: no child gets them.
    hidden: Vec<String>,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
    writes: tokio::sync::Mutex<writes::Log>,
}

impl Default for Integrations {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Integrations {
    /// The service for the `[[trackers]]` entries of the station config.
    #[must_use]
    pub fn new(trackers: Vec<TrackerConfig>) -> Self {
        Self::with_programs(trackers, Programs::default())
    }

    /// [`Integrations::new`] with other helper programs (tests).
    #[must_use]
    pub fn with_programs(trackers: Vec<TrackerConfig>, programs: Programs) -> Self {
        let hidden = trackers
            .iter()
            .filter_map(|t| match &t.token {
                TokenSource::Env(name) => Some(name.clone()),
                TokenSource::File(_) => None,
            })
            .collect();
        Self {
            programs,
            trackers,
            hidden,
            cache: Mutex::new(HashMap::new()),
            writes: tokio::sync::Mutex::new(writes::Log::default()),
        }
    }

    /// True for a method of this service.
    #[must_use]
    pub fn serves(method: &str) -> bool {
        METHODS.iter().any(|(m, _)| *m == method)
    }

    /// Answers one request. `resolve` turns a station path into its
    /// canonical form when it is inside the allowed roots.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        resolve: &(dyn Fn(&str) -> Option<PathBuf> + Sync),
    ) -> Outcome {
        match self.dispatch(method, params, resolve).await {
            Ok(value) => Outcome::Ok(value),
            Err(fail) => Outcome::Err(WireError::new(fail.code, fail.message)),
        }
    }

    async fn dispatch(
        &self,
        method: &str,
        params: Value,
        resolve: &(dyn Fn(&str) -> Option<PathBuf> + Sync),
    ) -> Result<Value, Fail> {
        match method {
            "integrations.status" => {
                parse::<StatusQuery>(&params)?;
                to_value(&self.status().await)
            }
            "issues.search" => {
                let q: IssueSearch = parse(&params)?;
                q.check().map_err(invalid)?;
                self.cached(method, &params, async {
                    self.search(&q).await.map(|issues| IssueList { issues })
                })
                .await
            }
            "issues.get" => {
                let q: IssueGet = parse(&params)?;
                q.check().map_err(invalid)?;
                check_key(&q.binding, &q.key)?;
                to_value(&self.get(&q).await?)
            }
            "issues.asset" => {
                let q: AssetQuery = parse(&params)?;
                q.check().map_err(invalid)?;
                to_value(&self.asset(&q).await?)
            }
            "prs.list" => {
                let q: PrsList = parse(&params)?;
                q.forge.check().map_err(invalid)?;
                self.cached(method, &params, async {
                    self.forge(&q.forge)
                        .prs_list()
                        .await
                        .map(|groups| fleetdeck_proto::integrations::PrList { groups })
                })
                .await
            }
            "prs.get" => {
                let q: PrRef = parse(&params)?;
                q.check().map_err(invalid)?;
                to_value(&self.forge(&q.forge).pr_get(q.number).await?)
            }
            "prs.checks" => {
                let q: PrRef = parse(&params)?;
                q.check().map_err(invalid)?;
                to_value(&self.forge(&q.forge).pr_checks(q.number).await?)
            }
            _ => self.write(method, params, resolve).await,
        }
    }

    pub(crate) fn forge<'a>(&'a self, binding: &'a ForgeBinding) -> Forge<'a> {
        Forge {
            programs: &self.programs,
            hidden: &self.hidden,
            binding,
        }
    }

    /// The `[[trackers]]` entry of `binding`.
    pub(crate) fn tracker<'a>(&'a self, binding: &'a TrackerBinding) -> Result<Tracker<'a>, Fail> {
        let cfg = self
            .trackers
            .iter()
            .find(|t| t.id == binding.tracker)
            .ok_or_else(|| {
                Fail::unavailable(format!(
                    "this station has no tracker `{}` in its station.toml",
                    binding.tracker
                ))
            })?;
        if cfg.kind != binding.kind {
            return Err(Fail::new(
                code::INVALID_PARAMS,
                format!("tracker `{}` is not of the kind of the binding", cfg.id),
            ));
        }
        Tracker::open(&self.programs, cfg, &binding.key)
    }

    /// A cached list answer: a hit inside [`CACHE_TTL`] gives the stored
    /// value; a miss runs `produce` and stores its answer.
    async fn cached<T: Serialize>(
        &self,
        method: &str,
        params: &Value,
        produce: impl std::future::Future<Output = Result<T, Fail>>,
    ) -> Result<Value, Fail> {
        let key = format!("{method} {params}");
        {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((at, value)) = cache.get(&key)
                && at.elapsed() < CACHE_TTL
            {
                return Ok(value.clone());
            }
        }
        let value = to_value(&produce.await?)?;
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        if cache.len() >= CACHE_MAX
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(key, (Instant::now(), value.clone()));
        Ok(value)
    }

    pub(crate) fn clear_cache(&self) {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    async fn search(
        &self,
        q: &IssueSearch,
    ) -> Result<Vec<fleetdeck_proto::integrations::IssueSummary>, Fail> {
        match &q.binding {
            Binding::Tracker(t) => self.tracker(t)?.search(q).await,
            Binding::Forge(f) => self.forge(f).issues_search(q).await,
        }
    }

    async fn get(&self, q: &IssueGet) -> Result<fleetdeck_proto::integrations::IssueDetail, Fail> {
        match &q.binding {
            Binding::Tracker(t) => self.tracker(t)?.get(&q.key).await,
            Binding::Forge(f) => {
                let number = fleetdeck_proto::integrations::issue_number(&q.key)
                    .ok_or_else(|| invalid("the issue key is not valid"))?;
                self.forge(f).issue_get(number).await
            }
        }
    }

    async fn asset(&self, q: &AssetQuery) -> Result<Asset, Fail> {
        match &q.binding {
            Binding::Tracker(t) => self.tracker(t)?.asset(&q.url).await,
            Binding::Forge(f) => self.forge(f).asset(&q.url).await,
        }
    }

    /// The `[[trackers]]` entries and the CLIs, each with its credential
    /// check.
    pub async fn status(&self) -> IntegrationsStatus {
        let mut trackers = Vec::new();
        for cfg in &self.trackers {
            let binding = TrackerBinding {
                tracker: cfg.id.clone(),
                kind: cfg.kind,
                key: "X".to_owned(),
            };
            let checked = match self.tracker(&binding) {
                Ok(t) => t.whoami().await,
                Err(e) => Err(e),
            };
            let (ok, user, message) = match checked {
                Ok(user) => (true, Some(user), String::new()),
                Err(e) => (false, None, e.message),
            };
            trackers.push(TrackerStatus {
                id: cfg.id.clone(),
                kind: cfg.kind,
                site: cfg.site.clone(),
                ok,
                user,
                message,
            });
        }
        let (gh, glab) = tokio::join!(
            self.cli_status(ForgeKind::Github),
            self.cli_status(ForgeKind::Gitlab)
        );
        IntegrationsStatus { trackers, gh, glab }
    }

    async fn cli_status(&self, kind: ForgeKind) -> CliStatus {
        let binding = ForgeBinding {
            kind,
            host: match kind {
                ForgeKind::Github => "github.com".to_owned(),
                ForgeKind::Gitlab => "gitlab.com".to_owned(),
            },
            repo: "x/x".to_owned(),
        };
        match self.forge(&binding).viewer().await {
            Ok(user) => CliStatus {
                present: true,
                authenticated: true,
                user: Some(user),
                message: String::new(),
            },
            Err(e) => CliStatus {
                present: e.code != code::UNAVAILABLE,
                authenticated: false,
                user: None,
                message: e.message,
            },
        }
    }
}

fn invalid(why: &'static str) -> Fail {
    Fail::new(code::INVALID_PARAMS, why)
}

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, Fail> {
    serde_json::from_value(params.clone())
        .map_err(|e| Fail::new(code::INVALID_PARAMS, format!("params: {e}")))
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, Fail> {
    serde_json::to_value(value).map_err(|e| Fail::bad_answer(e.to_string()))
}

/// A tracker issue key must belong to the project of its binding.
pub(crate) fn check_key(binding: &Binding, key: &str) -> Result<(), Fail> {
    if let Binding::Tracker(t) = binding {
        let prefix = key.split_once('-').map_or("", |(p, _)| p);
        if !prefix.eq_ignore_ascii_case(&t.key) {
            return Err(invalid(
                "the issue key is not in the project of the binding",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
