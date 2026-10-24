//! The sends of the move driver (design §12.3; FG-9, T4.4b). [`handle`] runs
//! the step of one row by its state: the steps 1 to 4 in one task, the poll of
//! a `starting` move, and the resends of the finish, the release, the abort
//! and the undo.
//!
//! The commands (`MoveOut`, `MoveIn`, `MoveFinish`, `MoveUndo`) have a fixed
//! command id and a new envelope for each send, as the land sender
//! (decision Q7): the station keeps the receipt, so a resend is safe.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, Receipt, ReceiptState, RejectCode,
    SessionCommand,
};
use fleetdeck_proto::fleet_git::{
    MoveExportAnswer, MoveFrom, MoveIdParams, MoveKind, MovePrepareAnswer, MovePrepareParams,
    MovePublication, MoveSourceRef, StartKind, station_slug,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, ProviderInstanceId, SessionId, StationId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::fleet_git_move::Ctx;
use crate::store::LinkFeature;
use crate::store::fleet_git_start::{StartOutcome, StartQuery};
use crate::store::session_moves::{MoveRow, MoveState};
use crate::store::session_moves_steps::{MoveGate, MovePatch, authority_text, backoff_ms};

/// The longest summary of a branch-only move, in characters (design §12.6).
const SUMMARY_MAX: usize = 4000;

/// A failure of a step: the code and the text for the board. An empty code
/// means that another party moved the row first: the step stops quietly.
#[derive(Debug, Clone)]
pub(crate) struct Fail {
    pub code: String,
    pub text: String,
}

impl Fail {
    pub(crate) fn new(code: &str, text: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            text: text.into(),
        }
    }

    fn quiet() -> Self {
        Self::new("", "")
    }

    fn internal(e: &dyn std::fmt::Display) -> Self {
        tracing::warn!(error = %e, "move store");
        Self::new(
            "internal",
            "Fleet Deck could not read its own data. Try again.",
        )
    }
}

/// What one command send came to.
pub(crate) enum Cmd {
    Accepted(Receipt),
    /// The station stored a rejected receipt.
    Rejected(Fail),
    /// No receipt: no link, a timeout, or a receipt that is not stored.
    Failed(Fail),
}

/// What one request came to.
pub(crate) enum Ask<T> {
    Ok(T),
    /// The station answered with an error.
    Err(Fail),
    /// No answer: no link, a timeout.
    Failed(Fail),
}

impl<T> Ask<T> {
    pub(crate) fn fail(self) -> Result<T, Fail> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Err(f) | Self::Failed(f) => Err(f),
        }
    }
}

/// The code and the text of a rejected receipt (as the land sender): the
/// `<code>: <text>` prefix of the message, else the reject code name.
fn reject_fail(reject: &fleetdeck_proto::Reject) -> Fail {
    if let Some((code, rest)) = reject.message.split_once(": ")
        && !code.is_empty()
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        return Fail::new(code, rest);
    }
    let name = serde_json::to_value(reject.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "rejected".to_owned());
    Fail::new(&name, reject.message.clone())
}

/// One move row with its two stations, for the steps of one task.
pub(crate) struct Run {
    pub ctx: Arc<Ctx>,
    pub row: MoveRow,
    pub src: StationId,
    pub dst: StationId,
    pub src_name: String,
    pub dst_name: String,
}

/// Runs the step of `row` by its state.
pub(crate) async fn handle(ctx: &Arc<Ctx>, row: MoveRow) {
    let Ok(Some(row)) = ctx.store.move_row(row.move_id.clone()).await else {
        return;
    };
    let run = Run {
        src: StationId::from(row.src_station.clone()),
        dst: StationId::from(row.dst_station.clone()),
        src_name: ctx.store.move_station_name(&row.src_station).await,
        dst_name: ctx.store.move_station_name(&row.dst_station).await,
        ctx: Arc::clone(ctx),
        row,
    };
    match run.row.state {
        MoveState::Holding => run.forward().await,
        MoveState::Starting => run.poll().await,
        MoveState::Committed | MoveState::Finishing => run.finish().await,
        MoveState::Releasing => run.release().await,
        MoveState::Aborting => run.aborting().await,
        MoveState::Undoing => run.undoing().await,
        _ => {}
    }
}

