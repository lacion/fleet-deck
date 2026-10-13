//! Requests of a deck link (ADR-004 §9.3, §9.5, §9.6). The control channel
//! already checked the method, the roles, the feature, the scope (from the
//! grant cache) and the `command_id`. Here:
//!
//! - a mutating request is parsed into its typed envelope; a malformed
//!   envelope, a session command without `target_incarnation` (required
//!   over a link) and a refused rotation get a rejected receipt that is not
//!   stored;
//! - the decision transaction re-checks the caller's pin and grant
//!   (`Caller::peer`, G5) and applies the G3 receipt rules;
//! - the response to a mutating request is the `Receipt`.

use fleetdeck_core::identity::PeerKey;
use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_link::methods::code;
use fleetdeck_link::revoke::{RotateError, check_rotation, finish_rotation};
use fleetdeck_proto::assist::AssistRequest;
use fleetdeck_proto::command::{CommandResult, ReceiptState};
use fleetdeck_proto::link::{
    Outcome, PasteImage, PrecheckQuery, PreflightQuery, ReceiptQuery, StationInfo, WireError,
    WorkspaceQuery, WorktreesQuery,
};
use fleetdeck_proto::mutation::Mutation;
use fleetdeck_proto::{Receipt, Reject, RejectCode};
use serde::Serialize;

use super::StationLink;
use crate::store::{Caller, DecideError, StoreError, peers};

/// The error code of a request that this build does not serve yet.
pub const NOT_IMPLEMENTED: &str = "not_implemented";

fn ok(value: impl Serialize) -> Outcome {
    match serde_json::to_value(value) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => internal(e),
    }
}

fn internal(e: impl std::fmt::Display) -> Outcome {
    Outcome::Err(WireError::new("internal", e.to_string()))
}

fn bad_params(e: impl std::fmt::Display) -> Outcome {
    Outcome::Err(WireError::new(code::PROTOCOL_ERROR, format!("params: {e}")))
}

/// The longest a `workspace.list` may read the file system.
pub const WORKSPACE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `workspace.list` (M2 P4-4, DL-061): error codes `missing_workspace`,
/// `outside_roots` and `timeout`.
async fn workspaces(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    use crate::manager::policy::WorkspaceError;
    let query: WorkspaceQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let policy = ctx.manager.policy();
    let task =
        tokio::task::spawn_blocking(move || crate::manager::workspaces::list(&policy, &query));
    match tokio::time::timeout(WORKSPACE_TIMEOUT, task).await {
        Ok(Ok(Ok(listing))) => ok(listing),
        Ok(Ok(Err(e @ WorkspaceError::Missing(_)))) => {
            Outcome::Err(WireError::new("missing_workspace", e.to_string()))
        }
        Ok(Ok(Err(e @ WorkspaceError::Outside(_)))) => {
            Outcome::Err(WireError::new("outside_roots", e.to_string()))
        }
        Ok(Err(e)) => internal(e),
        Err(_) => Outcome::Err(WireError::new(
            "timeout",
            "the directory took too long to read",
        )),
    }
}

/// `workspace.worktrees` (DL-142): the read-only inventory of the session
/// worktrees. It runs no Git command that writes.
async fn worktrees(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    use crate::workspace::worktrees;
    if let Err(e) = serde_json::from_value::<WorktreesQuery>(params) {
        return bad_params(e);
    }
    match worktrees::inventory(
        ctx.manager.store(),
        &ctx.manager.policy(),
        &worktrees::git_options(),
    )
    .await
    {
        Ok(list) => ok(list),
        Err(e) => internal(e),
    }
}

/// `files.write_temp` (DL-147): stores a pasted image in the paste
/// directory. Error codes `unsupported` (no paste directory), `invalid_params`,
/// `too_large`, `conflict`, `no_space` and `internal`.
async fn write_temp(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    use crate::workspace::paste;
    let query: PasteImage = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let Some(dir) = ctx.paste_dir.clone() else {
        return Outcome::Err(WireError::new(
            "unsupported",
            "this station has no paste directory",
        ));
    };
    let _guard = paste::LOCK.lock().await;
    let task = tokio::task::spawn_blocking(move || {
        paste::write(&dir, &query, std::time::SystemTime::now())
    });
    match task.await {
        Ok(Ok(answer)) => ok(answer),
        Ok(Err(e)) => Outcome::Err(WireError::new(e.code, e.message)),
        Err(e) => internal(e),
    }
}

