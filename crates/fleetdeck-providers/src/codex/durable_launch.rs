//! The durable launch of a Codex session (DL-113): the direct start's
//! workspace, capability, mode and thread rules, without a process.

use std::ffi::OsString;
use std::path::Path;

use fleetdeck_proto::command::SessionSpec;

use super::super::{CoreConfig, app_server_args, capabilities};
use super::CodexAdapter;
use crate::adapter::{ProviderError, Result, canonical_workspace};
use crate::durable::{DurableConfig, DurableLaunch};
use crate::host::ProcessSpec;

impl CodexAdapter {
    pub(super) fn launch_durable(&self, spec: &SessionSpec) -> Result<DurableLaunch> {
        let cwd = canonical_workspace(Path::new(&spec.cwd))?;
        let caps = capabilities(self.config.allow_full_access);
        if let Some(mode) = &spec.mode
            && !caps.modes.iter().any(|m| m == mode)
        {
            return Err(ProviderError::InvalidMode(mode.clone()));
        }
        let cwd_text = cwd.to_string_lossy().into_owned();
        Ok(DurableLaunch {
            process: ProcessSpec {
                program: self.config.program.clone(),
                args: app_server_args().into_iter().map(OsString::from).collect(),
                cwd: cwd.clone(),
                env: self.config.env.clone(),
            },
            core: DurableConfig::Codex {
                config: CoreConfig {
                    provider_instance: spec.provider_instance.clone(),
                    cwd: cwd_text.clone(),
                    capabilities: caps,
                    resumed: None,
                    client: self.config.client.clone(),
                },
                thread: super::super::SessionSpec {
                    model: spec.model.clone(),
                    effort: self.config.effort.clone(),
                    mode: spec.mode.clone(),
                    cwd: Some(cwd_text),
                    resume: None,
                    fork: false,
                },
            },
            stop: self.config.stop,
        })
    }
}
