//! Model help on the station (R10, DL-167): small, cheap, optional text from
//! the owner's own provider login.
//!
//! - [`Assist::generate`] is the one entry (`assist.generate`): it checks the
//!   switch, builds the prompt ([`prompts`]), answers from the cache when the
//!   same input was answered before, refuses at the daily cap, runs one
//!   headless CLI call ([`runner`]) and stores the text and the tokens.
//! - Nothing depends on it. Every caller has a plain fallback: no title, no
//!   digest, no sentence. An error here is never shown as an error state.
//! - [`watch`] names a session after its first turn and summarizes a session
//!   when it ends.

pub mod input;
pub mod prompts;
pub mod runner;
pub mod watch;

use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::{Driver, ModelHelpConfig};
use fleetdeck_proto::assist::{AssistAnswer, AssistKind, MAX_INPUT_CHARS, MAX_OUTPUT_CHARS};
use fleetdeck_proto::command::MAX_TITLE_CHARS;
use fleetdeck_proto::link::StationProvider;
use fleetdeck_proto::{Event, ProviderInstanceId, SessionId};
use ring::digest::{SHA256, digest};
use tokio::sync::Semaphore;

use crate::store::{Store, StoreError};
use runner::{Call, CliRunner, Runner};

/// The calls that run at the same time.
const CONCURRENT_CALLS: usize = 2;

/// A provider instance that the model help can call.
#[derive(Debug, Clone)]
pub struct AssistInstance {
    pub id: ProviderInstanceId,
    pub driver: Driver,
    pub binary: PathBuf,
    pub env: ChildEnv,
}

/// Why a call gave no text. [`Self::code`] is the wire error code of
/// `assist.generate`.
#[derive(Debug, thiserror::Error)]
pub enum AssistError {
    #[error("model help is switched off in station.toml")]
    Disabled,
    #[error("the daily token cap of the model help is reached")]
    Budget,
    #[error("no signed-in provider instance can run the model help")]
    NoProvider,
    #[error("{0}")]
    InvalidParams(String),
    #[error("the model call failed: {0}")]
    Failed(String),
}

impl AssistError {
    /// The wire code of the error.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Budget => "budget",
            Self::NoProvider => "no_provider",
            Self::InvalidParams(_) => "invalid_params",
            Self::Failed(_) => "failed",
        }
    }
}

impl From<StoreError> for AssistError {
    fn from(e: StoreError) -> Self {
        Self::Failed(e.to_string())
    }
}

/// The model help of one station.
#[derive(Debug)]
pub struct Assist {
    config: ModelHelpConfig,
    store: Store,
    instances: Vec<AssistInstance>,
    runner: Arc<dyn Runner>,
    slots: Semaphore,
}

impl Assist {
    /// The model help with the real CLI runner.
    #[must_use]
    pub fn new(config: ModelHelpConfig, store: Store, instances: Vec<AssistInstance>) -> Self {
        Self::with_runner(config, store, instances, Arc::new(CliRunner))
    }

    /// The model help with another runner (the tests use a fake).
    #[must_use]
    pub fn with_runner(
        config: ModelHelpConfig,
        store: Store,
        instances: Vec<AssistInstance>,
        runner: Arc<dyn Runner>,
    ) -> Self {
        Self {
            config,
            store,
            instances,
            runner,
            slots: Semaphore::new(CONCURRENT_CALLS),
        }
    }

    /// False when `[model_help] enabled = false`.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// The daily token cap.
    #[must_use]
    pub fn cap(&self) -> u64 {
        self.config.daily_token_cap
    }

    /// The tokens used today and the cap, for `station.info`; `None` when
    /// the model help is off.
    pub async fn usage(&self) -> Option<(u64, u64)> {
        if !self.enabled() {
            return None;
        }
        match self.store.assist_tokens_today().await {
            Ok(tokens) => Some((tokens, self.cap())),
            Err(e) => {
                tracing::warn!(error = %e, "model help usage");
                None
            }
        }
    }

    /// The instance that runs a call: the first one of the configured
    /// driver, else the first one that is signed in.
    fn pick(&self, providers: &[StationProvider]) -> Option<&AssistInstance> {
        match self.config.provider {
            Some(driver) => self.instances.iter().find(|i| i.driver == driver),
            None => self.instances.iter().find(|i| {
                providers
                    .iter()
                    .any(|p| p.instance == i.id && p.auth == "signed_in")
            }),
        }
    }

    fn model(&self, driver: Driver) -> &str {
        match driver {
            Driver::Claude => &self.config.claude_model,
            Driver::Codex => &self.config.codex_model,
        }
    }

    /// `assist.generate`. For [`AssistKind::Digest`] the input is a session
    /// id: the station builds the text from the session's last timeline
    /// lines and stores the digest with `session.digest`.
    ///
    /// # Errors
    /// [`AssistError`]; the caller keeps its plain fallback.
    pub async fn generate(
        &self,
        kind: AssistKind,
        input: &str,
        providers: &[StationProvider],
    ) -> Result<AssistAnswer, AssistError> {
        if !self.enabled() {
            return Err(AssistError::Disabled);
        }
        if kind == AssistKind::Digest {
            return self.digest(input.trim(), providers).await;
        }
        self.text(kind, input, providers).await
    }

