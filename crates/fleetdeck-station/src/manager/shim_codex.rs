//! The durable Codex driver of a shim session (ADR-005 §2.1; DL-110,
//! DL-112): [`CodexCore`] as a [`DurableCore`] and the Codex handshake
//! (`initialize`, `thread/start`) of a newly launched shim. A restored
//! driver never repeats the handshake ([`DurableDriver::restore`]).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_providers::adapter::{Answer, ProviderError};
use fleetdeck_providers::apps::{
    AppBridge, AppError, AppTool, READ_TIMEOUT, TOOLS_TIMEOUT, ToolAnswer,
};
use fleetdeck_providers::checkpoint::{AdapterCheckpoint, CheckpointError};
use fleetdeck_providers::codex::AppOp as CodexAppOp;
use fleetdeck_providers::codex::{
    CodexCore, CoreConfig, Output, SessionSpec, TOOLS_PAGES, parse_tools_page, rpc, tool_answer,
};
use fleetdeck_providers::host::shim::host::Attached;

pub use super::durable::{DriverError, REASON_FRAME_UNKNOWN};
use super::durable::{DurableCore, DurableDriver, OPEN_WAIT, Steps, host_input, not_a_command};
use crate::store::{Effect, EffectPayload, IdentitySignal, NewEvent, Store};
use serde_json::Value;

/// The durable Codex driver of one shim.
pub type DurableCodex = DurableDriver<CodexCore>;

pub(super) fn split(out: Vec<Output>) -> Steps {
    let mut steps = Steps::default();
    for o in out {
        match o {
            Output::Emit(event) => steps.events.push(NewEvent::from(event)),
            Output::Write(frame) => steps.frames.push(frame.to_string()),
            Output::NativeIdentity { native_id } => steps.identities.push(IdentitySignal {
                native_id,
                after: steps.events.len(),
            }),
            Output::AppCandidate(candidate) => steps.apps.push(candidate),
        }
    }
    steps
}

impl DurableCore for CodexCore {
    type Config = CoreConfig;

    fn create(config: CoreConfig) -> Self {
        Self::new(config)
    }

    fn save(&self) -> Result<AdapterCheckpoint, CheckpointError> {
        self.checkpoint()
    }

    fn load(config: CoreConfig, checkpoint: &AdapterCheckpoint) -> Result<Self, CheckpointError> {
        Self::restore(config, checkpoint)
    }

    fn keep_runtime_of(&mut self, old: &mut Self) {
        Self::keep_runtime_of(self, old);
    }

    fn line(&mut self, line: &str) -> Steps {
        match rpc::parse_line(line) {
            Ok(frame) => split(self.on_frame(frame)),
            Err(_) => Steps::default(),
        }
    }

    fn exit(&mut self, code: Option<i32>) -> Steps {
        split(self.on_exit(code))
    }

    fn command(&mut self, effect: &Effect) -> Result<Steps, ProviderError> {
        let out = match &effect.payload {
            EffectPayload::Prompt { input, .. } | EffectPayload::FollowUp { input, .. } => {
                self.prompt(&host_input(input))
            }
            EffectPayload::Steer { input, .. } => self.steer(&host_input(input)),
            EffectPayload::Interrupt { .. } => {
                self.interrupt(Some(effect.effect_id.as_str().to_owned()))
            }
            EffectPayload::Answer {
                request_id,
                decision,
                by,
                ..
            } => self.answer(&Answer {
                request: request_id.clone(),
                decision: decision.clone(),
                by: *by,
            }),
            EffectPayload::SetMode { mode, .. } => self.set_mode(mode),
            EffectPayload::SetModel { model } => self.set_model(model),
            EffectPayload::SetEffort { effort } => {
                self.set_effort(effort, Some(effect.effect_id.as_str().to_owned()))
            }
            // The first input of a launch (the start or resume effect is
            // claimed while it is written, DL-113, DL-182).
            EffectPayload::Start {
                prompt: Some(input),
                ..
            }
            | EffectPayload::Resume {
                prompt: Some(input),
                ..
            } => self.prompt(&host_input(input)),
            EffectPayload::Start { prompt: None, .. }
            | EffectPayload::Resume { prompt: None, .. }
            | EffectPayload::Stop { .. }
            | EffectPayload::Revert { .. }
            | EffectPayload::Worktree { .. }
            | EffectPayload::Land { .. }
            | EffectPayload::MoveIn { .. }
            | EffectPayload::StopExecution { .. } => Err(not_a_command()),
            // B8: a `!` command of a Codex session.
            EffectPayload::ShellCommand {
                command,
                timeout_ms,
                ..
            } => self.shell_command(effect.effect_id.as_str(), command, *timeout_ms),
        }?;
        Ok(split(out))
    }

