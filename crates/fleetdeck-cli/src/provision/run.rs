//! The state machine of `fleetdeck provision-bootstrap` (design
//! `omarchy-p8.md` 3.2): pure logic over [`Effects`].
//!
//! Stdout stays empty. Every stderr line goes through `Effects::progress`:
//! `provision: <phase>: <code>` with fixed words, or a fixed text.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::private_file::try_lock_private_below;
use fleetdeck_core::provision::ProvisionFile;
use fleetdeck_proto::link::VersionStatus;

use super::binding::{self, Authorization};
use super::effects::{Effects, JoinOutcome, ServiceState, StationView, Terminal};
use super::release::{self, Manifest};
use super::retry::{Action, SHORT, classify};
use super::run_checks::{IntegrationStatus, build_matches};
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
    /// A retry authorization that cannot be used: the word after
    /// `provision: retry:`. Nothing is saved.
    RetryRefused(&'static str),
    StateInvalid,
    Unexpected,
}

impl From<Terminal> for Stop {
    fn from(t: Terminal) -> Self {
        Self::Terminal(t)
    }
}

/// The validated provisioning file.
pub(super) struct Input {
    pub(super) file: ProvisionFile,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) config_sha256: String,
}

/// The provisioning file as the supervisor needs it before a report: `None`
/// when it does not pass the rules of step 2.
pub(super) fn read_provision_input(effects: &dyn Effects, path: &Path) -> Option<Input> {
    Run::read_input(effects, path).ok()
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
/// `authorization` is the path of a retry authorization of a report (design
/// `board-gaps.md` 4.3): the run checks it under the lock and consumes it
/// with one write of the state, then goes on as a `retry` run.
pub async fn run(
    effects: &dyn Effects,
    file: &Path,
    retry: bool,
    authorization: Option<&Path>,
) -> Exit {
    let mut run = Run {
        effects,
        root: effects.trust_root(),
        state_path: state::state_path(&effects.station_dir()),
        state: None,
        manifest: None,
        notified: false,
    };
    // The guard stays until the function ends: `execute` and `finish` (the
    // save of a terminal condition) both run under the lock.
    let _lock = match run.lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => return Exit::Busy,
        Err(stop) => return run.finish(stop),
    };
    match run.execute(file, retry, authorization).await {
        Ok(exit) => exit,
        Err(stop) => run.finish(stop),
    }
}

/// Where the run goes next in steps 9 to 13.
enum Next {
    Pair(StationView),
    Link,
    Providers,
    Integration,
    Final,
}

