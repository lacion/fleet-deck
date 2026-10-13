//! The station policy from `station.toml` (PLAN §6.5, §10): allowed roots,
//! the session limit, the provider instances, and the offline approval
//! policy. The decision step applies it in the command's transaction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fleetdeck_core::ChildEnv;
use fleetdeck_core::config::{OfflineApprovalPolicy as ConfigOffline, StationConfig};
use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::event::ProviderKind;
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

/// The validated policy of one station.
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
    /// The coordinator bindings of this station (ADR-006 §5): the ingress
    /// authenticates against them, and the owner's renew and revoke
    /// commands change them (DL-212). Shared, not configuration.
    coordinator: crate::coordinator::Bindings,
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
            coordinator: crate::coordinator::Bindings::default(),
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

    /// The coordinator bindings of this station.
    #[must_use]
    pub fn coordinator_bindings(&self) -> &crate::coordinator::Bindings {
        &self.coordinator
    }

    /// The same policy with other coordinator bindings (station tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_coordinator(mut self, bindings: crate::coordinator::Bindings) -> Self {
        self.coordinator = bindings;
        self
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
            coordinator: crate::coordinator::Bindings::default(),
        }
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
