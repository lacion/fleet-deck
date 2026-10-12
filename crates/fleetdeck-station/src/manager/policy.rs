//! The station policy from `station.toml` (PLAN §6.5, §10): allowed roots,
//! the session limit, the provider instances, and the offline approval
//! policy. The decision step applies it in the command's transaction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// The validated policy of one station.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Canonical roots (symlinks resolved, t3code trap 8).
    allowed_roots: Vec<PathBuf>,
    max_sessions: u32,
    offline: OfflineApprovalPolicy,
    instances: BTreeMap<ProviderInstanceId, ProviderKind>,
    /// Canonical repository path → the owner's group (DL-201).
    repo_groups: BTreeMap<PathBuf, String>,
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
            offline,
            instances: config
                .provider_instances
                .iter()
                .map(|i| (i.id.clone(), i.driver.kind()))
                .collect(),
            repo_groups: BTreeMap::new(),
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
            offline,
            instances,
            repo_groups: BTreeMap::new(),
            coordinator: crate::coordinator::Bindings::default(),
        }
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
