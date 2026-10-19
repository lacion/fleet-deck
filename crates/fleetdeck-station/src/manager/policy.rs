//! The station policy from `station.toml` (PLAN §6.5, §10): allowed roots,
//! the session limit, the provider instances, and the offline approval
//! policy. The decision step applies it in the command's transaction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fleetdeck_core::ChildEnv;
use fleetdeck_core::config::{
    DEFAULT_MAX_SHELLS, HostChoice, ModelHelpConfig, OfflineApprovalPolicy as ConfigOffline,
    StationConfig, TrackerConfig,
};
use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_providers::adapter::canonical_workspace;

use crate::store::OfflineApprovalPolicy;

/// Why the policy refuses a workspace.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceError {
    #[error("`{0}` does not exist or is not a directory")]
    Missing(String),
    #[error("`{0}` is outside the allowed roots of this station")]
    Outside(String),
}

/// The platform facts of a station that hosts its sessions under shims
/// (DL-182). Without it the station runs the direct host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShimHostPlatform {
    /// Resume on the shim host needs the Linux process and boot readers
    /// (`/proc`); on another platform a resume, an import, the input of a
    /// hibernated session and a handover are refused.
    pub supported: bool,
}

/// The refusal of a resume on a shim host that this platform cannot run.
pub const SHIM_HOST_NEEDS_LINUX: &str = "Import and resume need Linux on a shim host. On this station, set Host to direct in its settings.";

/// The environment of each provider instance, as a provider process gets it:
/// the environment of its setup command too (DL-148).
pub type SetupEnvs = BTreeMap<ProviderInstanceId, ChildEnv>;

/// The configuration snapshot of one station: the values that the station
/// reads at run time. [`super::policy_hub::PolicyHub`] holds the current one;
/// every consumer reads it when it handles a request, a decision or a
/// dispatch. It holds configuration only. The runtime state is built once and
/// lives elsewhere: the coordinator bindings, the integration service, the
/// assist runner and budget, and the shells.
#[derive(Debug, Clone)]
pub struct Policy {
    /// `name` of `station.toml`: the name that the board shows. It is empty
    /// for a test policy.
    name: String,
    /// Canonical roots (symlinks resolved, t3code trap 8).
    allowed_roots: Vec<PathBuf>,
    max_sessions: u32,
    /// `allow_setup_cmd` of `station.toml` (DL-148).
    allow_setup_cmd: bool,
    /// The environment of a setup command, by provider instance (DL-148).
    setup_envs: SetupEnvs,
    offline: OfflineApprovalPolicy,
    instances: BTreeMap<ProviderInstanceId, ProviderKind>,
    /// Canonical repository path → the owner's group (DL-201).
    repo_groups: BTreeMap<PathBuf, String>,
    /// `hibernate_after_min` of `station.toml`; 0 is off (R6.6, DL-169).
    hibernate_after_min: u32,
    /// The `[[trackers]]` entries (R3, DL-164). The integration service
    /// is rebuilt from them when they change.
    trackers: Vec<TrackerConfig>,
    /// `allow_shell` of `station.toml`: new shells and handovers.
    allow_shell: bool,
    /// `allow_bypass_permissions` of `station.toml` (NV-24).
    allow_bypass_permissions: bool,
    /// `allow_fleet_control` of `station.toml` (ADR-008 §3).
    allow_fleet_control: bool,
    /// `allow_desktop` of `station.toml` (R13 §6.4).
    allow_desktop: bool,
    /// `allow_desktop_capture` of `station.toml` (R13 P5a).
    allow_desktop_capture: bool,
    /// `allow_desktop_input` of `station.toml` (R13 P7 §3.3).
    allow_desktop_input: bool,
    /// `allow_desktop_click` of `station.toml` (R13 P7b §3.2).
    allow_desktop_click: bool,
    /// `allow_desktop_input_any` of `station.toml` (R13 P7b §3.2).
    allow_desktop_input_any: bool,
    /// `capture_monitors` of `station.toml` (R13 P5a); empty means every monitor.
    capture_monitors: Vec<String>,
    /// `fleet_summary` of `station.toml` (R13 P4): the deck may send the fleet summary.
    fleet_summary: bool,
    /// `max_shells` of `station.toml`.
    max_shells: u32,
    /// `[model_help]` of `station.toml` (R10, DL-167).
    model_help: ModelHelpConfig,
    /// The files hidden from the members of a shared session (R11.9,
    /// DL-166): the default list plus `member_deny` of `station.toml`.
    member_deny: MemberDeny,
    /// `Some` for a station with `host = "shim"` (DL-182); `None` is the
    /// direct host.
    shim_host: Option<ShimHostPlatform>,
    /// Fleet Git accepts a local bare repository as a remote (design §3.4):
    /// the `station.toml` key.
    allow_local_remotes: bool,
    /// Where Fleet Git clones new homes: `clone_root` of `station.toml`, or
    /// the first allowed root + `/projects` (FG-5).
    clone_root: PathBuf,
    /// The labels of `traits` (FG-6).
    traits: Vec<String>,
    /// `allow_check_cmd` of `station.toml` (FG-8).
    allow_check_cmd: bool,
    /// `[taildrive_mounts]` of `station.toml` (FG-12).
    taildrive_mounts: BTreeMap<String, PathBuf>,
}

