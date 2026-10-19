//! A scripted fake of [`Effects`] with a virtual clock: `sleep` advances the
//! clock and never waits. The state files are real files in a private
//! temporary root.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{StationProvider, VersionStatus};

use fleetdeck_core::private_file::read_private;
use fleetdeck_link::{ReportReply, ValidReport};

use super::effects::{
    BoxFuture, Build, CallError, ConfigState, Effects, JoinOutcome, ReportFailure, RootFile,
    ServiceState, StationView, Terminal,
};
use super::release::{DESKTOP_PATH, HOOK_PATH, MANIFEST_PATH};
use super::sha256_hex;
use super::state::{self, ProvisionState};

pub const DECK_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const OTHER_FP: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
pub const STATION_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const STATION_FP_2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const HOOK_BYTES: &[u8] = b"#!/bin/sh\nexit 0\n";
pub const DESKTOP_BYTES: &[u8] = b"[Desktop Entry]\nNoDisplay=true\n";

/// What `join` does the next time.
#[derive(Debug, Clone, Copy)]
pub enum JoinStep {
    /// The deck pairs.
    Paired,
    /// A failure code; nothing is paired.
    Code(&'static str),
    /// A failure code after the deck committed the pairing.
    CodeCommitted(&'static str),
}

/// What `report` does the next time.
pub enum ReportStep {
    /// This reply.
    Reply(ReportReply),
    /// A `retry` reply for the exact binding of the report that arrives.
    RetryFor(&'static str),
    /// A `retry` reply for a binding that is not the current one.
    StaleRetry,
    /// No reply: this fixed word.
    Fail(&'static str),
}

/// A function that runs once after the authorization is read.
pub type AuthHook = Box<dyn FnOnce() + Send>;

/// A function that runs once, at the first sleep (the sleep is recorded).
pub type SleepHook = Box<dyn FnOnce(&Fake) + Send>;

pub struct Station {
    pub fingerprint: String,
    pub up_at: Duration,
    pub paired: Option<String>,
    pub link_script: VecDeque<bool>,
    pub link_default: bool,
    pub build: Build,
    pub binary_sha256: String,
    pub providers: Option<Vec<StationProvider>>,
    pub probe_errors: u32,
    /// One edit of the station per answer without a probe (before it is built).
    pub edits: VecDeque<Edit>,
    /// One edit of the station per answer with a probe.
    pub probe_edits: VecDeque<Edit>,
    /// The error of a call while the station is not up.
    pub down_error: CallError,
}

pub type Edit = Box<dyn FnOnce(&mut Station) + Send>;
/// One answer of `omarchy status --json`: the exit code and the stdout.
pub type StatusAnswer = (i32, Vec<u8>);
/// A function that sees every stderr line.
pub type ProgressHook = Box<dyn Fn(&str) + Send>;

pub struct Fake {
    pub dir: tempfile::TempDir,
    base: Instant,
    pub clock: Mutex<Duration>,
    pub calls: Mutex<Vec<String>>,
    pub sleeps: Mutex<Vec<Duration>>,
    pub stderr: Mutex<Vec<String>>,
    pub notices: Mutex<Vec<String>>,
    pub provision: Mutex<Result<Vec<u8>, Terminal>>,
    pub files: Mutex<HashMap<PathBuf, io::Result<RootFile>>>,
    pub own_sha256: Mutex<Option<String>>,
    pub build: Mutex<Build>,
    pub config: Mutex<ConfigState>,
    pub service: Mutex<ServiceState>,
    pub station: Mutex<Station>,
    pub joins: Mutex<VecDeque<JoinStep>>,
    pub join_args: Mutex<Vec<(String, u16, String, String)>>,
    pub install: Mutex<VecDeque<io::Result<i32>>>,
    pub status: Mutex<VecDeque<io::Result<StatusAnswer>>>,
    pub trusted: Mutex<Option<String>>,
    pub progress_hooks: Mutex<Vec<ProgressHook>>,
    pub dir_error: Mutex<bool>,
    /// The key of the station can be loaded.
    pub identity: Mutex<bool>,
    pub report_steps: Mutex<VecDeque<ReportStep>>,
    /// The reports that arrived: the host, the port, the pin, the report.
    pub reports: Mutex<Vec<(String, u16, String, ValidReport)>>,
    /// What `jitter` returns (at most the maximum).
    pub jitter: Mutex<Duration>,
    pub authorization_deleted: Mutex<Vec<PathBuf>>,
    pub fail_delete: Mutex<bool>,
    pub after_auth_read: Mutex<Option<AuthHook>>,
    pub on_first_sleep: Mutex<Option<SleepHook>>,
    /// After this many sleeps the next run fails with `Unexpected` (the
    /// directory of the station gives an error), which ends a supervisor.
    pub stop_after_sleeps: Mutex<Option<usize>>,
}

pub fn build() -> Build {
    Build {
        version: "2.0.0".to_owned(),
        commit: "abc1234".to_owned(),
        release: true,
    }
}

/// The answer of `omarchy status --json`: the exit code and the stdout.
pub fn status_json(
    code: i32,
    installed: bool,
    files: &[&str],
    plugin: bool,
    line: &str,
) -> StatusAnswer {
    let files: Vec<String> = files
        .iter()
        .map(|state| format!(r#"{{"path":"/home/u/.config/f","state":"{state}"}}"#))
        .collect();
    let text = format!(
        r#"{{"installed":{installed},"files":[{}],"plugin_enabled":{plugin},"summary":"on","hyprland_line":"{line}"}}"#,
        files.join(",")
    );
    (code, text.into_bytes())
}

/// Installed, every file ok, plugin enabled, Hyprland line present.
pub fn healthy_status() -> StatusAnswer {
    status_json(0, true, &["ok", "ok"], true, "ok")
}

pub fn binary_sha() -> String {
    sha256_hex(b"binary")
}

pub fn provider(
    instance: &str,
    version: Option<&str>,
    auth: &str,
    st: VersionStatus,
) -> StationProvider {
    StationProvider {
        instance: instance.into(),
        provider: ProviderKind::Claude,
        version: version.map(str::to_owned),
        auth: auth.to_owned(),
        capabilities: None,
        version_status: st,
    }
}

pub fn manifest_json() -> String {
    format!(
        r#"{{"version":1,"release_version":"2.0.0","commit":"abc1234","package_sha256":"{p}","binary_sha256":"{b}","hooks":{{"provision_first_login_sha256":"{h}","autostart_desktop_sha256":"{d}"}}}}"#,
        p = sha256_hex(b"package"),
        b = binary_sha(),
        h = sha256_hex(HOOK_BYTES),
        d = sha256_hex(DESKTOP_BYTES),
    )
}

pub fn root_file(bytes: &[u8], mode: u32) -> RootFile {
    RootFile {
        bytes: bytes.to_vec(),
        uid: 0,
        mode,
        symlink: false,
        parent_uid: 0,
        parent_mode: 0o755,
    }
}

/// A provisioning file for the default deck.
pub fn file_text(name: &str) -> Vec<u8> {
    format!(
        "version = 1\ndeck = \"deck.example.ts.net:7421\"\ndeck_fingerprint = \"{DECK_FP}\"\nname = \"{name}\"\n"
    )
    .into_bytes()
}

impl Fake {
    /// A machine where the whole bootstrap succeeds on the first try.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from(MANIFEST_PATH),
            Ok(root_file(manifest_json().as_bytes(), 0o444)),
        );
        files.insert(PathBuf::from(HOOK_PATH), Ok(root_file(HOOK_BYTES, 0o755)));
        files.insert(
            PathBuf::from(DESKTOP_PATH),
            Ok(root_file(DESKTOP_BYTES, 0o644)),
        );
        Self {
            dir,
            base: Instant::now(),
            clock: Mutex::new(Duration::ZERO),
            calls: Mutex::new(Vec::new()),
            sleeps: Mutex::new(Vec::new()),
            stderr: Mutex::new(Vec::new()),
            notices: Mutex::new(Vec::new()),
            provision: Mutex::new(Ok(file_text("laptop"))),
            files: Mutex::new(files),
            own_sha256: Mutex::new(Some(binary_sha())),
            build: Mutex::new(build()),
            config: Mutex::new(ConfigState::Absent),
            service: Mutex::new(ServiceState::Absent),
            station: Mutex::new(Station {
                fingerprint: STATION_FP.to_owned(),
                up_at: Duration::ZERO,
                paired: None,
                link_script: VecDeque::new(),
                link_default: true,
                build: build(),
                binary_sha256: binary_sha(),
                providers: Some(Vec::new()),
                probe_errors: 0,
                edits: VecDeque::new(),
                probe_edits: VecDeque::new(),
                down_error: CallError::Unavailable,
            }),
            joins: Mutex::new(VecDeque::from([JoinStep::Paired])),
            join_args: Mutex::new(Vec::new()),
            install: Mutex::new(VecDeque::new()),
            status: Mutex::new(VecDeque::new()),
            trusted: Mutex::new(None),
            progress_hooks: Mutex::new(Vec::new()),
            dir_error: Mutex::new(false),
            identity: Mutex::new(true),
            report_steps: Mutex::new(VecDeque::new()),
            reports: Mutex::new(Vec::new()),
            jitter: Mutex::new(Duration::ZERO),
            authorization_deleted: Mutex::new(Vec::new()),
            fail_delete: Mutex::new(false),
            after_auth_read: Mutex::new(None),
            on_first_sleep: Mutex::new(None),
            stop_after_sleeps: Mutex::new(None),
        }
    }

    pub fn state(&self) -> Option<ProvisionState> {
        state::load(&state::state_path(&self.station_dir())).unwrap()
    }

    pub fn state_file(&self) -> PathBuf {
        state::state_path(&self.station_dir())
    }

    pub fn log(&self, entry: impl Into<String>) {
        self.calls.lock().unwrap().push(entry.into());
    }

    pub fn count(&self, entry: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == entry)
            .count()
    }

    pub fn lines(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }

    pub fn elapsed(&self) -> Duration {
        *self.clock.lock().unwrap()
    }

    pub fn push_join(&self, step: JoinStep) {
        self.joins.lock().unwrap().push_back(step);
    }

    pub fn set_join(&self, steps: &[JoinStep]) {
        *self.joins.lock().unwrap() = steps.iter().copied().collect();
    }

    pub fn set_paired(&self, deck: Option<&str>) {
        self.station.lock().unwrap().paired = deck.map(str::to_owned);
    }

    pub fn set_file(&self, path: &str, file: io::Result<RootFile>) {
        self.files.lock().unwrap().insert(PathBuf::from(path), file);
    }

    pub fn edit_file(&self, path: &str, edit: impl FnOnce(&mut RootFile)) {
        let mut files = self.files.lock().unwrap();
        let file = files.get_mut(Path::new(path)).unwrap().as_mut().unwrap();
        edit(file);
    }
}

impl Effects for Fake {
    fn station_dir(&self) -> PathBuf {
        self.dir.path().join("station")
    }

    fn trust_root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn ensure_station_dir(&self) -> io::Result<()> {
        if *self.dir_error.lock().unwrap() {
            return Err(io::Error::other("no directory"));
        }
        match std::fs::DirBuilder::new()
            .mode(0o700)
            .create(self.station_dir())
        {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
            _ => Ok(()),
        }
    }

    fn now(&self) -> Instant {
        self.base + *self.clock.lock().unwrap()
    }

    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            *self.clock.lock().unwrap() += d;
            let slept = {
                let mut sleeps = self.sleeps.lock().unwrap();
                sleeps.push(d);
                sleeps.len()
            };
            if self
                .stop_after_sleeps
                .lock()
                .unwrap()
                .is_some_and(|n| slept >= n)
            {
                *self.dir_error.lock().unwrap() = true;
            }
            let hook = self.on_first_sleep.lock().unwrap().take();
            if let Some(hook) = hook {
                hook(self);
            }
        })
    }