    fn ended(&self) -> bool {
        self.has_ended()
    }
}

impl DurableDriver<CodexCore> {
    /// Rolls the conversation back by `turns` turns (`thread/rollback`;
    /// DL-116), the shim-host form of the direct driver's rollback. A
    /// refusal before the frame is committed is definite; once the
    /// `thread/rollback` state and frame are committed, every failure is
    /// [`ProviderError::Uncertain`] except the provider's own refusal. The
    /// committed `Sent` state survives a station restart, so the restored
    /// core still refuses conversation input until the answer arrives.
    ///
    /// # Errors
    /// See above.
    pub async fn rollback(&self, turns: u32, timeout: Duration) -> Result<(), ProviderError> {
        match self.internal(|c| c.rollback(turns).map(split)).await {
            Ok(()) => {}
            Err(DriverError::Provider(e)) => return Err(e),
            Err(e) => return Err(ProviderError::Uncertain(e.to_string())),
        }
        let deadline = tokio::time::Instant::now() + timeout;
        if self
            .wait_until(|c| c.rollback_answer().is_some(), deadline, "rollback")
            .await
            .is_err()
        {
            return Err(ProviderError::Uncertain(
                "no rollback answer before the deadline or the provider ended".to_owned(),
            ));
        }
        match self.with_core(CodexCore::rollback_answer).await {
            Some(Ok(())) => Ok(()),
            Some(Err(refusal)) => Err(ProviderError::Refused(refusal)),
            None => Err(ProviderError::Uncertain(
                "the rollback answer went missing".to_owned(),
            )),
        }
    }

    /// Drives a newly launched shim: commits the first checkpoint, then the
    /// handshake and the thread (`initialize`, the fleet skill requests
    /// when `skill_dir` is set, `thread/start`). With `offer_apps`, the
    /// `initialize` offers the MCP Apps extension (B15; the adapter's
    /// `offers_mcp_apps` decides).
    ///
    /// # Errors
    /// A store error, or a handshake that does not finish.
    pub async fn open(
        store: &Store,
        attached: Attached,
        config: CoreConfig,
        thread: &SessionSpec,
        skill_dir: Option<&Path>,
        offer_apps: bool,
    ) -> Result<Arc<Self>, DriverError> {
        let mut core = CodexCore::new(config.clone());
        core.set_offer_mcp_apps(offer_apps);
        let this = Self::new(store, &attached, config, core);
        this.start_pump(attached);
        this.internal(|c| c.initialize().map(split)).await?;
        this.wait(CodexCore::is_initialized, "initialize").await?;
        if let Some(dir) = skill_dir {
            this.inject_skill(dir).await?;
        }
        this.internal(|c| c.open_thread(thread).map(split)).await?;
        this.wait(|c| !c.is_opening() && c.thread_id().is_some(), "thread")
            .await?;
        Ok(this)
    }

