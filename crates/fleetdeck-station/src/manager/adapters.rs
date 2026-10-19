//! The adapters and the setup environments built from `station.toml`
//! (moved out of `manager/mod.rs` to keep it under 800 lines).

use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::config::{Driver, StationConfig};
use fleetdeck_providers::ProviderAdapter;
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig, RawLogSettings};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::SessionHost;
use fleetdeck_providers::rawlog::RawLogLimits;

use super::{Adapters, policy};

/// Builds the adapters of the configured provider instances: one Claude or
/// Codex adapter per instance, with the raw-frame log under `raw_root`.
///
/// # Errors
/// An instance `env` table that cannot go into a child environment.
pub fn adapters_from_config(
    config: &StationConfig,
    host: &Arc<dyn SessionHost>,
    raw_root: Option<PathBuf>,
) -> std::result::Result<Adapters, fleetdeck_core::childenv::ChildEnvError> {
    let mut out = Adapters::new();
    // The stop grace of `limits.provider_stop_grace_seconds` (a restart value).
    let stop = super::policy_limits::stop_policy(&config.limits);
    let raw_log = || {
        raw_root.as_ref().map(|root| RawLogSettings {
            root: root.clone(),
            limits: RawLogLimits::default(),
        })
    };
    for instance in &config.provider_instances {
        let env = ChildEnv::for_instance(ChildEnv::from_process(), instance)?;
        let adapter: Arc<dyn ProviderAdapter> = match instance.driver {
            Driver::Claude => {
                let mut cfg =
                    ClaudeAdapterConfig::new(instance.id.clone(), instance.binary.clone(), env);
                cfg.raw_log = raw_log();
                cfg.allow_bypass = config.allow_bypass_permissions;
                cfg.stop = stop;
                Arc::new(ClaudeAdapter::new(cfg, Arc::clone(host)))
            }
            Driver::Codex => {
                let mut cfg =
                    CodexAdapterConfig::new(instance.id.clone(), instance.binary.clone(), env);
                cfg.raw_log = raw_log();
                cfg.allow_full_access = config.allow_bypass_permissions;
                cfg.stop = stop;
                Arc::new(CodexAdapter::new(cfg, Arc::clone(host)))
            }
        };
        out.insert(instance.id.clone(), adapter);
    }
    Ok(out)
}

/// The environments of the setup commands: for each provider instance the
/// same environment that its adapter gets (DL-148).
///
/// # Errors
/// An instance `env` table that cannot go into a child environment.
pub fn setup_envs_from_config(
    config: &StationConfig,
) -> std::result::Result<policy::SetupEnvs, fleetdeck_core::childenv::ChildEnvError> {
    let mut out = policy::SetupEnvs::new();
    for instance in &config.provider_instances {
        let env = ChildEnv::for_instance(ChildEnv::from_process(), instance)?;
        out.insert(instance.id.clone(), env);
    }
    Ok(out)
}
