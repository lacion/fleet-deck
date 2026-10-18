//! The state machine of `fleetdeck provision-bootstrap` (design
//! `omarchy-p8.md` 3.2): pure logic over [`Effects`].
//!
//! Stdout stays empty. Every stderr line goes through `Effects::progress`:
//! `provision: <phase>: <code>` with fixed words, or a fixed text.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::private_file::try_lock_private_below;
use fleetdeck_core::provision::ProvisionFile;
use fleetdeck_proto::link::VersionStatus;

use super::effects::{Effects, JoinOutcome, ServiceState, StationView, Terminal};
use super::release::{self, Manifest};
use super::retry::{Action, SHORT, classify};
use super::sha256_hex;
use super::state::{self, Phase, ProvisionState};
use crate::station_fingerprint::grouped;
use crate::tailnet_join::{check_name, parse_target};

/// The first answer of the station: every 500 ms, at most 60 s.
const STATION_POLL: Duration = Duration::from_millis(500);
const STATION_WAIT: Duration = Duration::from_secs(60);
/// The readiness of the link: every second, 60 s per round.
const LINK_POLL: Duration = Duration::from_secs(1);
const LINK_WAIT: Duration = Duration::from_secs(60);

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The bootstrap is complete.
    Complete,
    /// Another instance holds the lock.
    Busy,
    /// An unexpected failure.
    Unexpected,
    /// A retained terminal condition.
    Terminal,
}

impl Exit {
    /// The process exit code: 0, 0, 1, 2.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Complete | Self::Busy => 0,
            Self::Unexpected => 1,
            Self::Terminal => 2,
        }
    }
}

/// Why the sequence stopped before the end.
enum Stop {
    Terminal(Terminal),
    StateInvalid,
    Unexpected,
}

impl From<Terminal> for Stop {
    fn from(t: Terminal) -> Self {
        Self::Terminal(t)
    }
}

/// The validated provisioning file.
struct Input {
    file: ProvisionFile,
    host: String,
    port: u16,
    config_sha256: String,
}

struct Run<'a> {
    effects: &'a dyn Effects,
    root: PathBuf,
    state_path: PathBuf,
    state: Option<ProvisionState>,
    manifest: Option<Manifest>,
    notified: bool,
}

/// Runs the bootstrap once (the loops inside it wait for retryable
/// conditions). `retry` clears a retained terminal error first.
pub async fn run(effects: &dyn Effects, file: &Path, retry: bool) -> Exit {
    let mut run = Run {
        effects,
        root: effects.trust_root(),
        state_path: state::state_path(&effects.station_dir()),
        state: None,
        manifest: None,
        notified: false,
    };
    match run.execute(file, retry).await {
        Ok(exit) => exit,
        Err(stop) => run.finish(stop),
    }
}

