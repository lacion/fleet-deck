//! The adapter configs of restored shim sessions (moved out of `mod.rs`
//! for the module size rule; no behavior change).

use super::{Adapters, shim_station};

/// The adapter configs of restored shim sessions: each session's adapter
/// builds its durable launch from the stored summary.
#[must_use]
pub fn durable_configs(adapters: &Adapters) -> shim_station::Configs {
    fn launch(
        adapters: &Adapters,
        summary: &fleetdeck_proto::SessionSummary,
    ) -> Option<fleetdeck_providers::durable::DurableConfig> {
        let instance = summary.provider_instance.clone()?;
        let adapter = adapters.get(&instance)?;
        let spec = fleetdeck_proto::command::SessionSpec {
            effort: None,
            provider_instance: instance,
            cwd: summary.cwd.clone()?,
            mode: summary.mode.clone(),
            model: summary.model.clone(),
            title: summary.title.clone(),
            coordinator: None,
            launch: None,
            extra_env: None,
        };
        adapter.durable_launch(&spec).ok().map(|l| l.core)
    }
    let (a, b) = (adapters.clone(), adapters.clone());
    shim_station::Configs {
        claude: Box::new(move |summary| match launch(&a, summary)? {
            fleetdeck_providers::durable::DurableConfig::Claude(config) => Some(config),
            fleetdeck_providers::durable::DurableConfig::Codex { .. } => None,
        }),
        codex: Box::new(move |summary| match launch(&b, summary)? {
            fleetdeck_providers::durable::DurableConfig::Codex { config, .. } => Some(config),
            fleetdeck_providers::durable::DurableConfig::Claude(_) => None,
        }),
    }
}