/// `workspace.preflight` (DL-144): the Git access check of a clone URL. A
/// failed check is an answer with `ok: false`. Error codes `invalid_url`,
/// `forbidden` (the URL rules of a clone) and `internal`.
async fn preflight(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    use crate::workspace::GitOptions;
    use crate::workspace::preflight::{self, PreflightError};
    let query: PreflightQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let policy = ctx.manager.policy();
    let inside = |p: &std::path::Path| policy.workspace(&p.to_string_lossy()).ok();
    match preflight::run(&query.url, inside, &GitOptions::isolated().program).await {
        Ok(answer) => ok(answer),
        Err(PreflightError::Refused(r)) => Outcome::Err(WireError::new(
            if r.forbidden {
                "forbidden"
            } else {
                "invalid_url"
            },
            r.message,
        )),
        Err(PreflightError::Spawn(e)) => internal(e),
    }
}

/// `workspace.precheck` (R5, DL-168): the shell precheck of a schedule. It
/// runs the owner's command, so it needs `allow_setup_cmd`. A command that
/// exits non-zero is an answer. Error codes `unsupported`, `invalid_params`,
/// `missing_workspace`, `outside_roots` and `internal`.
async fn precheck(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    use crate::manager::policy::WorkspaceError;
    use crate::workspace::precheck::{self, PrecheckError};
    let query: PrecheckQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let policy = ctx.manager.policy();
    if !policy.allow_setup_cmd() {
        return Outcome::Err(WireError::new(
            "unsupported",
            "setup commands are off on this station (allow_setup_cmd = false)",
        ));
    }
    if let Err(PrecheckError::Invalid(m) | PrecheckError::Run(m)) = precheck::check_cmd(&query.cmd)
    {
        return Outcome::Err(WireError::new("invalid_params", m));
    }
    let cwd = match policy.workspace(&query.cwd) {
        Ok(p) => p,
        Err(e @ WorkspaceError::Missing(_)) => {
            return Outcome::Err(WireError::new("missing_workspace", e.to_string()));
        }
        Err(e @ WorkspaceError::Outside(_)) => {
            return Outcome::Err(WireError::new("outside_roots", e.to_string()));
        }
    };
    let env = fleetdeck_core::ChildEnv::from_process();
    match precheck::run(&query.cmd, &cwd, &env, precheck::TIMEOUT).await {
        Ok(answer) => ok(answer),
        Err(PrecheckError::Invalid(m)) => Outcome::Err(WireError::new("invalid_params", m)),
        Err(PrecheckError::Run(m)) => internal(m),
    }
}

/// The `integrations.v1` methods (R3, DL-164): issues, pull requests and
/// their writes. The service owns the error codes.
async fn integration(ctx: &StationLink, method: &str, params: serde_json::Value) -> Outcome {
    let policy = ctx.manager.policy();
    let resolve = |path: &str| policy.workspace(path).ok();
    ctx.manager
        .integrations()
        .call(method, params, &resolve)
        .await
}

/// `assist.generate` (DL-167): one model-help text. Error codes `disabled`,
/// `budget`, `no_provider`, `invalid_params` and `failed`; a caller keeps
/// its plain fallback for each of them.
async fn assist(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let request: AssistRequest = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let Some(assist) = ctx.manager.assist() else {
        return Outcome::Err(WireError::new(
            "disabled",
            "this station runs without model help",
        ));
    };
    let providers = ctx.manager.providers().await;
    match assist
        .generate(request.kind, &request.input, &providers)
        .await
    {
        Ok(answer) => ok(answer),
        Err(e) => Outcome::Err(WireError::new(e.code(), e.to_string())),
    }
}

