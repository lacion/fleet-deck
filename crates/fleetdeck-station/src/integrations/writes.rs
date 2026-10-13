//! The write verbs of R3.9: `prs.create`, `prs.comment`, `prs.review`,
//! `issues.comment` and `issues.transition`.
//!
//! Each write has a `command_id`. The station keeps the answers of the last
//! 500 writes for one hour: a repeated id with the same request returns the
//! stored answer and writes nothing; the same id with another request is
//! refused. One write runs at a time. A write records one `tracing` line:
//! action, target, actor and result URL; never the text.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fleetdeck_proto::integrations::{
    Binding, IssueCommentWrite, IssueTransition, PrComment, PrCreate, PrReview, WriteResult, code,
};
use serde_json::Value;

use super::exec::Fail;
use super::{Integrations, check_key, invalid, parse, to_value};

/// How many write answers the station keeps.
pub const LOG_MAX: usize = 500;
/// How long it keeps them.
pub const LOG_TTL: Duration = Duration::from_secs(3600);

/// The recent writes.
#[derive(Debug, Default)]
pub struct Log {
    entries: VecDeque<Entry>,
}

#[derive(Debug)]
struct Entry {
    id: String,
    method: String,
    request: String,
    at: Instant,
    result: WriteResult,
}

impl Log {
    fn find(&mut self, id: &str) -> Option<&Entry> {
        self.entries.retain(|e| e.at.elapsed() < LOG_TTL);
        self.entries.iter().find(|e| e.id == id)
    }

    fn put(&mut self, entry: Entry) {
        if self.entries.len() >= LOG_MAX {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }
}

/// What a write needs once its params are checked.
enum Action {
    PrCreate(PrCreate),
    PrComment(PrComment),
    PrReview(PrReview),
    IssueComment(IssueCommentWrite),
    IssueTransition(IssueTransition),
}

impl Action {
    fn parse(method: &str, params: &Value) -> Result<(Self, String), Fail> {
        let checked = |r: Result<(), &'static str>| r.map_err(invalid);
        Ok(match method {
            "prs.create" => {
                let p: PrCreate = parse(params)?;
                checked(p.check())?;
                (Self::PrCreate(p.clone()), p.command_id)
            }
            "prs.comment" => {
                let p: PrComment = parse(params)?;
                checked(p.check())?;
                (Self::PrComment(p.clone()), p.command_id)
            }
            "prs.review" => {
                let p: PrReview = parse(params)?;
                checked(p.check())?;
                (Self::PrReview(p.clone()), p.command_id)
            }
            "issues.comment" => {
                let p: IssueCommentWrite = parse(params)?;
                checked(p.check())?;
                check_key(&p.binding, &p.key)?;
                (Self::IssueComment(p.clone()), p.command_id)
            }
            "issues.transition" => {
                let p: IssueTransition = parse(params)?;
                checked(p.check())?;
                check_key(&p.binding, &p.key)?;
                (Self::IssueTransition(p.clone()), p.command_id)
            }
            other => {
                return Err(Fail::new(
                    code::INVALID_PARAMS,
                    format!("`{other}` is not a write method"),
                ));
            }
        })
    }

    /// The object of the write, for the log line.
    fn target(&self) -> String {
        let issue = |b: &Binding, key: &str| match b {
            Binding::Tracker(t) => format!("{}:{key}", t.tracker),
            Binding::Forge(f) => format!("{}/{}{key}", f.host, f.repo),
        };
        match self {
            Self::PrCreate(p) => format!("{}/{} {}", p.forge.host, p.forge.repo, p.head),
            Self::PrComment(p) => format!("{}/{}!{}", p.forge.host, p.forge.repo, p.number),
            Self::PrReview(p) => format!("{}/{}!{}", p.forge.host, p.forge.repo, p.number),
            Self::IssueComment(p) => issue(&p.binding, &p.key),
            Self::IssueTransition(p) => issue(&p.binding, &p.key),
        }
    }
}

impl Integrations {
    pub(super) async fn write(
        &self,
        method: &str,
        params: Value,
        resolve: &(dyn Fn(&str) -> Option<PathBuf> + Sync),
    ) -> Result<Value, Fail> {
        let (action, id) = Action::parse(method, &params)?;
        let request = params.to_string();
        let mut log = self.writes.lock().await;
        if let Some(prev) = log.find(&id) {
            if prev.method == method && prev.request == request {
                return to_value(&prev.result);
            }
            return Err(Fail::new(
                code::INVALID_PARAMS,
                "this command id was used for another request",
            ));
        }
        let target = action.target();
        let result = self.perform(action, resolve).await?;
        tracing::info!(
            method,
            command_id = %id,
            target = %target,
            actor = result.actor.as_deref().unwrap_or("-"),
            url = %result.url,
            "integration write"
        );
        log.put(Entry {
            id,
            method: method.to_owned(),
            request,
            at: Instant::now(),
            result: result.clone(),
        });
        drop(log);
        self.clear_cache();
        to_value(&result)
    }