/// `host:port` with brackets around an IPv6 address.
fn canonical(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

impl Run<'_> {
    fn line(&self, phase: &str, code: &str) {
        self.effects
            .progress(&format!("provision: {phase}: {code}"));
    }

    fn phase_word(&self) -> &'static str {
        self.state
            .as_ref()
            .map_or("validated", |state| state.phase.word())
    }

    /// The end of a run that stopped early. A terminal condition is saved
    /// first (the phase stays).
    fn finish(&mut self, stop: Stop) -> Exit {
        match stop {
            Stop::Terminal(t) => {
                if let Some(state) = self.state.as_mut() {
                    state.error = Some(t.0.to_owned());
                    if self.save().is_err() {
                        self.line(self.phase_word(), "unexpected_error");
                        return Exit::Unexpected;
                    }
                }
                self.line(self.phase_word(), t.0);
                Exit::Terminal
            }
            Stop::StateInvalid => {
                self.line(self.phase_word(), "state_invalid");
                Exit::Unexpected
            }
            Stop::Unexpected => {
                self.line(self.phase_word(), "unexpected_error");
                Exit::Unexpected
            }
        }
    }

    fn save(&self) -> Result<(), Stop> {
        let Some(state) = &self.state else {
            return Err(Stop::Unexpected);
        };
        state::save(&self.root, &self.state_path, state).map_err(|_| Stop::Unexpected)
    }

    fn state_mut(&mut self) -> Result<&mut ProvisionState, Stop> {
        self.state.as_mut().ok_or(Stop::Unexpected)
    }

    /// Records progress. A rerun walks the steps again from the start, so
    /// a phase never goes down here: a crash in a rerun keeps the furthest
    /// phase (the config-change rule of step 4 depends on it). Only the
    /// config-change restart of step 4 lowers the phase, directly.
    fn set_phase(&mut self, phase: Phase) -> Result<(), Stop> {
        let state = self.state_mut()?;
        state.phase = state.phase.max(phase);
        self.save()
    }

    fn complete(&mut self) -> Result<Exit, Stop> {
        let state = self.state_mut()?;
        state.phase = Phase::Complete;
        state.error = None;
        self.save()?;
        Ok(Exit::Complete)
    }

    async fn execute(&mut self, file: &Path, retry: bool) -> Result<Exit, Stop> {
        let effects = self.effects;
        // 1. The lock.
        effects.ensure_station_dir().map_err(|_| Stop::Unexpected)?;
        let lock = state::lock_path(&effects.station_dir());
        let Some(_lock) =
            try_lock_private_below(&self.root, &lock).map_err(|_| Stop::Unexpected)?
        else {
            return Ok(Exit::Busy);
        };
        // 2. The provisioning file.
        let input = Self::read_input(effects, file)?;
        // 3. The state.
        match state::load(&self.state_path).map_err(|_| Stop::StateInvalid)? {
            None => self.state = Some(ProvisionState::new(input.config_sha256.clone())),
            Some(stored) => {
                self.state = Some(stored);
                if let Some(error) = self.state_mut()?.error.clone() {
                    if !retry {
                        self.line(self.phase_word(), &error);
                        return Ok(Exit::Terminal);
                    }
                    self.state_mut()?.error = None;
                    self.save()?;
                }
            }
        }
        // 4. A changed file.
        let state = self.state_mut()?;
        if state.config_sha256 != input.config_sha256 {
            match state.phase {
                Phase::Validated | Phase::Service => {
                    state.config_sha256.clone_from(&input.config_sha256);
                    state.phase = Phase::Validated;
                }
                _ => return Err(Terminal::CONFIG_CHANGED.into()),
            }
        }
        // 5. The release prerequisite.
        self.manifest = Some(release::check(effects, &effects.build())?);
        if self.state_mut()?.phase == Phase::Complete && self.complete_checks(&input).await? {
            return Ok(Exit::Complete);
        }
        self.sequence(&input).await
    }

    /// Step 2: reads and validates the provisioning file.
    fn read_input(effects: &dyn Effects, path: &Path) -> Result<Input, Stop> {
        let bytes = effects.read_provision(path)?;
        let invalid = Terminal::FILE_INVALID;
        let file = fleetdeck_core::provision::parse(&bytes).map_err(|_| invalid)?;
        check_name(&file.name).map_err(|_| invalid)?;
        let (host, port) = parse_target(&file.deck).map_err(|_| invalid)?;
        if canonical(&host, port) != file.deck {
            return Err(invalid.into());
        }
        Ok(Input {
            config_sha256: sha256_hex(&bytes),
            file,
            host,
            port,
        })
    }

    /// The checks of a run whose stored phase is `Complete` (D20). `true`
    /// means complete; `false` runs the sequence from step 6.
    async fn complete_checks(&mut self, input: &Input) -> Result<bool, Stop> {
        let Ok(view) = self.effects.fingerprint(false).await else {
            return Ok(false);
        };
        self.identity(&view)?;
        match view.paired_deck.as_deref() {
            Some(deck) if deck == input.file.deck_fingerprint => {}
            Some(_) => return Err(Terminal::PAIRED_ELSEWHERE.into()),
            None => return Ok(false),
        }
        let status = self
            .effects
            .omarchy_status()
            .await
            .map_err(|_| Stop::Unexpected)?;
        if status != 0 {
            self.integration().await?;
            self.complete()?;
        }
        Ok(true)
    }

    /// The station key must be the stored one; the first one is stored.
    fn identity(&mut self, view: &StationView) -> Result<(), Stop> {
        let state = self.state_mut()?;
        match &state.station_fingerprint {
            Some(stored) if *stored != view.fingerprint => {
                Err(Terminal::STATION_IDENTITY_CHANGED.into())
            }
            Some(_) => Ok(()),
            None => {
                state.station_fingerprint = Some(view.fingerprint.clone());
                self.save()
            }
        }
    }

    /// Steps 6 to 13.
    async fn sequence(&mut self, input: &Input) -> Result<Exit, Stop> {
        // 6. The station configuration.
        match self.effects.station_config() {
            super::effects::ConfigState::Absent => self
                .effects
                .create_station_config(&input.file.name)
                .map_err(|_| Stop::Unexpected)?,
            super::effects::ConfigState::Named(n) if n != input.file.name => {
                return Err(Terminal::CONFIG_CONFLICT.into());
            }
            super::effects::ConfigState::Named(_) | super::effects::ConfigState::Unnamed => {}
            super::effects::ConfigState::Invalid => {
                return Err(Terminal::CONFIG_CONFLICT.into());
            }
        }
        self.set_phase(Phase::Service)?;
        // 7 and 8. The service and the station.
        let mut view = self.station(true).await?;
        // 9 and 10. The pairing and the link.
        loop {
            self.pair(input, view).await?;
            self.set_phase(Phase::Paired)?;
            match self.link(input).await? {
                None => break,
                Some(unpaired) => view = unpaired,
            }
        }
        self.set_phase(Phase::Link)?;
        // 11. The providers.
        self.providers().await?;
        self.set_phase(Phase::Integration)?;
        // 12 and 13. The integration.
        self.integration().await?;
        self.complete()
    }

    /// Step 7.
    fn service(&self) -> Result<(), Stop> {
        match self.effects.service() {
            ServiceState::Absent => self.effects.install_service().map_err(|_| Stop::Unexpected),
            ServiceState::Matching { active: false } => {
                self.effects.start_service().map_err(|_| Stop::Unexpected)
            }
            ServiceState::Matching { active: true } => Ok(()),
            ServiceState::Conflict => Err(Terminal::SERVICE_CONFLICT.into()),
        }
    }

    /// Steps 7 and 8, or step 8 alone: an answer of the station. No answer in
    /// 60 s waits 30 s and goes back to step 7.
    async fn station(&mut self, with_service: bool) -> Result<StationView, Stop> {
        let mut service = with_service;
        loop {
            if service {
                self.service()?;
            }
            service = true;
            if let Some(view) = self.poll_station().await? {
                return Ok(view);
            }
            self.line("service", "station_unavailable");
            self.effects.sleep(SHORT).await;
        }
    }

    /// Step 8: the poll, then the checks of the first answer.
    async fn poll_station(&mut self) -> Result<Option<StationView>, Stop> {
        let deadline = self.effects.now() + STATION_WAIT;
        loop {
            if let Ok(view) = self.effects.fingerprint(false).await {
                self.accept(&view)?;
                return Ok(Some(view));
            }
            if self.effects.now() >= deadline {
                return Ok(None);
            }
            self.effects.sleep(STATION_POLL).await;
        }
    }

    fn accept(&mut self, view: &StationView) -> Result<(), Stop> {
        let manifest = self.manifest.as_ref().ok_or(Stop::Unexpected)?;
        if !view.build.release
            || view.build.version != manifest.release_version
            || view.build.commit != manifest.commit
            || view.binary_sha256 != manifest.binary_sha256
        {
            return Err(Terminal::BUILD_MISMATCH.into());
        }
        self.identity(view)?;
        if !self.notified {
            self.notified = true;
            let text = grouped(&view.fingerprint);
            self.effects.notify(&text);
            self.effects.progress(&format!(
                "Approve this station fingerprint on the board: {text}"
            ));
        }
        Ok(())
    }

    /// Step 9. `view` is the last answer of the station.
    async fn pair(&mut self, input: &Input, mut view: StationView) -> Result<(), Stop> {
        let mut already_paired = 0_u32;
        loop {
            match view.paired_deck.as_deref() {
                Some(deck) if deck == input.file.deck_fingerprint => return Ok(()),
                Some(_) => return Err(Terminal::PAIRED_ELSEWHERE.into()),
                None => {}
            }
            let outcome = self
                .effects
                .join(
                    &input.host,
                    input.port,
                    &input.file.deck_fingerprint,
                    &input.file.name,
                )
                .await;
            match outcome {
                JoinOutcome::Paired => return Ok(()),
                JoinOutcome::Code(code) => match classify(&code) {
                    Action::RetryAfter(wait) => {
                        already_paired = 0;
                        self.line("pairing", &code);
                        self.effects.sleep(wait).await;
                    }
                    Action::AlreadyPaired => {
                        already_paired += 1;
                        if already_paired >= 2 {
                            self.line("pairing", "already_paired");
                            self.effects.sleep(SHORT).await;
                        }
                    }
                    Action::Stop => return Err(Terminal::from_code(&code).into()),
                },
            }
            view = self.station(false).await?;
        }
    }

    /// Step 10: `None` when the link is up; `Some` is an answer that shows no
    /// pairing (back to step 9).
    async fn link(&mut self, input: &Input) -> Result<Option<StationView>, Stop> {
        loop {
            let deadline = self.effects.now() + LINK_WAIT;
            loop {
                if let Ok(view) = self.effects.fingerprint(false).await {
                    self.identity(&view)?;
                    match view.paired_deck.as_deref() {
                        Some(deck) if deck == input.file.deck_fingerprint => {}
                        Some(_) => return Err(Terminal::PAIRED_ELSEWHERE.into()),
                        None => return Ok(Some(view)),
                    }
                    if view.link_up {
                        return Ok(None);
                    }
                }
                if self.effects.now() >= deadline {
                    break;
                }
                self.effects.sleep(LINK_POLL).await;
            }
            self.effects.sleep(SHORT).await;
        }
    }

    /// Step 11.
    async fn providers(&mut self) -> Result<(), Stop> {
        loop {
            let Ok(view) = self.effects.fingerprint(true).await else {
                self.effects.sleep(SHORT).await;
                continue;
            };
            let Some(providers) = view.providers else {
                return Err(Terminal::PROVIDER_PROBE_FAILED.into());
            };
            for provider in &providers {
                if provider.version.is_none()
                    || matches!(
                        provider.version_status,
                        VersionStatus::Broken | VersionStatus::Unknown
                    )
                {
                    return Err(Terminal::PROVIDER_PROBE_FAILED.into());
                }
                if provider.auth == "signed_out" {
                    self.effects.progress(&format!(
                        "provision: providers: signed_out {}",
                        provider.instance
                    ));
                }
            }
            if providers.is_empty() {
                self.effects.progress("No providers are configured.");
            }
            return Ok(());
        }
    }

    /// Step 12.
    async fn integration(&mut self) -> Result<(), Stop> {
        let installed = self
            .effects
            .omarchy_install()
            .await
            .map_err(|_| Stop::Unexpected)?;
        if installed != 0 {
            return Err(Terminal::INTEGRATION_FAILED.into());
        }
        let status = self
            .effects
            .omarchy_status()
            .await
            .map_err(|_| Stop::Unexpected)?;
        if status != 0 {
            return Err(Terminal::INTEGRATION_FAILED.into());
        }
        Ok(())
    }
}
