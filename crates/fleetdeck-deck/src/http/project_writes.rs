//! The write verbs of a project and the session starts from tickets and
//! pull requests (R3.4, R3.7, R3.9, DL-164).
//!
//! - `POST /api/projects/{id}/prs`, `.../prs/{n}/comments`,
//!   `.../prs/{n}/reviews`, `.../issues/{key}/comments`,
//!   `.../issues/{key}/transition`: one write for each request. Owner only,
//!   CSRF (cookie login), audited. The body carries `confirm: true`, which
//!   the board sets only after the owner confirmed this write. The deck sends
//!   the write to the station once and never retries or batches it. The
//!   audit row names the action, the target and the result URL; it never
//!   holds the text.
//! - `POST /api/projects/{id}/sessions`: for each issue (or for a pull
//!   request to review) the deck reads the ticket through the station and
//!   starts a session with the ticket text in the injection envelope. The
//!   start is flagged `origin_text`, so the station runs it in the most
//!   restrictive mode of its provider whatever the board asks.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::{
    CommandOrigin, SessionCommand, SessionSpec, UserInput, WorkspaceRequest,
};
use fleetdeck_proto::fleet_git::StartKind;
use fleetdeck_proto::integrations::{
    Binding, CommentBody, DEFAULT_PROMPT, DEFAULT_REVIEW_PROMPT, IssueCommentWrite, IssueDetail,
    IssueGet, IssueRef, IssueTransition, PrComment, PrCreateBody, PrDetail, PrFetchHead, PrHead,
    PrRef, PrReview, Project, ReviewBody, StartFailure, StartSessions, StartedSession,
    StartedSessions, TrackerBinding, TransitionBody, WriteResult, valid_branch,
};
use fleetdeck_proto::untrusted::fence;
use serde_json::{Value, json};

use super::Board;
use super::command_routes::RECEIPT_WAIT;
use super::guard::{ApiError, owner_only};
use super::project_targets::{Target, target_for, targets};
use super::projects_routes::{READ_WAIT, ask, bad_body, forge_of, load, station_of};
use crate::link::CommandKind;
use crate::store::{AuditResult, DeckError, NewAudit, Principal};

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message.into())
}

/// The binding of an issue key: `#12` or `12` is the forge; `PROJ-1` is the
/// tracker whose project key is `PROJ`.
fn binding_of_key(project: &Project, key: &str) -> Result<Binding, ApiError> {
    if key.starts_with('#') || key.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(Binding::Forge(forge_of(project)?));
    }
    let prefix = key.split_once('-').map_or("", |(p, _)| p);
    project
        .trackers
        .iter()
        .find(|t: &&TrackerBinding| t.key.eq_ignore_ascii_case(prefix))
        .cloned()
        .map(Binding::Tracker)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_source",
                format!("this project has no tracker for the key `{key}`"),
            )
        })
}

/// Runs one write of the owner. `build` makes the station method and params
/// from the project.
#[allow(clippy::too_many_arguments)]
async fn write(
    board: &Arc<Board>,
    principal: &Principal,
    action: &'static str,
    id: &str,
    target: String,
    command_id: &str,
    confirm: bool,
    build: impl FnOnce(&Project) -> Result<(&'static str, Value), ApiError>,
) -> Result<Json<WriteResult>, ApiError> {
    let target = format!("{id}/{target}");
    let row = |result| {
        NewAudit::new(principal.actor(), action, result)
            .target(target.clone())
            .command(CommandId::from(command_id.to_owned()))
    };
    let run = async {
        owner_only(principal, "writes to GitHub, GitLab, Jira or Linear")?;
        if !confirm {
            return Err(bad(
                "confirm_required",
                "the owner must confirm this write on the board",
            ));
        }
        let project = load(board, id).await?;
        let (method, params) = build(&project)?;
        let (_, control) = station_of(board, &project).await?;
        ask::<WriteResult>(&control, method, params, READ_WAIT).await
    };
    match run.await {
        Ok(done) => {
            board
                .store
                .audit_or_log(row(AuditResult::Ok).detail(done.url.clone()))
                .await;
            Ok(Json(done))
        }
        Err(e) => {
            // A refusal before the station is not a failed write.
            let result = if matches!(
                e.status,
                StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ) {
                AuditResult::Refused
            } else {
                AuditResult::Failed
            };
            board.store.audit_or_log(row(result).detail(e.code)).await;
            Err(e)
        }
    }
}

fn body_of<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(b)| b).map_err(|e| bad_body(&e))
}