    /// R10.2: the digest of one session.
    async fn digest(
        &self,
        session_id: &str,
        providers: &[StationProvider],
    ) -> Result<AssistAnswer, AssistError> {
        let session = SessionId::from(session_id);
        let known = self.store.summary(session.clone()).await?;
        let Some(summary) = known else {
            return Err(AssistError::InvalidParams(format!(
                "no session `{session_id}`"
            )));
        };
        let events = self
            .store
            .assist_session_events(session.clone(), 300)
            .await?;
        let lines = input::digest_input(&events);
        if lines.is_empty() {
            return Err(AssistError::InvalidParams(
                "the session has nothing to summarize".to_owned(),
            ));
        }
        let answer = self.text(AssistKind::Digest, &lines, providers).await?;
        if summary.digest.as_deref() != Some(answer.text.as_str()) {
            let event = Event::SessionDigest {
                text: answer.text.clone(),
            };
            self.store.append(session, vec![event.into()]).await?;
        }
        Ok(answer)
    }

    /// The text of one prompt: cache, budget, call, store.
    async fn text(
        &self,
        kind: AssistKind,
        input: &str,
        providers: &[StationProvider],
    ) -> Result<AssistAnswer, AssistError> {
        if input.trim().is_empty() || input.contains('\0') {
            return Err(AssistError::InvalidParams(
                "the input is empty or has a NUL character".to_owned(),
            ));
        }
        if input.chars().count() > MAX_INPUT_CHARS {
            return Err(AssistError::InvalidParams(format!(
                "the input has more than {MAX_INPUT_CHARS} characters"
            )));
        }
        let instance = self.pick(providers).ok_or(AssistError::NoProvider)?;
        let model = self.model(instance.driver).to_owned();
        let prompt = prompts::build(kind, input);
        let hash = input_hash(kind, &model, &prompt);
        if let Some(hit) = self.store.assist_cache_get(hash.clone()).await? {
            return Ok(answer(hit.output, true));
        }
        if self.store.assist_tokens_today().await? >= self.cap() {
            return Err(AssistError::Budget);
        }
        let env = match instance.driver {
            Driver::Claude => {
                runner::claude_env(&instance.env, self.config.per_call_max_output_tokens)
            }
            Driver::Codex => instance.env.clone(),
        };
        let call = Call {
            driver: instance.driver,
            binary: instance.binary.clone(),
            env,
            model,
            prompt,
            max_output_tokens: self.config.per_call_max_output_tokens,
        };
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|e| AssistError::Failed(e.to_string()))?;
        let run = self
            .runner
            .run(call)
            .await
            .map_err(|e| AssistError::Failed(e.to_string()))?;
        let max_chars = MAX_OUTPUT_CHARS.min(self.config.per_call_max_output_tokens as usize * 4);
        let text = clean(kind, &run.text, max_chars)
            .ok_or_else(|| AssistError::Failed("the model gave no usable text".to_owned()))?;
        self.store
            .assist_record(
                hash,
                kind.as_str().to_owned(),
                text.clone(),
                run.input_tokens,
                run.output_tokens,
            )
            .await?;
        Ok(answer(text, false))
    }
}

fn answer(text: String, cached: bool) -> AssistAnswer {
    AssistAnswer {
        text,
        generated: true,
        cached,
    }
}

/// The cache key: the SHA-256 of the kind, the model and the prompt.
#[must_use]
pub fn input_hash(kind: AssistKind, model: &str, prompt: &str) -> String {
    let joined = format!("{}\0{model}\0{prompt}", kind.as_str());
    digest(&SHA256, joined.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Strips the decoration that a model adds and applies the limits of the
/// kind. `None` when nothing usable is left.
#[must_use]
pub fn clean(kind: AssistKind, raw: &str, max_chars: usize) -> Option<String> {
    let text = raw.trim();
    // The plan permission mode can answer with its own notice.
    if text.to_lowercase().contains("plan mode is active") {
        return None;
    }
    let text = match kind {
        AssistKind::Title => title(text)?,
        _ => {
            let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
            lines.join("\n").trim().to_owned()
        }
    };
    if text.is_empty() {
        return None;
    }
    Some(
        text.chars()
            .take(max_chars)
            .collect::<String>()
            .trim_end()
            .to_owned(),
    )
}

/// One line of at most [`prompts::TITLE_WORDS`] words and
/// [`MAX_TITLE_CHARS`] characters, without quotes or a final period.
fn title(text: &str) -> Option<String> {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let first = first
        .strip_prefix("Title:")
        .or_else(|| first.strip_prefix("title:"))
        .unwrap_or(first);
    let trimmed = first
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '*' | '#' | '.' | '“' | '”'))
        .trim();
    let words: Vec<&str> = trimmed
        .split_whitespace()
        .take(prompts::TITLE_WORDS)
        .collect();
    let joined: String = words.join(" ");
    let cut: String = joined.chars().take(MAX_TITLE_CHARS).collect();
    let cut = cut.trim().to_owned();
    (!cut.is_empty() && !cut.chars().any(char::is_control)).then_some(cut)
}

#[cfg(test)]
mod tests;