    /// One app request and its answer (see [`DurableDriver::app_exchange`]).
    async fn codex_app(
        &self,
        op: CodexAppOp,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Result<Value, String>, AppError> {
        self.app_exchange(
            |c| c.app_request(&op).map(|(id, out)| (id, split(out))),
            |c, id| c.take_app_answer(id),
            |c, id| c.forget_app_request(id),
            deadline,
        )
        .await
    }

    /// The fleet skill requests before the thread open (MCP P3 §4.2), the
    /// same steps as the direct driver: `skills/extraRoots/set`, then
    /// `skills/list` with `forceReload`. A request waits for its answer at
    /// most [`CodexCore::SKILL_REQUEST_TIMEOUT`]. An error answer or a
    /// timeout is a warning and the open goes on. With less than two
    /// timeouts left of the handshake wait, both are skipped.
    async fn inject_skill(&self, dir: &Path) -> Result<(), DriverError> {
        let deadline = tokio::time::Instant::now() + OPEN_WAIT;
        if OPEN_WAIT < CodexCore::SKILL_REQUEST_TIMEOUT * 2 {
            tracing::warn!("skill injection skipped: handshake deadline");
            return Ok(());
        }
        self.skill_step("skills/extraRoots/set", deadline, |c| {
            c.set_skill_root(dir).map(split)
        })
        .await?;
        self.skill_step("skills/list", deadline, |c| c.reload_skills().map(split))
            .await
    }

    async fn skill_step(
        &self,
        request: &'static str,
        deadline: tokio::time::Instant,
        op: impl FnOnce(&mut CodexCore) -> Result<Steps, ProviderError>,
    ) -> Result<(), DriverError> {
        self.internal(op).await?;
        let until = deadline.min(tokio::time::Instant::now() + CodexCore::SKILL_REQUEST_TIMEOUT);
        match self
            .wait_until(|c| !c.is_skill_request_pending(request), until, "skills")
            .await
        {
            Err(DriverError::Handshake(_)) => {
                tracing::warn!(request, "the fleet skill request timed out");
                Ok(())
            }
            other => other,
        }
    }
}

/// The app operations of a Codex session under a shim: the same operations
/// as the direct driver (`codex/driver_apps.rs`) on top of the durable
/// driver's app exchange.
#[async_trait]
impl AppBridge for DurableDriver<CodexCore> {
    async fn tools(&self) -> Result<Vec<AppTool>, AppError> {
        let deadline = tokio::time::Instant::now() + TOOLS_TIMEOUT;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..TOOLS_PAGES {
            let op = CodexAppOp::Tools {
                cursor: cursor.take(),
            };
            let page = self
                .codex_app(op, Some(deadline))
                .await?
                .map_err(AppError::Answered)?;
            let (listed, next) = parse_tools_page(&page);
            tools.extend(listed);
            cursor = next;
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, AppError> {
        let op = CodexAppOp::Read {
            server: server.to_owned(),
            uri: uri.to_owned(),
        };
        let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
        self.codex_app(op, Some(deadline))
            .await?
            .map_err(AppError::Answered)
    }

    async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<ToolAnswer, AppError> {
        let op = CodexAppOp::Call {
            server: server.to_owned(),
            tool: tool.to_owned(),
            arguments,
        };
        let result = self.codex_app(op, None).await?;
        result.map(tool_answer).map_err(AppError::Answered)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use fleetdeck_proto::InputId;
    use fleetdeck_proto::command::UserInput;
    use fleetdeck_proto::event::RunOrigin;
    use fleetdeck_providers::adapter::HostInput;
    use fleetdeck_providers::codex::{ClientInfo, capabilities};
    use serde_json::json;

    use super::*;

    /// The id of the last request that `out` writes.
    fn last_id(out: &[Output]) -> Value {
        out.iter()
            .rev()
            .find_map(|o| match o {
                Output::Write(v) => Some(v["id"].clone()),
                _ => None,
            })
            .unwrap()
    }

    fn input(n: u8, text: &str) -> HostInput {
        HostInput {
            id: InputId::from(format!("01J0000000000000000000000{n}")),
            origin: RunOrigin::Coordinator,
            input: UserInput::new(text.to_owned()),
            context: Vec::new(),
        }
    }

    /// Review blocker 2: a delayed app call races a coordinator follow-up
    /// that the running turn only queues. The durable driver refuses it
    /// (`provider_busy`) and rolls the core back from the checkpoint of the
    /// time before it; the app request still waits, and its later answer
    /// reaches the call.
    #[test]
    fn a_delayed_app_call_survives_the_rollback_of_a_refused_follow_up() {
        let config = CoreConfig {
            provider_instance: "codex".into(),
            cwd: "/w".to_owned(),
            capabilities: capabilities(false),
            resumed: None,
            client: ClientInfo::fleetdeck(),
            request_models: false,
        };
        let mut core = <CodexCore as DurableCore>::create(config.clone());
        let out = core.initialize().unwrap();
        let reply =
            json!({"id": last_id(&out), "result": {"userAgent": "fleetdeck/0.160.0 (Linux)"}});
        DurableCore::line(&mut core, &reply.to_string());
        let out = core.open_thread(&SessionSpec::default()).unwrap();
        let reply =
            json!({"id": last_id(&out), "result": {"thread": {"id": "thr-1"}, "model": "m"}});
        DurableCore::line(&mut core, &reply.to_string());
        // A turn is requested: a later input only waits in the core.
        assert!(!core.prompt(&input(1, "hello")).unwrap().is_empty());
        let op = CodexAppOp::Call {
            server: "demo".to_owned(),
            tool: "bump".to_owned(),
            arguments: json!({}),
        };
        let (id, _) = core.app_request(&op).unwrap();
        let before = DurableCore::save(&core).unwrap();
        assert!(core.prompt(&input(2, "later")).unwrap().is_empty());
        super::super::durable::roll_back(&config, &mut core, &before).unwrap();
        let answer = json!({"id": id, "result": {"content": []}});
        DurableCore::line(&mut core, &answer.to_string());
        assert_eq!(core.take_app_answer(&id), Some(Ok(json!({"content": []}))));
    }
}