fn params<T: serde::Serialize>(
    method: &'static str,
    p: &T,
    check: Result<(), &'static str>,
) -> Result<(&'static str, Value), ApiError> {
    check.map_err(|why| bad("invalid_params", why))?;
    Ok((method, json!(p)))
}

/// `POST /api/projects/{id}/prs`: creates a pull request or merge request
/// from `head` into `base` in the project's repository.
///
/// # Errors
/// 400 `invalid_body` / `confirm_required` / `invalid_params`; 403
/// `owner_only`; 404 `unknown_project`; 409 `no_binding` /
/// `station_unreachable` / `integration_unavailable`; 502 `upstream`.
pub async fn pr_create(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<PrCreateBody>, JsonRejection>,
) -> Result<Json<WriteResult>, ApiError> {
    let b = body_of(body)?;
    write(
        &board,
        &principal,
        "projects.pr_create",
        &id,
        format!("{} -> {}", b.head, b.base),
        &b.command_id.clone(),
        b.confirm,
        |project| {
            let p = b.params(forge_of(project)?, project.path.clone());
            params("prs.create", &p, p.check())
        },
    )
    .await
}

/// `POST /api/projects/{id}/prs/{n}/comments`.
///
/// # Errors
/// As [`pr_create`].
pub async fn pr_comment(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((id, number)): Path<(String, u64)>,
    body: Result<Json<CommentBody>, JsonRejection>,
) -> Result<Json<WriteResult>, ApiError> {
    let b = body_of(body)?;
    write(
        &board,
        &principal,
        "projects.pr_comment",
        &id,
        format!("pr {number}"),
        &b.command_id.clone(),
        b.confirm,
        |project| {
            let p = PrComment {
                command_id: b.command_id.clone(),
                forge: forge_of(project)?,
                number,
                body: b.body.clone(),
            };
            params("prs.comment", &p, p.check())
        },
    )
    .await
}

/// `POST /api/projects/{id}/prs/{n}/reviews`.
///
/// # Errors
/// As [`pr_create`].
pub async fn pr_review(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((id, number)): Path<(String, u64)>,
    body: Result<Json<ReviewBody>, JsonRejection>,
) -> Result<Json<WriteResult>, ApiError> {
    let b = body_of(body)?;
    write(
        &board,
        &principal,
        "projects.pr_review",
        &id,
        format!("pr {number}"),
        &b.command_id.clone(),
        b.confirm,
        |project| {
            let p = PrReview {
                command_id: b.command_id.clone(),
                forge: forge_of(project)?,
                number,
                event: b.event,
                body: b.body.clone(),
            };
            params("prs.review", &p, p.check())
        },
    )
    .await
}

/// `POST /api/projects/{id}/issues/{key}/comments`.
///
/// # Errors
/// As [`pr_create`]; 404 `unknown_source` for a key that no binding owns.
pub async fn issue_comment(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((id, key)): Path<(String, String)>,
    body: Result<Json<CommentBody>, JsonRejection>,
) -> Result<Json<WriteResult>, ApiError> {
    let b = body_of(body)?;
    write(
        &board,
        &principal,
        "projects.issue_comment",
        &id,
        format!("issue {key}"),
        &b.command_id.clone(),
        b.confirm,
        |project| {
            let p = IssueCommentWrite {
                command_id: b.command_id.clone(),
                binding: binding_of_key(project, &key)?,
                key: key.clone(),
                body: b.body.clone(),
            };
            params("issues.comment", &p, p.check())
        },
    )
    .await
}

