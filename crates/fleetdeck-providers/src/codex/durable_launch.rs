//! The durable launch of a Codex session (DL-113): the direct start's
//! workspace, capability, mode and thread rules, without a process.

use std::ffi::OsString;
use std::path::Path;

use fleetdeck_proto::command::SessionSpec;

use super::super::{CoreConfig, app_server_args, capabilities};
use super::CodexAdapter;
use crate::adapter::{NativeSessionId, ProviderError, Result, canonical_workspace};
use crate::durable::{DurableConfig, DurableLaunch};
use crate::host::ProcessSpec;

impl CodexAdapter {
    pub(super) fn launch_durable(&self, spec: &SessionSpec) -> Result<DurableLaunch> {
        self.launch_durable_as(spec, None)
    }

    /// The launch of a new session, or with `resume` the native resume of
    /// that thread (DL-182): the handshake sends `thread/resume` and
    /// `resumed = true`. An unoffered mode is refused before any process.
    pub(super) fn launch_durable_as(
        &self,
        spec: &SessionSpec,
        resume: Option<&NativeSessionId>,
    ) -> Result<DurableLaunch> {
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
                    resumed: resume.map(|_| true),
                    client: self.config.client.clone(),
                    request_models: self.config.request_models,
                },
                thread: super::super::SessionSpec {
                    model: spec.model.clone(),
                    effort: self.config.effort.clone(),
                    mode: spec.mode.clone(),
                    cwd: Some(cwd_text),
                    resume: resume.map(|id| id.0.clone()),
                    fork: false,
                },
            },
            stop: self.config.stop,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use fleetdeck_core::ChildEnv;

    use super::*;
    use crate::adapter::ProviderAdapter as _;
    use crate::codex::{CodexAdapter, CodexAdapterConfig};
    use crate::host::DirectChildHost;

    fn adapter() -> CodexAdapter {
        let config =
            CodexAdapterConfig::new("codex".into(), "codex".into(), ChildEnv::from_process());
        CodexAdapter::new(config, Arc::new(DirectChildHost::new()))
    }

    fn spec(dir: &std::path::Path, mode: Option<&str>) -> SessionSpec {
        SessionSpec {
            provider_instance: "codex".into(),
            cwd: dir.to_string_lossy().into_owned(),
            mode: mode.map(str::to_owned),
            model: None,
            title: None,
            coordinator: None,
        }
    }

    #[test]
    fn a_durable_resume_opens_the_thread_with_its_id_and_marks_the_core_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let id = NativeSessionId("thread-1".to_owned());
        let launch = adapter()
            .durable_resume_launch(&spec(dir.path(), None), &id)
            .unwrap();
        let DurableConfig::Codex { config, thread } = launch.core else {
            panic!("a Codex core");
        };
        assert_eq!(thread.resume.as_deref(), Some("thread-1"));
        assert!(!thread.fork);
        assert_eq!(config.resumed, Some(true));
    }

    #[test]
    fn a_durable_resume_refuses_an_unoffered_mode_before_any_process() {
        let dir = tempfile::tempdir().unwrap();
        let id = NativeSessionId("thread-1".to_owned());
        let err = adapter()
            .durable_resume_launch(&spec(dir.path(), Some("a_mode_of_a_later_build")), &id)
            .unwrap_err();
        assert!(matches!(err, ProviderError::InvalidMode(_)), "{err:?}");
    }

    #[test]
    fn a_new_durable_launch_opens_a_new_thread() {
        let dir = tempfile::tempdir().unwrap();
        let launch = adapter().durable_launch(&spec(dir.path(), None)).unwrap();
        let DurableConfig::Codex { config, thread } = launch.core else {
            panic!("a Codex core");
        };
        assert_eq!(thread.resume, None);
        assert_eq!(config.resumed, None);
    }
}
