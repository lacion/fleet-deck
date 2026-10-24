//! The adapters and the setup environments built from `station.toml`
//! (moved out of `manager/mod.rs` to keep it under 800 lines).

use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::config::{Driver, ProviderInstance, StationConfig};
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
    let raw_log = || raw_log_settings(config, raw_root.as_deref());
    for instance in &config.provider_instances {
        let env = instance_env(config, ChildEnv::from_process(), instance)?;
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

/// The child environment of a provider instance: `base` plus the instance
/// settings, and `DISABLE_AUTOUPDATER=1` for a Claude instance when
/// `provider_updates.enabled`, unless the instance `env` table sets the name
/// (B28 §3.10).
///
/// # Errors
/// An instance `env` table that cannot go into a child environment.
pub fn instance_env(
    config: &StationConfig,
    base: ChildEnv,
    instance: &ProviderInstance,
) -> std::result::Result<ChildEnv, fleetdeck_core::childenv::ChildEnvError> {
    let mut env = ChildEnv::for_instance(base, instance)?;
    if config.provider_updates.enabled
        && instance.driver == Driver::Claude
        && !instance.env.contains_key("DISABLE_AUTOUPDATER")
    {
        env.set("DISABLE_AUTOUPDATER", "1")?;
    }
    Ok(env)
}

/// The raw-frame log of the provider adapters: `None` when there is no
/// `raw_root` or when `retention.raw_log_mib` is 0 (off). Else 4 files of
/// a quarter of `raw_log_mib` each (a restart value).
fn raw_log_settings(
    config: &StationConfig,
    raw_root: Option<&std::path::Path>,
) -> Option<RawLogSettings> {
    let mib = u64::from(config.retention.raw_log_mib);
    if mib == 0 {
        return None;
    }
    raw_root.map(|root| RawLogSettings {
        root: root.to_path_buf(),
        limits: RawLogLimits {
            max_file_bytes: mib * 1024 * 1024 / 4,
            max_files: 4,
            ..RawLogLimits::default()
        },
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::policy_hub::test_config;

    fn settings(extra: &str) -> Option<RawLogSettings> {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path(), extra);
        raw_log_settings(&config, Some(std::path::Path::new("/raw")))
    }

    #[test]
    fn the_default_raw_log_is_4_files_of_4_mib() {
        let on = settings("").expect("raw logs are on");
        assert_eq!(on.root, std::path::Path::new("/raw"));
        assert_eq!(on.limits, RawLogLimits::default());
    }

    #[test]
    fn raw_logs_are_off_at_0_and_the_file_size_follows_the_setting() {
        assert!(settings("[retention]\nraw_log_mib = 0\n").is_none());
        let small = settings("[retention]\nraw_log_mib = 1\n").unwrap();
        assert_eq!(small.limits.max_file_bytes, 256 * 1024);
        assert_eq!(small.limits.max_files, 4);
        let big = settings("[retention]\nraw_log_mib = 256\n").unwrap();
        assert_eq!(big.limits.max_file_bytes, 64 * 1024 * 1024);
    }

    fn claude_env(extra: &str) -> ChildEnv {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path(), extra);
        let instance = ProviderInstance::default_for(Driver::Claude);
        instance_env(&config, ChildEnv::empty(), &instance).unwrap()
    }

    #[test]
    fn a_claude_env_has_disable_autoupdater_only_when_updates_are_on() {
        let on = claude_env("[provider_updates]\nenabled = true\n");
        assert_eq!(on.get("DISABLE_AUTOUPDATER").unwrap(), "1");
        assert!(claude_env("").get("DISABLE_AUTOUPDATER").is_none());
    }

    #[test]
    fn an_instance_env_value_wins_over_disable_autoupdater() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path(), "[provider_updates]\nenabled = true\n");
        let mut instance = ProviderInstance::default_for(Driver::Claude);
        instance.env.insert(
            "DISABLE_AUTOUPDATER".to_owned(),
            fleetdeck_core::secret::Secret::new("0".to_owned()),
        );
        let env = instance_env(&config, ChildEnv::empty(), &instance).unwrap();
        assert_eq!(env.get("DISABLE_AUTOUPDATER").unwrap(), "0");
    }

    #[test]
    fn a_codex_env_never_has_disable_autoupdater() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path(), "[provider_updates]\nenabled = true\n");
        let instance = ProviderInstance::default_for(Driver::Codex);
        let env = instance_env(&config, ChildEnv::empty(), &instance).unwrap();
        assert!(env.get("DISABLE_AUTOUPDATER").is_none());
    }

    #[test]
    fn there_is_no_raw_log_without_a_root() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path(), "");
        assert!(raw_log_settings(&config, None).is_none());
    }
}