/// `POST /api/projects/{id}/issues/{key}/transition`.
///
/// # Errors
/// As [`issue_comment`].
pub async fn issue_transition(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((id, key)): Path<(String, String)>,
    body: Result<Json<TransitionBody>, JsonRejection>,
) -> Result<Json<WriteResult>, ApiError> {
    let b = body_of(body)?;
    write(
        &board,
        &principal,
        "projects.issue_transition",
        &id,
        format!("issue {key} -> {}", b.to),
        &b.command_id.clone(),
        b.confirm,
        |project| {
            let p = IssueTransition {
                command_id: b.command_id.clone(),
                binding: binding_of_key(project, &key)?,
                key: key.clone(),
                to: b.to.clone(),
            };
            params("issues.transition", &p, p.check())
        },
    )
    .await
}

// ----- Sessions from tickets and pull requests -----

/// The longest branch slug, in characters.
const SLUG_CHARS: usize = 40;
/// The longest session title, in characters (the station's limit).
const TITLE_CHARS: usize = 80;
/// The most lines of the changed-file list in a review prompt.
const REVIEW_FILES: usize = 100;

/// A lower-case slug of `title` for a branch: ASCII letters and digits,
/// single dashes, at most 40 characters, no dash at either end.
#[must_use]
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let cut: String = out.trim_matches('-').chars().take(SLUG_CHARS).collect();
    cut.trim_matches('-').to_owned()
}

/// `<KEY>-<slug>` for a tracker issue and `issue-<n>-<slug>` for a forge
/// issue. The `n`-th session of a batch for one issue (from 2) gets `-<n>`.
#[must_use]
pub fn branch_for(key: &str, title: &str, nth: u32) -> String {
    let base = match key.strip_prefix('#') {
        Some(n) => format!("issue-{n}"),
        None if key.bytes().all(|b| b.is_ascii_digit()) => format!("issue-{key}"),
        None => key.to_owned(),
    };
    let slug = slug(title);
    let mut branch = if slug.is_empty() {
        base
    } else {
        format!("{base}-{slug}")
    };
    if nth > 1 {
        branch.push_str(&format!("-{nth}"));
    }
    branch
}

/// `text` on one line, cut to `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The issue link of a session: the station's reference with a title that
/// passes the station's checks.
fn issue_ref(detail: &IssueDetail) -> IssueRef {
    let r = &detail.summary.r#ref;
    let title = one_line(&r.title, 300);
    IssueRef {
        title: if title.is_empty() {
            r.key.clone()
        } else {
            title
        },
        ..r.clone()
    }
}

fn start(
    project: &Project,
    req: &StartSessions,
    title: String,
    prompt: String,
    branch: String,
    base: Option<String>,
    issue: Option<IssueRef>,
) -> SessionCommand {
    SessionCommand::Start {
        spec: SessionSpec {
            provider_instance: req.provider_instance.clone(),
            cwd: project.path.clone(),
            mode: req.mode.clone(),
            model: req.model.clone(),
            title: Some(title),
            coordinator: None,
            launch: None,
            extra_env: None,
        },
        prompt: Some(UserInput::new(prompt)),
        workspace: Some(Box::new(WorkspaceRequest::Worktree {
            branch: Some(branch),
            base,
        })),
        setup_cmd: None,
        issue: issue.map(Box::new),
        origin_text: true,
        fleet_git: None,
        batch: None,
    }
}

/// The starts for one issue: `count` sessions on branches `<KEY>-<slug>`,
/// `...-2`, ...
fn issue_starts(
    project: &Project,
    req: &StartSessions,
    detail: &IssueDetail,
) -> Vec<SessionCommand> {
    let template = project.prompt_template.as_deref().unwrap_or(DEFAULT_PROMPT);
    let r = issue_ref(detail);
    let title = one_line(&format!("{} {}", r.key, r.title), TITLE_CHARS);
    (1..=req.count_per_issue)
        .map(|nth| {
            start(
                project,
                req,
                title.clone(),
                format!("{template}\n\n{}", detail.body_markdown),
                branch_for(&r.key, &r.title, nth),
                None,
                Some(r.clone()),
            )
        })
        .collect()
}

/// The branch of a review: `review/pr-<n>-<oid7>-<b4>`. `oid7` is the first 7
/// characters of the fetched head and `b4` the last 4 characters of the batch
/// id in lower case, so every review is a new branch on the head of the pull
/// request.
pub fn review_branch(number: u64, head: &PrHead, batch_id: &str) -> String {
    let oid7: String = head.oid.chars().take(7).collect();
    let skip = batch_id.chars().count().saturating_sub(4);
    let b4: String = batch_id
        .chars()
        .skip(skip)
        .collect::<String>()
        .to_lowercase();
    format!("review/pr-{number}-{oid7}-{b4}")
}