    async fn perform(
        &self,
        action: Action,
        resolve: &(dyn Fn(&str) -> Option<PathBuf> + Sync),
    ) -> Result<WriteResult, Fail> {
        match action {
            Action::PrCreate(p) => {
                let cwd = resolve(&p.path).ok_or_else(|| {
                    Fail::new(
                        code::OUTSIDE_ROOTS,
                        "the path is outside the allowed roots of this station",
                    )
                })?;
                // A repository with one remote must be the bound repository.
                if let Some((_, Some(origin))) = fleetdeck_core::git::repo_of(&cwd) {
                    let wanted = format!("{}/{}", p.forge.host, p.forge.repo).to_lowercase();
                    if origin != wanted {
                        return Err(Fail::new(
                            code::INVALID_PARAMS,
                            format!(
                                "the repository at the path has the origin `{origin}`, not `{wanted}`"
                            ),
                        ));
                    }
                }
                let forge = self.forge(&p.forge);
                let actor = forge.viewer().await?;
                let url = forge
                    .pr_create(cwd, &p.head, &p.base, &p.title, &p.body, p.draft)
                    .await?;
                Ok(WriteResult {
                    url,
                    actor: Some(actor),
                })
            }
            Action::PrComment(p) => {
                let forge = self.forge(&p.forge);
                let actor = forge.viewer().await?;
                let url = forge.comment(true, p.number, &p.body).await?;
                Ok(WriteResult {
                    url,
                    actor: Some(actor),
                })
            }
            Action::PrReview(p) => {
                let forge = self.forge(&p.forge);
                let actor = forge.viewer().await?;
                let url = forge.review(p.number, p.event, &p.body).await?;
                Ok(WriteResult {
                    url,
                    actor: Some(actor),
                })
            }
            Action::IssueComment(p) => match &p.binding {
                Binding::Tracker(t) => {
                    let tracker = self.tracker(t)?;
                    let actor = tracker.whoami().await?;
                    let url = tracker.comment(&p.key, &p.body).await?;
                    Ok(WriteResult {
                        url,
                        actor: Some(actor),
                    })
                }
                Binding::Forge(f) => {
                    let number = fleetdeck_proto::integrations::issue_number(&p.key)
                        .ok_or_else(|| invalid("the issue key is not valid"))?;
                    let forge = self.forge(f);
                    let actor = forge.viewer().await?;
                    let url = forge.comment(false, number, &p.body).await?;
                    Ok(WriteResult {
                        url,
                        actor: Some(actor),
                    })
                }
            },
            Action::IssueTransition(p) => match &p.binding {
                Binding::Tracker(t) => {
                    let tracker = self.tracker(t)?;
                    let actor = tracker.whoami().await?;
                    let url = tracker.transition(&p.key, &p.to).await?;
                    Ok(WriteResult {
                        url,
                        actor: Some(actor),
                    })
                }
                Binding::Forge(f) => {
                    let number = fleetdeck_proto::integrations::issue_number(&p.key)
                        .ok_or_else(|| invalid("the issue key is not valid"))?;
                    super::forge::open_state(&p.to)?;
                    let forge = self.forge(f);
                    let actor = forge.viewer().await?;
                    let url = forge.transition(number, &p.to).await?;
                    Ok(WriteResult {
                        url,
                        actor: Some(actor),
                    })
                }
            },
        }
    }
}
