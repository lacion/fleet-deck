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
        }
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