impl Run {
    pub(crate) fn id(&self) -> String {
        self.row.move_id.clone()
    }

    pub(crate) fn session(&self) -> SessionId {
        SessionId::from(self.row.src_session.clone())
    }

    fn unreachable(&self, station: &StationId) -> Fail {
        let name = if station == &self.src {
            &self.src_name
        } else {
            &self.dst_name
        };
        Fail::new(
            "station_unreachable",
            format!(
                "Fleet Deck lost contact with {name} during the move. The session stays on {}.",
                self.src_name
            ),
        )
    }

    /// The link of `station` for the move feature, or the failure.
    fn link(&self, station: &StationId) -> Result<fleetdeck_link::control::Control, Fail> {
        match self
            .ctx
            .store
            .link_feature(station, feature::FLEET_GIT_MOVE)
        {
            LinkFeature::Negotiated => {}
            LinkFeature::Without => {
                return Err(Fail::new(
                    "fleet_git_unsupported",
                    "This station does not support Fleet Git. Update it first.",
                ));
            }
            LinkFeature::Down => return Err(self.unreachable(station)),
        }
        self.ctx
            .links
            .control_for(station)
            .ok_or_else(|| self.unreachable(station))
    }

    pub(crate) async fn ask<T: DeserializeOwned>(
        &self,
        station: &StationId,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Ask<T> {
        let control = match self.link(station) {
            Ok(c) => c,
            Err(f) => return Ask::Failed(f),
        };
        match tokio::time::timeout(deadline, control.request(method, None, params)).await {
            Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value(v) {
                Ok(answer) => Ask::Ok(answer),
                Err(e) => {
                    tracing::warn!(method, error = %e, "move answer does not parse");
                    Ask::Err(Fail::new(
                        "internal",
                        format!("{method} gave an unknown answer."),
                    ))
                }
            },
            Ok(Ok(Outcome::Err(e))) => Ask::Err(Fail::new(&e.code, e.message)),
            Ok(Err(e)) => {
                tracing::debug!(method, error = %e, "move call: link ended");
                Ask::Failed(self.unreachable(station))
            }
            Err(_) => {
                tracing::debug!(method, "move call timed out");
                Ask::Failed(self.unreachable(station))
            }
        }
    }

    /// One send of a command with its fixed id.
    pub(crate) async fn command(
        &self,
        station: &StationId,
        command_id: &str,
        command: SessionCommand,
        deadline: Duration,
    ) -> Cmd {
        let control = match self.link(station) {
            Ok(c) => c,
            Err(f) => return Cmd::Failed(f),
        };
        let Ok(Some(rec)) = self.ctx.store.station(station.clone()).await else {
            return Cmd::Failed(self.unreachable(station));
        };
        let Some(incarnation) = rec.incarnation else {
            return Cmd::Failed(self.unreachable(station));
        };
        // `MoveOut` and `MoveIn` of a coordinator move carry its authority.
        let authority = matches!(
            command,
            SessionCommand::MoveOut { .. } | SessionCommand::MoveIn { .. }
        )
        .then(|| self.row.authority.clone())
        .flatten();
        let envelope = CommandEnvelope {
            command_id: CommandId::from(command_id.to_owned()),
            target_incarnation: Some(incarnation),
            retry_until: self.ctx.store.now_ms().saturating_add(RETRY_WINDOW_MS),
            origin: if authority.is_some() {
                CommandOrigin::Coordinator
            } else {
                CommandOrigin::Board
            },
            authority,
            command,
        };
        let method = envelope.command.method();
        let Ok(params) = serde_json::to_value(&envelope) else {
            return Cmd::Failed(Fail::internal(&"envelope"));
        };
        let reply = tokio::time::timeout(
            deadline,
            control.request(method, Some(&envelope.command_id), params),
        )
        .await;
        let value = match reply {
            Ok(Ok(Outcome::Ok(v))) => v,
            Ok(Ok(Outcome::Err(e))) => return Cmd::Failed(Fail::new(&e.code, e.message)),
            Ok(Err(_)) | Err(_) => return Cmd::Failed(self.unreachable(station)),
        };
        let Ok(receipt) = serde_json::from_value::<Receipt>(value) else {
            return Cmd::Failed(Fail::new(
                "internal",
                format!("{method} gave no valid receipt."),
            ));
        };
        if receipt.command_id.as_str() != command_id {
            return Cmd::Failed(Fail::new(
                "internal",
                format!("{method} answered another command."),
            ));
        }
        match receipt.state {
            ReceiptState::Accepted => Cmd::Accepted(receipt),
            ReceiptState::Rejected => {
                let Some(reject) = receipt.reject else {
                    return Cmd::Failed(Fail::new("internal", format!("{method} gave no reason.")));
                };
                // These two store no receipt: the send is repeated.
                if matches!(
                    reject.code,
                    RejectCode::IncarnationMismatch | RejectCode::Expired
                ) {
                    return Cmd::Failed(self.unreachable(station));
                }
                Cmd::Rejected(reject_fail(&reject))
            }
        }
    }

    /// A command of steps 1 to 4: any failure is a failure of the step.
    async fn send(
        &self,
        station: &StationId,
        command_id: &str,
        command: SessionCommand,
        deadline: Duration,
    ) -> Result<Receipt, Fail> {
        match self.command(station, command_id, command, deadline).await {
            Cmd::Accepted(r) => Ok(r),
            Cmd::Rejected(f) | Cmd::Failed(f) => Err(f),
        }
    }

    async fn advance(
        &self,
        from: &'static [MoveState],
        to: MoveState,
        patch: MovePatch,
    ) -> Result<(), Fail> {
        match self
            .ctx
            .store
            .move_advance(self.id(), from, to, patch)
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(Fail::quiet()),
            Err(e) => Err(Fail::internal(&e)),
        }
    }

