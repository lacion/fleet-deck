//! The station policy from `station.toml` (PLAN §6.5, §10): allowed roots,
//! the session limit, the provider instances, and the offline approval
//! policy. The decision step applies it in the command's transaction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fleetdeck_core::ChildEnv;
use fleetdeck_core::config::{
    DEFAULT_MAX_SHELLS, ModelHelpConfig, OfflineApprovalPolicy as ConfigOffline, StationConfig,
    TrackerConfig,
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
    /// `max_shells` of `station.toml`.
    max_shells: u32,
    /// `[model_help]` of `station.toml` (R10, DL-167).
    model_help: ModelHelpConfig,
    /// The files hidden from the members of a shared session (R11.9,
    /// DL-166): the default list plus `member_deny` of `station.toml`.
    member_deny: MemberDeny,
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
        Self {
            allowed_roots: config
                .allowed_roots
                .iter()
                .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()))
                .collect(),
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
            max_shells: config.max_shells,
            model_help: config.model_help.clone(),
            member_deny: MemberDeny::new(&config.member_deny),
        }
        .with_repo_groups(&config.repo_groups)
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
        Self {
            allowed_roots: allowed_roots
                .into_iter()
                .map(|r| r.canonicalize().unwrap_or(r))
                .collect(),
            max_sessions,
            allow_setup_cmd: false,
            setup_envs: SetupEnvs::new(),
            offline,
            instances,
            repo_groups: BTreeMap::new(),
            hibernate_after_min: 0,
            trackers: Vec::new(),
            allow_shell: true,
            max_shells: DEFAULT_MAX_SHELLS,
            model_help: ModelHelpConfig::default(),
            member_deny: MemberDeny::default(),
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