/// The `files.v1` methods (M3 DL-088). A [`FileError`] keeps its code.
///
/// [`FileError`]: crate::workspace::files::FileError
async fn file_method(ctx: &StationLink, method: &str, params: serde_json::Value) -> Outcome {
    use crate::manager::files;
    use crate::workspace::GitOptions;
    use crate::workspace::files::FileError;
    fn done<T: Serialize>(r: Result<T, FileError>) -> Outcome {
        match r {
            Ok(v) => ok(v),
            Err(e) => Outcome::Err(WireError::new(e.code, e.message)),
        }
    }
    let store = ctx.manager.store();
    // The snapshot of this call: the roots and `member_deny` of the moment.
    let current = ctx.manager.policy();
    let policy = &*current;
    let git = GitOptions::isolated();
    // The deck marks a call for a member of a shared session (R11.9); the
    // station hides the files of the member deny list from it.
    let mut params = params;
    let member = fleetdeck_proto::member_deny::take_member_flag(&mut params);
    match method {
        "files.list" => match serde_json::from_value(params) {
            Ok(q) => done(files::list_as(store, policy, q, member).await),
            Err(e) => bad_params(e),
        },
        "files.read" => match serde_json::from_value(params) {
            Ok(q) => done(files::read_as(store, policy, q, member).await),
            Err(e) => bad_params(e),
        },
        "files.search" => match serde_json::from_value(params) {
            Ok(q) => done(files::search_as(store, policy, &git, q, member).await),
            Err(e) => bad_params(e),
        },
        "files.diff" => match serde_json::from_value(params) {
            Ok(q) => done(files::diff_as(store, policy, &git, q, member).await),
            Err(e) => bad_params(e),
        },
        "files.session_diff" => match serde_json::from_value(params) {
            Ok(q) => done(crate::workspace::session_diff::run(store, policy, &git, q).await),
            Err(e) => bad_params(e),
        },
        other => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("`{other}` is not served by this station build"),
        )),
    }
}