    /// The authority gate of a coordinator move (design §12.3, B3).
    async fn gate(&self) -> Result<(), Fail> {
        let started = tokio::time::Instant::now();
        let unit = self.ctx.opts.retry_unit_ms;
        let mut n = 0;
        loop {
            match self.ctx.store.move_gate(self.id()).await {
                Err(e) => return Err(Fail::internal(&e)),
                Ok(MoveGate::Go) => return Ok(()),
                Ok(MoveGate::Lost { code, reason }) => {
                    return Err(Fail::new(code, authority_text(code, &reason)));
                }
                Ok(MoveGate::Wait) => {
                    if started.elapsed() > self.ctx.opts.deadlines.gate_wait {
                        let code = "authority_unknown";
                        return Err(Fail::new(code, authority_text(code, "")));
                    }
                    let wait = u64::try_from(backoff_ms(n, unit)).unwrap_or(1000);
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                    n += 1;
                }
            }
        }
    }

    /// Steps 1 to 4 (design §12.3).
    async fn forward(&self) {
        if let Err(f) = self.steps().await {
            if f.code.is_empty() {
                return;
            }
            tracing::info!(move_id = %self.row.move_id, code = %f.code, "move aborts");
            let note = MovePatch::note(&f.code, &f.text);
            let from = &[
                MoveState::Holding,
                MoveState::Exporting,
                MoveState::Importing,
                MoveState::Starting,
            ];
            if let Err(e) = self
                .ctx
                .store
                .move_advance(self.id(), from, MoveState::Aborting, note)
                .await
            {
                tracing::warn!(error = %e, "move abort");
            }
        }
    }