/// The start of a review: a new worktree branch on the fetched head of the
/// pull request (R3.7), the description and the changed files in the
/// envelope.
fn review_start(
    project: &Project,
    req: &StartSessions,
    pr: &PrDetail,
    head: &PrHead,
) -> SessionCommand {
    let prompt = project
        .review_prompt
        .as_deref()
        .unwrap_or(DEFAULT_REVIEW_PROMPT);
    let s = &pr.summary;
    let mut files: Vec<String> = pr
        .files
        .iter()
        .take(REVIEW_FILES)
        .map(|f| format!("{} (+{} -{})", f.path, f.additions, f.deletions))
        .collect();
    if pr.files.len() > REVIEW_FILES {
        files.push(format!(
            "... and {} more files",
            pr.files.len() - REVIEW_FILES
        ));
    }
    let listing = fence(&format!("pr:#{} files", s.number), &files.join("\n"));
    let title = one_line(&format!("Review #{} {}", s.number, s.title), TITLE_CHARS);
    let branch = review_branch(s.number, head, &req.batch_id);
    start(
        project,
        req,
        title,
        format!(
            "{prompt}\n\n{}\n\nChanged files:\n{listing}",
            pr.body_markdown
        ),
        branch,
        Some(head.ref_name.clone()),
        None,
    )
}

fn failure(key: &str, e: &ApiError) -> StartFailure {
    StartFailure {
        key: key.to_owned(),
        code: e.code.to_owned(),
        message: e.message.clone(),
    }
}

