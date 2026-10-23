//! The durable launch of a Claude session (DL-113): the direct start's
//! workspace, capability and argument rules, without a process.

use std::ffi::OsString;
use std::path::Path;

use fleetdeck_proto::command::SessionSpec;

use super::super::core::CoreConfig;
use super::super::{Launch, capabilities, cli_args};
use super::{ClaudeAdapter, env_with_extra};
use crate::adapter::{NativeSessionId, Result, canonical_workspace};
use crate::durable::{DurableConfig, DurableLaunch};
use crate::host::{ProcessSpec, identity_path};

impl ClaudeAdapter {
    pub(super) fn launch_durable(&self, spec: &SessionSpec) -> Result<DurableLaunch> {
        self.launch_durable_as(spec, None)
    }

    /// The launch of a new session, or with `resume` the native resume of
    /// that conversation (DL-182): `--resume <id>` and `resumed = true`.
    /// The mode rules are those of a new session.
    pub(super) fn launch_durable_as(
        &self,
        spec: &SessionSpec,
        resume: Option<&NativeSessionId>,
    ) -> Result<DurableLaunch> {
        // With a launch identity the path is the checked one, never resolved
        // again by name (B9).
        let (cwd, pinned) = match &spec.launch {
            Some(id) => (identity_path(id), Some(id.as_ref().clone())),
            None => (canonical_workspace(Path::new(&spec.cwd))?, None),
        };
        let caps = capabilities(self.config.allow_bypass);
        let launch = Launch {
            model: spec.model.clone(),
            mode: spec.mode.clone(),
            resume: resume.cloned(),
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
                env: super::apps::with_apps_host(env_with_extra(
                    &self.config.env,
                    spec.extra_env.as_deref(),
                )),
                launch: pinned,
            },
            core: DurableConfig::Claude(CoreConfig {
                provider_instance: spec.provider_instance.clone(),
                cwd: cwd.to_string_lossy().into_owned(),
                capabilities: caps,
                resumed: resume.map(|_| true),
            }),
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
    use crate::claude::{ClaudeAdapter, ClaudeAdapterConfig};
    use crate::host::DirectChildHost;

    fn adapter() -> ClaudeAdapter {
        let config =
            ClaudeAdapterConfig::new("claude".into(), "claude".into(), ChildEnv::from_process());
        ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()))
    }

    fn spec(dir: &std::path::Path, mode: Option<&str>) -> SessionSpec {
        SessionSpec {
            provider_instance: "claude".into(),
            cwd: dir.to_string_lossy().into_owned(),
            mode: mode.map(str::to_owned),
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        }
    }

    fn flag(args: &[OsString], name: &str) -> Option<String> {
        let at = args.iter().position(|a| a == name)?;
        args.get(at + 1).map(|a| a.to_string_lossy().into_owned())
    }

    #[test]
    fn a_durable_resume_has_the_resume_flag_the_stored_mode_and_the_resumed_marker() {
        let dir = tempfile::tempdir().unwrap();
        let id = NativeSessionId("native-1".to_owned());
        let launch = adapter()
            .durable_resume_launch(&spec(dir.path(), Some("acceptEdits")), &id)
            .unwrap();
        assert_eq!(
            flag(&launch.process.args, "--resume").as_deref(),
            Some("native-1")
        );
        assert_eq!(
            flag(&launch.process.args, "--permission-mode").as_deref(),
            Some("acceptEdits")
        );
        assert!(!launch.process.args.iter().any(|a| a == "--fork-session"));
        let DurableConfig::Claude(core) = launch.core else {
            panic!("a Claude core");
        };
        assert_eq!(core.resumed, Some(true));
    }

    #[test]
    fn a_durable_resume_runs_an_unoffered_mode_as_the_safest_mode() {
        let dir = tempfile::tempdir().unwrap();
        let id = NativeSessionId("native-1".to_owned());
        let launch = adapter()
            .durable_resume_launch(&spec(dir.path(), Some("a_mode_of_a_later_build")), &id)
            .unwrap();
        assert_eq!(
            flag(&launch.process.args, "--permission-mode").as_deref(),
            Some("default")
        );
    }

    #[test]
    fn a_new_durable_launch_has_no_resume_flag_and_no_resumed_marker() {
        let dir = tempfile::tempdir().unwrap();
        let launch = adapter().durable_launch(&spec(dir.path(), None)).unwrap();
        assert!(!launch.process.args.iter().any(|a| a == "--resume"));
        let DurableConfig::Claude(core) = launch.core else {
            panic!("a Claude core");
        };
        assert_eq!(core.resumed, None);
    }

    #[test]
    fn an_extra_env_entry_reaches_the_process_and_the_instance_env_wins() {
        let dir = tempfile::tempdir().unwrap();
        let mut base = ChildEnv::from_process();
        base.set("FG_CLASH", "instance").unwrap();
        let config = ClaudeAdapterConfig::new("claude".into(), "claude".into(), base);
        let adapter = ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()));
        let mut with = spec(dir.path(), None);
        with.extra_env = Some(Box::new(std::collections::BTreeMap::from([
            ("FG_TEST_FLAG".to_owned(), "sccache".to_owned()),
            ("FG_CLASH".to_owned(), "project".to_owned()),
        ])));
        let launch = adapter.durable_launch(&with).unwrap();
        let env = &launch.process.env;
        assert_eq!(env.get("FG_TEST_FLAG").unwrap(), "sccache");
        assert_eq!(env.get("FG_CLASH").unwrap(), "instance");
        // Without `extra_env` the environment is the instance environment.
        let launch = adapter.durable_launch(&spec(dir.path(), None)).unwrap();
        assert!(launch.process.env.get("FG_TEST_FLAG").is_none());
    }

    #[test]
    fn a_launch_identity_gives_the_checked_path_and_no_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let id = fleetdeck_proto::launch::LaunchIdentity {
            root: root.to_string_lossy().into_owned(),
            rel: "link".to_owned(),
            dev: 1,
            ino: 2,
        };
        let mut with = spec(&root.join("link"), None);
        with.launch = Some(Box::new(id.clone()));
        let launch = adapter().durable_launch(&with).unwrap();
        assert_eq!(launch.process.cwd, root.join("link"));
        assert_eq!(launch.process.launch, Some(id));
        // Without an identity the link is followed (today's behavior).
        let launch = adapter()
            .durable_launch(&spec(&root.join("link"), None))
            .unwrap();
        assert_eq!(launch.process.cwd, root.join("real"));
        assert_eq!(launch.process.launch, None);
    }
}