    async fn steps(&self) -> Result<(), Fail> {
        let d = self.ctx.opts.deadlines;
        let id = self.id();
        // 1. Hold (source).
        self.gate().await?;
        let out = SessionCommand::MoveOut {
            session_id: self.session(),
            move_id: id.clone(),
            to_station: self.dst.clone(),
        };
        self.send(&self.src, &self.row.out_command_id, out, d.move_out)
            .await?;
        self.advance(
            &[MoveState::Holding],
            MoveState::Exporting,
            MovePatch::default(),
        )
        .await?;
        // 2. Export (source).
        self.gate().await?;
        let answer: MoveExportAnswer = self
            .ask(
                &self.src,
                "session.move_export",
                json!(MoveIdParams {
                    move_id: id.clone()
                }),
                d.export,
            )
            .await
            .fail()?;
        // The instance for `MoveIn`, checked on the target before any state
        // advance and before `session.move_prepare` (FG-9).
        let instance = self.instance(answer.provider_instance.as_ref()).await?;
        let publication = answer.publication.clone();
        let mut patch = MovePatch {
            publication: Some(publication.clone()),
            ..MovePatch::default()
        };
        let kind = match (&answer.manifest, &answer.stream_token) {
            (Some(manifest), Some(token)) => {
                patch.kind = Some("native".to_owned());
                patch.bytes = Some(manifest.total);
                self.advance(&[MoveState::Exporting], MoveState::Importing, patch)
                    .await?;
                self.import(manifest, token).await?;
                self.advance(
                    &[MoveState::Importing],
                    MoveState::Starting,
                    MovePatch::default(),
                )
                .await?;
                MoveKind::Native
            }
            (None, None) => {
                patch.kind = Some("branch_only".to_owned());
                self.advance(&[MoveState::Exporting], MoveState::Starting, patch)
                    .await?;
                MoveKind::BranchOnly
            }
            _ => {
                return Err(Fail::new(
                    "move_checksum",
                    "The conversation files did not arrive complete. Try again.",
                ));
            }
        };
        // 4. Start (target).
        self.gate().await?;
        self.move_in(publication, kind, instance).await
    }

    /// The provider instance of `MoveIn`: the one of the summary, else the one
    /// of the export answer. The move aborts (`move_not_ready`) when there is
    /// none or when the target has no such instance.
    async fn instance(
        &self,
        exported: Option<&ProviderInstanceId>,
    ) -> Result<ProviderInstanceId, Fail> {
        let store = &self.ctx.store;
        let facts = store
            .move_in_facts(self.id())
            .await
            .map_err(|e| Fail::internal(&e))?
            .ok_or_else(|| {
                Fail::new("move_not_ready", "This session is not in a synced project.")
            })?;
        let instance = facts
            .instance
            .map(ProviderInstanceId::from)
            .or_else(|| exported.cloned())
            .filter(|i| !i.to_string().is_empty());
        let Some(instance) = instance else {
            return Err(Fail::new(
                "move_not_ready",
                format!("{} has no provider for this session.", facts.dst_name),
            ));
        };
        let known = store
            .station_info(&self.dst)
            .is_some_and(|(info, _)| info.providers.iter().any(|p| p.instance == instance));
        if known {
            Ok(instance)
        } else {
            Err(Fail::new(
                "move_not_ready",
                format!("{} has no provider named {instance}.", facts.dst_name),
            ))
        }
    }

    /// Step 3: prepare the target and pipe the files.
    async fn import(
        &self,
        manifest: &fleetdeck_proto::fleet_git::TranscriptManifest,
        export_token: &str,
    ) -> Result<(), Fail> {
        let d = self.ctx.opts.deadlines;
        self.gate().await?;
        let params = MovePrepareParams {
            move_id: self.id(),
            manifest: manifest.clone(),
            source: MoveSourceRef {
                station_id: self.src.clone(),
                session_id: self.session(),
            },
        };
        let prepared: MovePrepareAnswer = self
            .ask(&self.dst, "session.move_prepare", json!(params), d.prepare)
            .await
            .fail()?;
        self.gate().await?;
        crate::fleet_git_move_pipe::run(
            &self.ctx.links,
            (&self.src, &self.dst),
            (export_token, &prepared.stream_token),
            manifest.total,
            &d,
        )
        .await
    }