/// The default `clone_root`: the first allowed root + `/projects`.
fn default_clone_root(roots: &[PathBuf]) -> PathBuf {
    roots
        .first()
        .map_or_else(|| PathBuf::from("projects"), |root| root.join("projects"))
}

impl Policy {
    /// The policy of `config`. A root that does not exist stays as written:
    /// nothing can be inside it until it exists.
    #[must_use]
    pub fn from_config(config: &StationConfig) -> Self {
        let offline = match config.offline_approval_policy {
            ConfigOffline::Wait => OfflineApprovalPolicy::Wait,
            ConfigOffline::DenyAfter(d) => OfflineApprovalPolicy::DenyAfter {
                after_ms: u64::try_from(d.as_millis()).unwrap_or(u64::MAX),
            },
        };
        let allowed_roots: Vec<PathBuf> = config
            .allowed_roots
            .iter()
            .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()))
            .collect();
        Self {
            name: config.name.clone(),
            clone_root: config
                .clone_root
                .clone()
                .unwrap_or_else(|| default_clone_root(&allowed_roots)),
            allowed_roots,
            max_sessions: config.max_sessions,
            allow_setup_cmd: config.allow_setup_cmd,
            setup_envs: SetupEnvs::new(),
            offline,
            instances: config
                .provider_instances
                .iter()
                .map(|i| (i.id.clone(), i.driver.kind()))
                .collect(),
            repo_groups: BTreeMap::new(),
            hibernate_after_min: config.hibernate_after_min,
            trackers: config.trackers.clone(),
            allow_shell: config.allow_shell,
            allow_bypass_permissions: config.allow_bypass_permissions,
            allow_fleet_control: config.allow_fleet_control,
            allow_desktop: config.allow_desktop,
            allow_desktop_capture: config.allow_desktop_capture,
            allow_desktop_input: config.allow_desktop_input,
            allow_desktop_click: config.allow_desktop_click,
            allow_desktop_input_any: config.allow_desktop_input_any,
            capture_monitors: config.capture_monitors.clone(),
            fleet_summary: config.fleet_summary,
            max_shells: config.max_shells,
            model_help: config.model_help.clone(),
            member_deny: MemberDeny::new(&config.member_deny),
            shim_host: matches!(config.host, HostChoice::Shim).then_some(ShimHostPlatform {
                supported: cfg!(target_os = "linux"),
            }),
            allow_local_remotes: config.allow_local_remotes,
            traits: config.traits.clone(),
            allow_check_cmd: config.allow_check_cmd,
            taildrive_mounts: config.taildrive_mounts.clone(),
        }
        .with_repo_groups(&config.repo_groups)
    }

    /// The name that the board shows for this station (empty for a test
    /// policy).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sets the owner's repo groups (paths are resolved like the roots).
    #[must_use]
    pub fn with_repo_groups(mut self, groups: &BTreeMap<PathBuf, String>) -> Self {
        self.repo_groups = groups
            .iter()
            .map(|(p, g)| (p.canonicalize().unwrap_or_else(|_| p.clone()), g.clone()))
            .collect();
        self
    }

    /// The same policy with hibernation on after `minutes` of idleness
    /// (0 turns it off).
    #[must_use]
    pub fn with_hibernate_after_min(mut self, minutes: u32) -> Self {
        self.hibernate_after_min = minutes;
        self
    }

    /// Minutes of idleness after which a session hibernates; 0 is off.
    #[must_use]
    pub fn hibernate_after_min(&self) -> u32 {
        self.hibernate_after_min
    }

    /// The owner's group of a canonical repository path.
    #[must_use]
    pub fn repo_group(&self, repo: &Path) -> Option<&str> {
        self.repo_groups.get(repo).map(String::as_str)
    }

    /// A policy for tests and the all-in-one mode.
    #[must_use]
    pub fn new(
        allowed_roots: Vec<PathBuf>,
        max_sessions: u32,
        offline: OfflineApprovalPolicy,
        instances: BTreeMap<ProviderInstanceId, ProviderKind>,
    ) -> Self {
        let allowed_roots: Vec<PathBuf> = allowed_roots
            .into_iter()
            .map(|r| r.canonicalize().unwrap_or(r))
            .collect();
        Self {
            name: String::new(),
            clone_root: default_clone_root(&allowed_roots),
            allowed_roots,
            max_sessions,
            allow_setup_cmd: false,
            setup_envs: SetupEnvs::new(),
            offline,
            instances,
            repo_groups: BTreeMap::new(),
            hibernate_after_min: 0,
            trackers: Vec::new(),
            allow_shell: true,
            allow_bypass_permissions: false,
            allow_fleet_control: false,
            allow_desktop: false,
            allow_desktop_capture: false,
            allow_desktop_input: false,
            allow_desktop_click: false,
            allow_desktop_input_any: false,
            capture_monitors: Vec::new(),
            fleet_summary: false,
            max_shells: DEFAULT_MAX_SHELLS,
            model_help: ModelHelpConfig::default(),
            member_deny: MemberDeny::default(),
            shim_host: None,
            allow_local_remotes: false,
            traits: Vec::new(),
            allow_check_cmd: false,
            taildrive_mounts: BTreeMap::new(),
        }
    }

    /// The same policy with another station name (tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    /// The same policy with local remotes allowed or not (Fleet Git, design
    /// §3.4).
    #[must_use]
    pub fn with_allow_local_remotes(mut self, on: bool) -> Self {
        self.allow_local_remotes = on;
        self
    }

    /// Whether Fleet Git accepts a local bare repository as a remote.
    #[must_use]
    pub fn allow_local_remotes(&self) -> bool {
        self.allow_local_remotes
    }

    /// Where Fleet Git clones new homes (FG-5).
    #[must_use]
    pub fn clone_root(&self) -> &Path {
        &self.clone_root
    }

    /// The same policy with another clone folder.
    #[must_use]
    pub fn with_clone_root(mut self, root: PathBuf) -> Self {
        self.clone_root = root;
        self
    }

    /// The labels of `traits` (FG-6).
    #[must_use]
    pub fn traits(&self) -> &[String] {
        &self.traits
    }

    /// The same policy with other station labels.
    #[must_use]
    pub fn with_traits(mut self, traits: Vec<String>) -> Self {
        self.traits = traits;
        self
    }

    /// Whether a project may run its check command here (FG-8).
    #[must_use]
    pub fn allow_check_cmd(&self) -> bool {
        self.allow_check_cmd
    }

    /// The Taildrive mounts of this station (FG-12).
    #[must_use]
    pub fn taildrive_mounts(&self) -> &BTreeMap<String, PathBuf> {
        &self.taildrive_mounts
    }

    /// The same policy for a station on the shim host; `supported` is
    /// false on a platform that cannot resume there (DL-182). Tests use it
    /// to show the refusal on Linux.
    #[must_use]
    pub fn with_shim_host(mut self, supported: bool) -> Self {
        self.shim_host = Some(ShimHostPlatform { supported });
        self
    }

    /// The same policy for a station that hosts its sessions under shims:
    /// a host that the policy already names stays (a test sets it).
    #[must_use]
    pub fn on_shim_host(mut self) -> Self {
        self.shim_host.get_or_insert(ShimHostPlatform {
            supported: cfg!(target_os = "linux"),
        });
        self
    }

    /// The same policy for a station on the direct host (DL-188 step 0: a
    /// macOS station with host = shim runs the direct host).
    #[must_use]
    pub fn without_shim_host(mut self) -> Self {
        self.shim_host = None;
        self
    }

    /// Why a resume, an import, the input of a hibernated session or a
    /// handover is refused for the platform, if it is.
    #[must_use]
    pub fn shim_host_refusal(&self) -> Option<&'static str> {
        match self.shim_host {
            Some(ShimHostPlatform { supported: false }) => Some(SHIM_HOST_NEEDS_LINUX),
            _ => None,
        }
    }

    /// The `[[trackers]]` entries of the station.
    #[must_use]
    pub fn trackers(&self) -> &[TrackerConfig] {
        &self.trackers
    }

    /// Whether new shells and terminal handovers are allowed.
    #[must_use]
    pub fn allow_shell(&self) -> bool {
        self.allow_shell
    }

    /// Whether a bypass or full-access mode may run (NV-24).
    #[must_use]
    pub fn allow_bypass_permissions(&self) -> bool {
        self.allow_bypass_permissions
    }

    /// Whether coordinators with a fleet-control grant may act on this
    /// station (ADR-008 §3). Read it live: at intake, at the decision and
    /// at the dispatch.
    #[must_use]
    pub fn allow_fleet_control(&self) -> bool {
        self.allow_fleet_control
    }

    /// Whether the owner may control this station's desktop (R13 §6.4). Read
    /// it live: at intake and right before every program run.
    #[must_use]
    pub fn allow_desktop(&self) -> bool {
        self.allow_desktop
    }

    /// Whether the owner may take screenshots from the board (R13 P5a). Read
    /// it live, together with [`Self::allow_desktop`].
    #[must_use]
    pub fn allow_desktop_capture(&self) -> bool {
        self.allow_desktop_capture
    }

    /// Whether authorized coordinator grants may type and move the pointer on
    /// this station (R13 P7 §3.3). Read it live.
    #[must_use]
    pub fn allow_desktop_input(&self) -> bool {
        self.allow_desktop_input
    }

    /// Whether authorized coordinator grants may send one left pointer click
    /// on this station (R13 P7b §3.2). Needs [`Self::allow_desktop_input`].
    /// Read it live.
    #[must_use]
    pub fn allow_desktop_click(&self) -> bool {
        self.allow_desktop_click
    }

    /// Whether authorized coordinator grants may send input to other
    /// application windows on this station (R13 P7b §3.2). Needs
    /// [`Self::allow_desktop_input`]. Read it live.
    #[must_use]
    pub fn allow_desktop_input_any(&self) -> bool {
        self.allow_desktop_input_any
    }

    /// The monitors that a screenshot may capture; empty means every monitor
    /// (R13 P5a). Read it live.
    #[must_use]
    pub fn capture_monitors(&self) -> &[String] {
        &self.capture_monitors
    }

    /// Whether the deck may send the fleet summary to this station (R13 P4).
    /// Read it live: at intake and before every push.
    #[must_use]
    pub fn fleet_summary(&self) -> bool {
        self.fleet_summary
    }

    /// The same policy with the fleet summary on or off.
    #[must_use]
    pub fn with_fleet_summary(mut self, on: bool) -> Self {
        self.fleet_summary = on;
        self
    }

    /// The same policy with desktop control on or off.
    #[must_use]
    pub fn with_allow_desktop(mut self, on: bool) -> Self {
        self.allow_desktop = on;
        self
    }

    /// The same policy with desktop screenshots on or off (R13 P5a).
    #[must_use]
    pub fn with_allow_desktop_capture(mut self, on: bool) -> Self {
        self.allow_desktop_capture = on;
        self
    }

    /// The same policy with coordinator input on or off (R13 P7).
    #[must_use]
    pub fn with_allow_desktop_input(mut self, on: bool) -> Self {
        self.allow_desktop_input = on;
        self
    }

    /// The same policy with another screenshot monitor list (R13 P5a).
    #[must_use]
    pub fn with_capture_monitors(mut self, list: Vec<String>) -> Self {
        self.capture_monitors = list;
        self
    }

    /// The same policy with fleet control on or off.
    #[must_use]
    pub fn with_allow_fleet_control(mut self, on: bool) -> Self {
        self.allow_fleet_control = on;
        self
    }

    /// The most shells that run at the same time.
    #[must_use]
    pub fn max_shells(&self) -> u32 {
        self.max_shells
    }

    /// The same policy with shells on or off and a shell limit.
    #[must_use]
    pub fn with_shell(mut self, allow: bool, max_shells: u32) -> Self {
        self.allow_shell = allow;
        self.max_shells = max_shells;
        self
    }

    /// The model help settings (R10, DL-167).
    #[must_use]
    pub fn model_help(&self) -> &ModelHelpConfig {
        &self.model_help
    }

    /// The same policy with other model help settings.
    #[must_use]
    pub fn with_model_help(mut self, config: ModelHelpConfig) -> Self {
        self.model_help = config;
        self
    }

    /// The snapshot for a new `config` while the station runs (DL-172
    /// §1.4). The values that apply only at a restart stay as they are:
    /// the provider instances and the setup environments that came from
    /// them.
    #[must_use]
    pub fn reconfigured(&self, config: &StationConfig) -> Self {
        let mut next = Self::from_config(config);
        next.instances.clone_from(&self.instances);
        next.setup_envs.clone_from(&self.setup_envs);
        // The host applies at a restart (DL-182).
        next.shim_host = self.shim_host;
        // The Fleet Git keys apply at a restart too (FG-5).
        next.clone_root.clone_from(&self.clone_root);
        next.traits.clone_from(&self.traits);
        next.allow_check_cmd = self.allow_check_cmd;
        next.allow_local_remotes = self.allow_local_remotes;
        next.taildrive_mounts.clone_from(&self.taildrive_mounts);
        next
    }

    /// Why a start that waited in the queue must not run under this
    /// snapshot (DL-172 §1.4): its folder is outside the roots now, or it
    /// carries a setup command that is off now. `None`: it can run.
    /// `provisioned` is true when the dispatch itself creates the folder
    /// (a worktree or a clone), so that the folder may not exist yet.
    #[must_use]
    pub fn start_refusal(
        &self,
        cwd: &str,
        provisioned: bool,
        has_setup_cmd: bool,
    ) -> Option<&'static str> {
        let inside = if provisioned {
            // The nearest existing parent decides.
            Path::new(cwd)
                .ancestors()
                .find_map(|p| p.canonicalize().ok())
                .is_some_and(|p| self.allowed_roots.iter().any(|root| p.starts_with(root)))
        } else {
            self.workspace(cwd).is_ok()
        };
        if !inside {
            Some("the station policy changed: the folder is outside the allowed roots")
        } else if has_setup_cmd && !self.allow_setup_cmd {
            Some("the station policy changed: setup commands are off")
        } else {
            None
        }
    }

    /// The files that a member of a shared session must not see.
    #[must_use]
    pub fn member_deny(&self) -> &MemberDeny {
        &self.member_deny
    }

    /// The same policy with another deck list (station tests).
    #[cfg(test)]
    pub(crate) fn set_member_deny(&mut self, deny: MemberDeny) {
        self.member_deny = deny;
    }

    /// The same policy with setup commands on or off (DL-148).
    #[must_use]
    pub fn with_allow_setup_cmd(mut self, on: bool) -> Self {
        self.allow_setup_cmd = on;
        self
    }

    /// The same policy with the environments of the setup commands.
    #[must_use]
    pub fn with_setup_envs(mut self, envs: SetupEnvs) -> Self {
        self.setup_envs = envs;
        self
    }

    /// The environment of a setup command of `instance`, if it has one.
    #[must_use]
    pub fn setup_env(&self, instance: &ProviderInstanceId) -> Option<&ChildEnv> {
        self.setup_envs.get(instance)
    }

    /// Whether a session start can run the owner's setup command (DL-148).
    #[must_use]
    pub fn allow_setup_cmd(&self) -> bool {
        self.allow_setup_cmd
    }

    #[must_use]
    pub fn max_sessions(&self) -> u32 {
        self.max_sessions
    }

    #[must_use]
    pub fn offline(&self) -> OfflineApprovalPolicy {
        self.offline
    }

    /// The canonical allowed roots.
    #[must_use]
    pub fn allowed_roots(&self) -> &[PathBuf] {
        &self.allowed_roots
    }

    /// Every configured provider instance with its kind.
    #[must_use]
    pub fn instances(&self) -> &BTreeMap<ProviderInstanceId, ProviderKind> {
        &self.instances
    }

    /// The provider kind of a configured instance.
    #[must_use]
    pub fn instance(&self, id: &ProviderInstanceId) -> Option<ProviderKind> {
        self.instances.get(id).copied()
    }

    /// The canonical workspace for `cwd`, if it is inside an allowed root.
    /// The check runs on the resolved path, so a symlink cannot leave a root.
    ///
    /// # Errors
    /// [`WorkspaceError`].
    pub fn workspace(&self, cwd: &str) -> Result<PathBuf, WorkspaceError> {
        let path = canonical_workspace(Path::new(cwd))
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| WorkspaceError::Missing(cwd.to_owned()))?;
        if self.allowed_roots.iter().any(|root| path.starts_with(root)) {
            Ok(path)
        } else {
            Err(WorkspaceError::Outside(path.to_string_lossy().into_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::policy_hub::test_config;

    #[test]
    fn the_fleet_git_keys_come_from_the_config_with_a_default_clone_root() {
        let root = tempfile::tempdir().unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let extra = "traits = [\"gpu\"]\nallow_check_cmd = true\nallow_local_remotes = true\n\
                     taildrive_mounts = { \"nas/out\" = \"/mnt/out\" }\n";
        let policy = Policy::from_config(&test_config(root.path(), extra));
        assert_eq!(policy.clone_root(), canonical.join("projects"));
        assert_eq!(policy.traits(), ["gpu"]);
        assert!(policy.allow_check_cmd() && policy.allow_local_remotes());
        assert_eq!(policy.taildrive_mounts()["nas/out"], Path::new("/mnt/out"));
        let key = format!("clone_root = \"{}/clones\"\n", root.path().display());
        let named = Policy::from_config(&test_config(root.path(), &key));
        assert_eq!(named.clone_root(), root.path().join("clones"));
        let bare = Policy::from_config(&test_config(root.path(), ""));
        assert!(!bare.allow_check_cmd() && !bare.allow_local_remotes());
        assert!(bare.traits().is_empty() && bare.taildrive_mounts().is_empty());
        // The policy of tests has the same default.
        let plain = Policy::new(
            vec![root.path().to_path_buf()],
            1,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        );
        assert_eq!(plain.clone_root(), canonical.join("projects"));
    }

    #[test]
    fn allow_desktop_input_is_off_by_default_and_follows_a_live_reload() {
        let root = tempfile::tempdir().unwrap();
        let off = Policy::from_config(&test_config(root.path(), ""));
        assert!(!off.allow_desktop_input());
        let on_config = test_config(root.path(), "allow_desktop_input = true\n");
        let on = Policy::from_config(&on_config);
        assert!(on.allow_desktop_input());
        // A live reload reads the value again, in both directions.
        assert!(off.reconfigured(&on_config).allow_desktop_input());
        assert!(
            !on.reconfigured(&test_config(root.path(), ""))
                .allow_desktop_input()
        );
        // The test policy and its builder.
        let plain = Policy::new(
            vec![root.path().to_path_buf()],
            1,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        );
        assert!(!plain.allow_desktop_input());
        assert!(plain.with_allow_desktop_input(true).allow_desktop_input());
    }

    #[test]
    fn allow_desktop_click_and_input_any_are_off_by_default_and_follow_a_live_reload() {
        let root = tempfile::tempdir().unwrap();
        let off = Policy::from_config(&test_config(root.path(), ""));
        assert!(!off.allow_desktop_click() && !off.allow_desktop_input_any());
        let on_config = test_config(
            root.path(),
            "allow_desktop_click = true\nallow_desktop_input_any = true\n",
        );
        let on = Policy::from_config(&on_config);
        assert!(on.allow_desktop_click() && on.allow_desktop_input_any());
        // Each key is its own: the first never sets the second.
        let click = Policy::from_config(&test_config(root.path(), "allow_desktop_click = true\n"));
        assert!(click.allow_desktop_click() && !click.allow_desktop_input_any());
        // A live reload reads both values again, in both directions.
        let reloaded = off.reconfigured(&on_config);
        assert!(reloaded.allow_desktop_click() && reloaded.allow_desktop_input_any());
        let reloaded = on.reconfigured(&test_config(root.path(), ""));
        assert!(!reloaded.allow_desktop_click() && !reloaded.allow_desktop_input_any());
        // The test policy has the same default.
        let plain = Policy::new(
            vec![root.path().to_path_buf()],
            1,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        );
        assert!(!plain.allow_desktop_click() && !plain.allow_desktop_input_any());
    }

    #[test]
    fn a_live_reload_keeps_the_restart_only_fleet_git_values() {
        let root = tempfile::tempdir().unwrap();
        let first = Policy::from_config(&test_config(root.path(), "traits = [\"gpu\"]\n"))
            .with_allow_local_remotes(true);
        let edited = test_config(
            root.path(),
            "traits = [\"other\"]\nallow_check_cmd = true\nallow_local_remotes = false\n",
        );
        let next = first.reconfigured(&edited);
        assert_eq!(next.traits(), ["gpu"]);
        assert!(!next.allow_check_cmd());
        assert!(next.allow_local_remotes(), "the restart-only value stays");
    }

    #[test]
    fn without_shim_host_clears_the_refusal_and_survives_reconfigured() {
        let config = test_config(Path::new("/tmp"), "host = \"shim\"\n");
        let shim = Policy::from_config(&config).with_shim_host(false);
        assert_eq!(shim.shim_host_refusal(), Some(SHIM_HOST_NEEDS_LINUX));
        let direct = shim.without_shim_host();
        assert_eq!(direct.shim_host_refusal(), None);
        // A live reload from a file that still says `shim` keeps the host.
        assert_eq!(direct.reconfigured(&config).shim_host_refusal(), None);
        // `on_shim_host` is the only way back.
        assert!(direct.shim_host.is_none());
    }
}