/// The `artifacts.*` methods (R12, DL-165). A station without an artifact
/// directory answers `unsupported`; a service error keeps its code.
async fn artifact_method(ctx: &StationLink, method: &str, params: serde_json::Value) -> Outcome {
    use crate::artifacts::ArtifactError;
    use fleetdeck_proto::artifacts::code;
    fn done<T: Serialize>(r: Result<T, ArtifactError>) -> Outcome {
        match r {
            Ok(v) => ok(v),
            Err(e) => Outcome::Err(WireError::new(e.code, e.message)),
        }
    }
    let Some(service) = ctx.manager.artifacts() else {
        return Outcome::Err(WireError::new(
            code::UNSUPPORTED,
            "this station has no artifact directory",
        ));
    };
    match method {
        "artifacts.publish" => match serde_json::from_value(params) {
            Ok(q) => done(service.publish(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.list" => match serde_json::from_value(params) {
            Ok(q) => done(service.list(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.get" => match serde_json::from_value(params) {
            Ok(q) => done(service.get(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.file" => match serde_json::from_value(params) {
            Ok(q) => done(service.file(q).await),
            Err(e) => bad_params(e),
        },
        other => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("`{other}` is not served by this station build"),
        )),
    }
}

/// Answers a request that is not `events.replay` or `session.snapshot`
/// (the stream handles those).
pub(super) async fn handle(
    ctx: &StationLink,
    control: &Control,
    peer: &PeerKey,
    req: InboundRequest,
) {
    let id = req.id;
    if let Some(outcome) = answer(ctx, control, peer, req).await
        && let Err(e) = control.respond(id, outcome).await
    {
        tracing::debug!(error = %e, "response");
    }
}

/// The outcome of a request; `None` when it was already answered.
async fn answer(
    ctx: &StationLink,
    control: &Control,
    peer: &PeerKey,
    req: InboundRequest,
) -> Option<Outcome> {
    if req.spec.mutating {
        return mutate(ctx, control, peer, req).await;
    }
    Some(read(ctx, req).await)
}

async fn read(ctx: &StationLink, req: InboundRequest) -> Outcome {
    let store = ctx.manager.store();
    match req.method.as_str() {
        "station.info" => {
            let usage = match ctx.manager.assist() {
                Some(assist) => assist.usage().await,
                None => None,
            };
            ok(StationInfo {
                name: ctx.name.clone(),
                incarnation: store.incarnation().clone(),
                head: store.head(),
                live_sessions: ctx.manager.live_sessions() as u64,
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                max_sessions: Some(ctx.manager.policy().max_sessions()),
                allow_shell: Some(ctx.manager.policy().allow_shell()),
                allow_setup_cmd: Some(ctx.manager.policy().allow_setup_cmd()),
                providers: ctx.manager.providers().await,
                assist_tokens_today: usage.map(|(tokens, _)| tokens),
                assist_cap: usage.map(|(_, cap)| cap),
            })
        }
        "workspace.list" => workspaces(ctx, req.params).await,
        "workspace.worktrees" => worktrees(ctx, req.params).await,
        "workspace.preflight" => preflight(ctx, req.params).await,
        "workspace.precheck" => precheck(ctx, req.params).await,
        "assist.generate" => assist(ctx, req.params).await,
        "files.write_temp" => write_temp(ctx, req.params).await,
        m if crate::integrations::Integrations::serves(m) => integration(ctx, m, req.params).await,
        "files.list" | "files.read" | "files.search" | "files.diff" | "files.session_diff" => {
            file_method(ctx, &req.method, req.params).await
        }
        "artifacts.publish" | "artifacts.list" | "artifacts.get" | "artifacts.file" => {
            artifact_method(ctx, &req.method, req.params).await
        }
        "shell.list" => ok(ctx.shells.list()),
        "session.list" => match store.snapshot().await {
            Ok(s) => ok(s.sessions),
            Err(e) => internal(e),
        },
        "command.receipt" => {
            let query: ReceiptQuery = match serde_json::from_value(req.params) {
                Ok(q) => q,
                Err(e) => return bad_params(e),
            };
            match store.receipt(query.command_id).await {
                Ok(r) => ok(r),
                Err(e) => internal(e),
            }
        }
        // The workspace service and its data streams are Phase 6.
        other => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("`{other}` is not served by this station build"),
        )),
    }
}

/// `shell.attach` (PLAN §6.8): a single-use token for one data stream of
/// this link with the purpose `shell:<id>`. Error codes `unsupported` (no
/// data channel on this link) and `invalid_state` (no such shell).
pub(super) fn shell_attach(
    ctx: &StationLink,
    data: Option<&fleetdeck_link::data::Data>,
    params: serde_json::Value,
) -> Outcome {
    use fleetdeck_proto::shell::{STREAM_PURPOSE, ShellAttach, ShellAttached};
    let query: ShellAttach = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    let Some(data) = data else {
        return Outcome::Err(WireError::new(
            "unsupported",
            "this link has no data channel",
        ));
    };
    if !ctx.shells.exists(&query.shell_id) {
        return Outcome::Err(WireError::new(
            "invalid_state",
            format!("no shell `{}`", query.shell_id),
        ));
    }
    match data.issue_token(&format!("{STREAM_PURPOSE}{}", query.shell_id)) {
        Ok(token) => ok(ShellAttached {
            stream_token: token.as_str().to_owned(),
        }),
        Err(e) => internal(e),
    }
}

fn not_stored(command_id: fleetdeck_proto::CommandId, reject: Reject, now: i64) -> Outcome {
    ok(Receipt::rejected(command_id, reject, now))
}

async fn mutate(
    ctx: &StationLink,
    control: &Control,
    peer: &PeerKey,
    req: InboundRequest,
) -> Option<Outcome> {
    let id = req.id;
    let store = ctx.manager.store();
    let now = store.now_ms();
    // `command_id` is present: the control channel refuses a mutating
    // request without one.
    let Some(command_id) = req.command_id.clone() else {
        return Some(Outcome::Err(WireError::new(
            code::MISSING_COMMAND_ID,
            "no command_id",
        )));
    };
    let mutation = match Mutation::parse(&req.method, req.command_id.as_ref(), req.params) {
        Ok(m) => m,
        Err(reject) => return Some(not_stored(command_id, reject, now)),
    };
    let caller = Caller::peer(peer.fingerprint(), req.spec.scope);
    let decided = match mutation {
        Mutation::Session(envelope) => {
            if envelope.target_incarnation.is_none() {
                let reject = Reject::new(
                    RejectCode::InvalidCommand,
                    "target_incarnation is required over a link",
                );
                return Some(not_stored(command_id, reject, now));
            }
            ctx.manager.submit(envelope, caller).await
        }
        Mutation::RotateKey(envelope) => {
            return rotate(ctx, control, id, peer, envelope, caller).await;
        }
        // DL-047: the station policy is read-only in this build.
        m @ Mutation::Configure(_) => {
            store
                .decide_mutation(m, caller, |_| {
                    Err(DecideError::Reject(Reject::new(
                        RejectCode::Unsupported,
                        "station.configure is not supported by this station build",
                    )))
                })
                .await
        }
        Mutation::Shell(envelope) => {
            // DL-405: the receipt transaction only checks and enqueues; the
            // PTY I/O runs after the commit, claimed first.
            let shells = std::sync::Arc::clone(&ctx.shells);
            let policy = ctx.manager.policy();
            let decided = store
                .decide_mutation(Mutation::Shell(envelope.clone()), caller, move |d| {
                    crate::shell::decide(d, &shells, &policy, &envelope)
                })
                .await;
            if let Err(e) =
                crate::shell::dispatch(store, &ctx.shells, ctx.manager.policy_hub()).await
            {
                tracing::warn!(error = %e, "shell dispatch");
            }
            decided
        }
        // The roles of the method table never deliver it to a station.
        Mutation::Coordinator(_) => {
            let reject = Reject::new(
                RejectCode::InvalidCommand,
                "a station does not hold coordinator calls",
            );
            return Some(not_stored(command_id, reject, now));
        }
    };
    Some(match decided {
        Ok(d) => ok(d.receipt),
        Err(e) => internal(e),
    })
}

fn rotate_reject(e: &RotateError) -> Reject {
    let code = match e {
        RotateError::NotLinkPeer | RotateError::NotPaired => RejectCode::Forbidden,
        RotateError::Store(_) => RejectCode::Unknown,
        _ => RejectCode::InvalidCommand,
    };
    Reject::new(code, e.to_string())
}

/// `link.rotate_key`: the checks of `fleetdeck-link`, then the key change
/// in the decision transaction (one transaction with the receipt), then the
/// link-layer part (pins, grant cache, close of the old key's link).
async fn rotate(
    ctx: &StationLink,
    control: &Control,
    id: u64,
    peer: &PeerKey,
    envelope: fleetdeck_proto::mutation::KeyRotationEnvelope,
    caller: Caller,
) -> Option<Outcome> {
    let store = ctx.manager.store();
    let now = store.now_ms();
    let command_id = envelope.command_id.clone();
    let (new_key, role) = match check_rotation(&ctx.node, peer, &envelope.rotation, &command_id) {
        Ok(checked) => checked,
        Err(e) => return Some(not_stored(command_id, rotate_reject(&e), now)),
    };
    let old = *peer;
    let decided = store
        .decide_mutation(
            Mutation::RotateKey(envelope),
            caller,
            move |d| match peers::rotate_in(d.connection(), &old, &new_key, d.now()) {
                Ok(()) => Ok(CommandResult::default()),
                Err(StoreError::UnknownDeck(k)) => Err(DecideError::Reject(Reject::new(
                    RejectCode::Forbidden,
                    format!("key {k} is not paired"),
                ))),
                Err(e) => Err(DecideError::Store(e)),
            },
        )
        .await;
    match decided {
        Ok(d) => {
            if d.receipt.state == ReceiptState::Accepted && !d.duplicate {
                ctx.peers.rotated_in_store(&old, &new_key);
                // The receipt goes out before the old key's link closes.
                let reply = ok(d.receipt);
                if let Err(e) = control.respond(id, reply).await {
                    tracing::debug!(error = %e, "rotation receipt");
                }
                finish_rotation(&ctx.node, &old, new_key, role);
                return None;
            }
            Some(ok(d.receipt))
        }
        Err(e) => Some(internal(e)),
    }
}