/// `POST /api/projects/{id}/sessions` (R3.4, R3.7): starts the sessions of
/// a batch. The answer holds the receipts of the starts and what could not
/// start.
///
/// # Errors
/// 400 `invalid_body` / `invalid_params`; 404 `unknown_project`; 409
/// `station_unreachable`.
pub async fn sessions(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<StartSessions>, JsonRejection>,
) -> Result<Json<StartedSessions>, ApiError> {
    let req = body_of(body)?;
    req.check().map_err(|why| bad("invalid_params", why))?;
    let project = load(&board, &id).await?;
    // The targets of the batch (FG-5, decisions B8, B16; FG-6, D4): the chosen
    // home, or with `home = "auto"` one placed station for each start. The
    // reads and the review fetch go to the first target.
    let targets = targets(&board, &project, &req).await?;
    let Target {
        rec,
        control,
        path: first_path,
    } = target_for(&targets, 0).clone();
    let mut commands: Vec<(usize, String, SessionCommand)> = Vec::new();
    let mut failed = Vec::new();
    let Some(cwd) = first_path else {
        // The home still clones, or failed: every start fails with the
        // refusal of the home.
        let keys = match req.review_of_pr {
            Some(number) => vec![format!("#{number}")],
            None => req.issues.clone(),
        };
        let refusal = super::project_git_routes::attach_start(
            &board,
            &rec,
            "",
            StartKind::Worktree,
            Some(project.project_id.clone()),
        )
        .await
        .err()
        .unwrap_or_else(|| bad("not_a_home", "This folder is not a home of the project."));
        let failed = keys.iter().map(|key| failure(key, &refusal)).collect();
        return Ok(Json(StartedSessions {
            started: Vec::new(),
            failed,
        }));
    };
    if let Some(number) = req.review_of_pr {
        let key = format!("#{number}");
        let pr = PrRef {
            forge: forge_of(&project)?,
            number,
        };
        match ask::<PrDetail>(&control, "prs.get", json!(pr), READ_WAIT).await {
            Ok(detail) if valid_branch(&detail.summary.head_branch) => {
                let fetch = PrFetchHead {
                    forge: pr.forge.clone(),
                    number,
                    path: cwd.clone(),
                };
                match ask::<PrHead>(&control, "prs.fetch_head", json!(fetch), READ_WAIT).await {
                    Ok(head) => {
                        commands.push((0, key, review_start(&project, &req, &detail, &head)));
                    }
                    Err(e) => failed.push(StartFailure {
                        key,
                        code: "pr_fetch_failed".to_owned(),
                        message: e.message,
                    }),
                }
            }
            Ok(_) => failed.push(StartFailure {
                key,
                code: "invalid_branch".to_owned(),
                message: "the head branch of the pull request is not a valid branch name"
                    .to_owned(),
            }),
            Err(e) => failed.push(failure(&key, &e)),
        }
    } else {
        for (at, key) in req.issues.iter().enumerate() {
            let get = binding_of_key(&project, key).map(|binding| IssueGet {
                binding,
                key: key.clone(),
            });
            let detail = match get {
                Ok(get) => match get.check() {
                    Ok(()) => {
                        ask::<IssueDetail>(&control, "issues.get", json!(get), READ_WAIT).await
                    }
                    Err(why) => Err(bad("invalid_params", why)),
                },
                Err(e) => Err(e),
            };
            match detail {
                Ok(detail) => {
                    // The index in the request order, failed reads included.
                    let first = at * req.count_per_issue as usize;
                    for (nth, command) in issue_starts(&project, &req, &detail)
                        .into_iter()
                        .enumerate()
                    {
                        commands.push((first + nth, key.clone(), command));
                    }
                }
                Err(e) => failed.push(failure(key, &e)),
            }
        }
    }

    // Fleet Git (design §8.5): every start of the batch is a worktree start in
    // the chosen home of this project, and its `cwd` is the path of that
    // home. A home that cannot start now (an unverified home, a taken Git
    // name) fails every start with the same 409; a sync-off project with a
    // ready home starts as before (O1).
    let mut attached = Vec::new();
    for (n, (at, key, mut command)) in commands.into_iter().enumerate() {
        // The station of this start (the same for every start without `auto`).
        let target = target_for(&targets, at);
        let Some(cwd) = target.path.clone() else {
            failed.push(failure(
                &key,
                &bad("not_a_home", "This folder is not a home of the project."),
            ));
            continue;
        };
        if let SessionCommand::Start {
            spec, fleet_git, ..
        } = &mut command
        {
            spec.cwd.clone_from(&cwd);
            match super::project_git_routes::attach_start(
                &board,
                &target.rec,
                &cwd,
                StartKind::Worktree,
                Some(project.project_id.clone()),
            )
            .await
            {
                Ok(start) => *fleet_git = start,
                Err(e) => {
                    failed.push(failure(&key, &e));
                    continue;
                }
            }
        }
        attached.push((n, at, key, command));
    }
    // Submit every start first, then wait for the receipts.
    let mut submitted = Vec::new();
    for (n, at, key, command) in attached {
        let command_id = CommandId::from(format!("{}-{}", req.batch_id, n + 1));
        match board
            .link
            .submit_as(
                target_for(&targets, at).rec.station_id.clone(),
                CommandOrigin::Board,
                CommandKind::Session { command },
                command_id,
            )
            .await
        {
            Ok((cmd, _)) => submitted.push((key, cmd)),
            Err(e) => {
                let api = match &e {
                    DeckError::NotSynced(_) => {
                        ApiError::new(StatusCode::CONFLICT, "not_synced", e.to_string())
                    }
                    DeckError::CommandIdReused(_) => {
                        ApiError::new(StatusCode::CONFLICT, "command_id_reused", e.to_string())
                    }
                    _ => ApiError::internal(&e),
                };
                failed.push(failure(&key, &api));
            }
        }
    }
    let mut started = Vec::new();
    for (key, cmd) in submitted {
        let command = board
            .link
            .wait_command(cmd.command_id, RECEIPT_WAIT)
            .await
            .map_err(ApiError::internal)?;
        started.push(StartedSession { key, command });
    }
    board
        .store
        .audit_or_log(
            NewAudit::new(principal.actor(), "projects.sessions", AuditResult::Ok)
                .target(id)
                .detail(format!(
                    "{} started, {} failed, batch {}",
                    started.len(),
                    failed.len(),
                    req.batch_id
                )),
        )
        .await;
    Ok(Json(StartedSessions { started, failed }))
}

#[cfg(test)]
#[path = "project_writes_tests.rs"]
mod tests;
