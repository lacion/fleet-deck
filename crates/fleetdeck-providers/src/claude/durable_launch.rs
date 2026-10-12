//! The durable launch of a Claude session (DL-113): the direct start's
//! workspace, capability and argument rules, without a process.

use std::ffi::OsString;
use std::path::Path;

use fleetdeck_proto::command::SessionSpec;

use super::super::core::CoreConfig;
use super::super::{Launch, capabilities, cli_args};
use super::ClaudeAdapter;
use crate::adapter::{Result, canonical_workspace};
use crate::durable::{DurableConfig, DurableLaunch};
use crate::host::ProcessSpec;

impl ClaudeAdapter {
    pub(super) fn launch_durable(&self, spec: &SessionSpec) -> Result<DurableLaunch> {
        let cwd = canonical_workspace(Path::new(&spec.cwd))?;
        let caps = capabilities(self.config.allow_bypass);
        let launch = Launch {
            model: spec.model.clone(),
            mode: spec.mode.clone(),
            resume: None,
            fork: false,
            extra_args: self.config.extra_args.clone(),
        };
        let args: Vec<OsString> = cli_args(&launch, &caps)
            .into_iter()
            .map(OsString::from)
            .collect();
        Ok(DurableLaunch {
            process: ProcessSpec {
                program: self.config.program.clone(),
                args,
                cwd: cwd.clone(),
                env: self.config.env.clone(),
            },
            core: DurableConfig::Claude(CoreConfig {
                provider_instance: spec.provider_instance.clone(),
                cwd: cwd.to_string_lossy().into_owned(),
                capabilities: caps,
                resumed: None,
            }),
            stop: self.config.stop,
        })
    }
}