/// The last read of the station before `Complete`.
enum Last {
    Done,
    Unpaired(StationView),
    LinkDown,
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
            Stop::RetryRefused(code) => {
                self.line("retry", code);
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

    /// Step 1: the lock. `None` when another instance holds it.
    fn lock(&self) -> Result<Option<File>, Stop> {
        self.effects
            .ensure_station_dir()
            .map_err(|_| Stop::Unexpected)?;
        let lock = state::lock_path(&self.effects.station_dir());
        try_lock_private_below(&self.root, &lock).map_err(|_| Stop::Unexpected)
    }

    async fn execute(
        &mut self,
        file: &Path,
        retry: bool,
        authorization: Option<&Path>,
    ) -> Result<Exit, Stop> {
        let effects = self.effects;
        // 2. The provisioning file.
        let input = Self::read_input(effects, file)?;
        // 3. The state. An authorization clears the retained error first.
        if let Some(path) = authorization {
            self.authorize(path, &input)?;
        }
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
        let manifest = release::check(effects, &effects.build())?;
        effects.set_trusted_binary(&manifest.binary_sha256);
        self.manifest = Some(manifest);
        if self.state_mut()?.phase == Phase::Complete && self.complete_checks(&input).await? {
            return Ok(Exit::Complete);
        }
        self.sequence(&input).await
    }

    /// Step 3, with a retry authorization: the authorization must be for the
    /// exact terminal condition that the state keeps now. One atomic write
    /// of the state clears the error and records the `retry_id`. A mismatch
    /// or an unusable authorization changes nothing (`report_changed`), and
    /// the refused file is deleted. A stop of the state load or save keeps
    /// the file.
    fn authorize(&mut self, path: &Path, input: &Input) -> Result<(), Stop> {
        let changed = || self.refuse(path, "report_changed");
        let bytes = self
            .effects
            .read_authorization(path)
            .map_err(|_| changed())?;
        let auth = Authorization::parse(&bytes).ok_or_else(changed)?;
        let mut state = state::load(&self.state_path)
            .map_err(|_| Stop::StateInvalid)?
            .ok_or_else(changed)?;
        let error = state.error.clone().ok_or_else(changed)?;
        let current = binding::digest(
            &error,
            state.phase,
            &input.config_sha256,
            &input.file.deck_fingerprint,
            state.station_fingerprint.as_deref(),
        );
        let stored = binding::load(&binding::binding_path(&self.effects.station_dir()))
            .ok_or_else(changed)?;
        if stored.terminal_id != auth.terminal_id
            || stored.state_digest != auth.state_digest
            || stored.state_digest != current
        {
            return Err(changed());
        }
        if state.consumed_retry_id.as_deref() == Some(auth.retry_id.as_str()) {
            return Err(self.refuse(path, "retry_replayed"));
        }
        state.error = None;
        state.consumed_retry_id = Some(auth.retry_id);
        state::save(&self.root, &self.state_path, &state).map_err(|_| Stop::Unexpected)?;
        // A crash before the delete is harmless: the next use is a replay.
        if self.effects.delete_authorization(path).is_err() {
            self.line("retry", "authorization_not_removed");
        }
        Ok(())
    }

    /// A definite refusal of an authorization: the file is deleted (a failure
    /// is a diagnostic line; a file that is already gone is not) and the
    /// refusal stays the same.
    fn refuse(&self, path: &Path, code: &'static str) -> Stop {
        if let Err(e) = self.effects.delete_authorization(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            self.line("retry", "authorization_not_removed");
        }
        Stop::RetryRefused(code)
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
        if !self.check_view(input, &view)? {
            return Ok(false);
        }
        let (code, bytes) = self
            .effects
            .omarchy_status_json()
            .await
            .map_err(|_| Stop::Unexpected)?;
        let healthy = code == 0 && IntegrationStatus::parse(&bytes).is_some_and(|s| s.healthy());
        if !healthy {
            self.integration().await?;
            match self.last_read(input).await? {
                Last::Done => {
                    self.complete()?;
                }
                Last::Unpaired(_) | Last::LinkDown => return Ok(false),
            }
        }
        Ok(true)
    }

    /// The checks of every answer of the station that the run reads: the
    /// build and the key (`check_build_identity`), then the pairing. `true`
    /// when the station is paired with the deck of the file; `false` when it
    /// shows no pairing (the caller goes back to the pairing step).
    fn check_view(&mut self, input: &Input, view: &StationView) -> Result<bool, Stop> {
        self.check_build_identity(view)?;
        match view.paired_deck.as_deref() {
            Some(deck) if deck == input.file.deck_fingerprint => Ok(true),
            Some(_) => Err(Terminal::PAIRED_ELSEWHERE.into()),
            None => Ok(false),
        }
    }

    /// The build and the binary hash are the manifest's; the station key is
    /// the stored one.
    fn check_build_identity(&mut self, view: &StationView) -> Result<(), Stop> {
        let manifest = self.manifest.as_ref().ok_or(Stop::Unexpected)?;
        if !build_matches(view, manifest) {
            return Err(Terminal::BUILD_MISMATCH.into());
        }
        self.identity(view)
    }

    /// The read before `Complete`: the checks of every answer, and the link
    /// up. A failed read is repeated after 30 s.
    async fn last_read(&mut self, input: &Input) -> Result<Last, Stop> {
        loop {
            let Ok(view) = self.effects.fingerprint(false).await else {
                self.effects.sleep(SHORT).await;
                continue;
            };
            if !self.check_view(input, &view)? {
                return Ok(Last::Unpaired(view));
            }
            return Ok(if view.link_up {
                Last::Done
            } else {
                Last::LinkDown
            });
        }
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
        let view = self.station(true).await?;
        // 9 to 13. An answer that shows no pairing goes back to step 9; a
        // link that is down before the end goes back to step 10.
        let mut next = Next::Pair(view);
        loop {
            next = match next {
                Next::Pair(view) => {
                    self.pair(input, view).await?;
                    self.set_phase(Phase::Paired)?;
                    Next::Link
                }
                Next::Link => match self.link(input).await? {
                    Some(unpaired) => Next::Pair(unpaired),
                    None => {
                        self.set_phase(Phase::Link)?;
                        Next::Providers
                    }
                },
                Next::Providers => match self.providers(input).await? {
                    Some(unpaired) => Next::Pair(unpaired),
                    None => {
                        self.set_phase(Phase::Integration)?;
                        Next::Integration
                    }
                },
                Next::Integration => {
                    self.integration().await?;
                    Next::Final
                }
                Next::Final => match self.last_read(input).await? {
                    Last::Done => return self.complete(),
                    Last::Unpaired(view) => Next::Pair(view),
                    Last::LinkDown => Next::Link,
                },
            };
        }
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
        self.check_build_identity(view)?;
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
                    if !self.check_view(input, &view)? {
                        return Ok(Some(view));
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

    /// Step 11: `None` when the providers are checked; `Some` is an answer
    /// that shows no pairing (back to step 9).
    async fn providers(&mut self, input: &Input) -> Result<Option<StationView>, Stop> {
        loop {
            let Ok(view) = self.effects.fingerprint(true).await else {
                self.effects.sleep(SHORT).await;
                continue;
            };
            if !self.check_view(input, &view)? {
                return Ok(Some(view));
            }
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
            return Ok(None);
        }
    }

    /// Step 12: the install and the status, again every 30 s while the
    /// plugin is not enabled (the Omarchy shell can answer late after the
    /// first login).
    async fn integration(&mut self) -> Result<(), Stop> {
        loop {
            let installed = self
                .effects
                .omarchy_install()
                .await
                .map_err(|_| Stop::Unexpected)?;
            if installed != 0 {
                return Err(Terminal::INTEGRATION_FAILED.into());
            }
            let (code, bytes) = self
                .effects
                .omarchy_status_json()
                .await
                .map_err(|_| Stop::Unexpected)?;
            let status = IntegrationStatus::parse(&bytes)
                .filter(|status| code == 0 && status.files_clean())
                .ok_or(Terminal::INTEGRATION_FAILED)?;
            if !status.plugin_enabled() {
                self.line("integration", "plugin_not_enabled");
                self.effects.sleep(SHORT).await;
                continue;
            }
            // The owner's `hyprland.lua` can be a link or not ours (the P4
            // fallback): not a failure of the bootstrap.
            if status.hyprland_line_missing() {
                self.line("integration", "hyprland_line_missing");
            }
            return Ok(());
        }
    }
}