    fn read_provision(&self, _path: &Path) -> Result<Vec<u8>, Terminal> {
        self.provision.lock().unwrap().clone()
    }

    fn root_file(&self, path: &Path) -> io::Result<RootFile> {
        match self.files.lock().unwrap().get(path) {
            Some(Ok(file)) => Ok(file.clone()),
            Some(Err(e)) => Err(io::Error::new(e.kind(), "scripted")),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    fn own_binary_sha256(&self) -> io::Result<String> {
        self.own_sha256
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| io::Error::other("unreadable"))
    }

    fn build(&self) -> Build {
        self.build.lock().unwrap().clone()
    }

    fn set_trusted_binary(&self, sha256: &str) {
        *self.trusted.lock().unwrap() = Some(sha256.to_owned());
    }

    fn station_config(&self) -> ConfigState {
        self.config.lock().unwrap().clone()
    }

    fn create_station_config(&self, name: &str) -> io::Result<()> {
        self.log(format!("create_config:{name}"));
        *self.config.lock().unwrap() = ConfigState::Named(name.to_owned());
        Ok(())
    }

    fn service(&self) -> ServiceState {
        *self.service.lock().unwrap()
    }

    fn install_service(&self) -> io::Result<()> {
        self.log("install_service");
        *self.service.lock().unwrap() = ServiceState::Matching { active: true };
        Ok(())
    }

    fn start_service(&self) -> io::Result<()> {
        self.log("start_service");
        *self.service.lock().unwrap() = ServiceState::Matching { active: true };
        Ok(())
    }

    fn fingerprint(&self, probe: bool) -> BoxFuture<'_, Result<StationView, CallError>> {
        Box::pin(async move {
            self.log(if probe {
                "fingerprint:probe"
            } else {
                "fingerprint"
            });
            let now = *self.clock.lock().unwrap();
            let mut st = self.station.lock().unwrap();
            if now < st.up_at {
                return Err(st.down_error);
            }
            if probe && st.probe_errors > 0 {
                st.probe_errors -= 1;
                return Err(CallError::Timeout);
            }
            let link_up = if probe {
                if let Some(edit) = st.probe_edits.pop_front() {
                    edit(&mut st);
                }
                st.link_default
            } else {
                if let Some(edit) = st.edits.pop_front() {
                    edit(&mut st);
                }
                let default = st.link_default;
                st.link_script.pop_front().unwrap_or(default)
            };
            Ok(StationView {
                fingerprint: st.fingerprint.clone(),
                paired_deck: st.paired.clone(),
                link_up,
                build: st.build.clone(),
                binary_sha256: st.binary_sha256.clone(),
                providers: if probe { st.providers.clone() } else { None },
            })
        })
    }