    /// Step 4: build `MoveIn` and send it.
    async fn move_in(
        &self,
        publication: MovePublication,
        kind: MoveKind,
        instance: ProviderInstanceId,
    ) -> Result<(), Fail> {
        let store = &self.ctx.store;
        let facts = store
            .move_in_facts(self.id())
            .await
            .map_err(|e| Fail::internal(&e))?
            .ok_or_else(|| {
                Fail::new("move_not_ready", "This session is not in a synced project.")
            })?;
        let slug = store
            .station_info(&self.dst)
            .map(|(info, _)| station_slug(&info.name));
        let query = StartQuery {
            station: self.dst.0.clone(),
            cwd: facts.home_folder.clone(),
            kind: StartKind::Worktree,
            slug,
            feature: store.link_feature(&self.dst, feature::FLEET_GIT),
            project: Some(facts.project_id.clone()),
        };
        let start = match store
            .git_start(query)
            .await
            .map_err(|e| Fail::internal(&e))?
        {
            StartOutcome::Attach(start) => start,
            outcome => return Err(self.start_fail(outcome, &facts.dst_name)),
        };
        let summary = self.summary(facts.digest.as_deref()).await;
        let command = SessionCommand::MoveIn {
            move_id: self.id(),
            kind,
            provider_instance: instance,
            title: facts.title,
            callsign_hint: facts.callsign,
            from: MoveFrom {
                station_id: self.src.clone(),
                session_id: self.session(),
                publication,
                summary,
            },
            fleet_git: start,
        };
        let d = self.ctx.opts.deadlines;
        let receipt = self
            .send(&self.dst, &self.row.in_command_id, command, d.move_in)
            .await?;
        if let Some(session) = receipt.result.and_then(|r| r.session_id)
            && let Err(e) = store.move_set_dst(self.id(), session.0).await
        {
            tracing::warn!(error = %e, "move target session");
        }
        let poll = i64::try_from(self.ctx.opts.poll.as_millis()).unwrap_or(10_000);
        store
            .move_next_at(self.id(), poll)
            .await
            .map_err(|e| Fail::internal(&e))
    }

    fn start_fail(&self, outcome: StartOutcome, dst: &str) -> Fail {
        match outcome {
            StartOutcome::Attach(_) | StartOutcome::Plain => Fail::new(
                "move_not_ready",
                format!("{dst} has no home of this project."),
            ),
            StartOutcome::Unsupported => Fail::new(
                "fleet_git_unsupported",
                "This station does not support Fleet Git. Update it first.",
            ),
            StartOutcome::StationInfoMissing => Fail::new(
                "station_info_missing",
                "The deck does not know this station's name yet. Try again in a moment.",
            ),
            StartOutcome::Unverified { path, repo } => Fail::new(
                "home_unverified",
                format!(
                    "The home {path} on {dst} does not point to {repo}. Fix its remote, then try again."
                ),
            ),
            StartOutcome::NotReady(text) => Fail::new("home_not_ready", text),
            StartOutcome::SlugTaken { repo, slug, other } => Fail::new(
                "slug_taken",
                format!(
                    "The Git name {slug} is reserved for station {other} in {repo}. Rename this station."
                ),
            ),
        }
    }

    /// The summary of a branch-only prompt (design §12.6, T4.4b Q12): the
    /// digest, else the last assistant message, at most 4000 characters.
    async fn summary(&self, digest: Option<&str>) -> Option<String> {
        let text = match digest.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => d.to_owned(),
            None => self
                .ctx
                .store
                .move_last_message(self.row.src_station.clone(), self.row.src_session.clone())
                .await
                .ok()
                .flatten()?,
        };
        Some(text.chars().take(SUMMARY_MAX).collect())
    }
}