    fn join(
        &self,
        host: &str,
        port: u16,
        deck_fingerprint: &str,
        name: &str,
    ) -> BoxFuture<'_, JoinOutcome> {
        let args = (
            host.to_owned(),
            port,
            deck_fingerprint.to_owned(),
            name.to_owned(),
        );
        Box::pin(async move {
            self.log("join");
            let deck = args.2.clone();
            self.join_args.lock().unwrap().push(args);
            let step = self.joins.lock().unwrap().pop_front();
            match step.expect("an unscripted join") {
                JoinStep::Paired => {
                    self.station.lock().unwrap().paired = Some(deck);
                    JoinOutcome::Paired
                }
                JoinStep::Code(code) => JoinOutcome::Code(code.to_owned()),
                JoinStep::CodeCommitted(code) => {
                    self.station.lock().unwrap().paired = Some(deck);
                    JoinOutcome::Code(code.to_owned())
                }
            }
        })
    }

    fn omarchy_install(&self) -> BoxFuture<'_, io::Result<i32>> {
        Box::pin(async move {
            self.log("omarchy_install");
            self.install.lock().unwrap().pop_front().unwrap_or(Ok(0))
        })
    }

    fn omarchy_status_json(&self) -> BoxFuture<'_, io::Result<StatusAnswer>> {
        Box::pin(async move {
            self.log("omarchy_status");
            self.status
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(healthy_status()))
        })
    }

    fn notify(&self, grouped_fingerprint: &str) {
        self.notices
            .lock()
            .unwrap()
            .push(grouped_fingerprint.to_owned());
    }

    fn progress(&self, line: &str) {
        self.stderr.lock().unwrap().push(line.to_owned());
        for hook in self.progress_hooks.lock().unwrap().iter() {
            hook(line);
        }
    }

    fn identity_loads(&self) -> bool {
        *self.identity.lock().unwrap()
    }

    fn report(
        &self,
        host: &str,
        port: u16,
        deck_fingerprint: &str,
        report: &ValidReport,
    ) -> BoxFuture<'_, Result<ReportReply, ReportFailure>> {
        let entry = (
            host.to_owned(),
            port,
            deck_fingerprint.to_owned(),
            report.clone(),
        );
        Box::pin(async move {
            self.log("report");
            let report = entry.3.clone();
            self.reports.lock().unwrap().push(entry);
            let step = self.report_steps.lock().unwrap().pop_front();
            match step.expect("an unscripted report") {
                ReportStep::Reply(reply) => Ok(reply),
                ReportStep::RetryFor(id) => Ok(ReportReply::retry(
                    id.to_owned(),
                    report.terminal_id,
                    report.state_digest,
                )),
                ReportStep::StaleRetry => Ok(ReportReply::retry(
                    "11".repeat(16),
                    "22".repeat(16),
                    report.state_digest,
                )),
                ReportStep::Fail(word) => Err(ReportFailure(word)),
            }
        })
    }

    fn jitter(&self, max: Duration) -> Duration {
        (*self.jitter.lock().unwrap()).min(max)
    }

    fn read_authorization(&self, path: &Path) -> io::Result<Vec<u8>> {
        let bytes = read_private(path, 1024).map_err(io::Error::other);
        if let Some(hook) = self.after_auth_read.lock().unwrap().take() {
            hook();
        }
        bytes
    }

    fn delete_authorization(&self, path: &Path) -> io::Result<()> {
        if *self.fail_delete.lock().unwrap() {
            return Err(io::Error::other("scripted"));
        }
        self.authorization_deleted
            .lock()
            .unwrap()
            .push(path.to_path_buf());
        std::fs::remove_file(path)
    }
}

/// One run of the bootstrap against the fake.
pub async fn go(fake: &Fake, retry: bool) -> super::run::Exit {
    super::run::run(fake, Path::new("/provision.toml"), retry, None).await
}

/// One run of the bootstrap with a retry authorization.
pub async fn go_authorized(fake: &Fake, authorization: &Path) -> super::run::Exit {
    super::run::run(
        fake,
        Path::new("/provision.toml"),
        false,
        Some(authorization),
    )
    .await
}

/// Writes the state file as it is given (a test of the file rules).
pub fn write_state(fake: &Fake, bytes: &[u8], mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fake.ensure_station_dir().unwrap();
    let path = fake.state_file();
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}
