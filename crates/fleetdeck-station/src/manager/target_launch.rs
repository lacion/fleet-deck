//! The launch identity of a start (B9, DL-200): the station opens the
//! checked folder below the allowed roots without a link, and the provider
//! starts in exactly that directory object, or does not start.

use std::path::{Path, PathBuf};

use fleetdeck_core::launch_dir::{LaunchDir, LaunchDirError};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::SessionSpec;
use fleetdeck_proto::session::SessionBase;

use super::super::SessionManager;
use super::SessionTarget;
use crate::store::{Dispatch, Effect, WorkspacePlan};
use crate::workspace::GitOptions;
use crate::workspace::base;
use crate::workspace::git::Confine;

/// The text of a launch without an identity.
pub(super) const LAUNCH_MISSING: &str =
    "The station did not check the session folder before the launch.";
/// The code of a launch without an identity.
pub(super) const LAUNCH_MISSING_CODE: &str = "launch_dir_missing";

/// The text of a launch on a target without a policy.
pub(super) const NO_POLICY: &str = "The station has no allowed roots for the session folder.";
/// The code of a launch on a target without a policy.
pub(super) const NO_POLICY_CODE: &str = "launch_dir_no_policy";

/// The checked folder of a start.
pub(in crate::manager) struct Launch {
    /// The spec with the identity (Linux; elsewhere `launch` stays `None`).
    pub(in crate::manager) spec: SessionSpec,
    /// The path for a Git read in the checked folder: `/proc/self/fd/<n>`
    /// on Linux, the folder path elsewhere.
    pub(in crate::manager) pinned: PathBuf,
    /// Keeps the folder open while `pinned` is used.
    _dir: LaunchDir,
}

impl SessionTarget {
    /// Takes the launch identity of `spec` (B9 §3.2): with an identity
    /// that this station set, `LaunchDir::reopen` (the folder must be the
    /// same object); without one, `LaunchDir::open_in` of `spec.cwd` below
    /// the current allowed roots. A failure fails the start with the text
    /// and returns the dispatch to return.
    pub(in crate::manager) async fn take_launch(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
    ) -> Result<Launch, Dispatch> {
        #[cfg(any(test, feature = "fault-injection"))]
        self.launch_hook.hold().await;
        let Some(policy) = self.current_policy() else {
            self.start_failed(&effect.session_id, NO_POLICY).await;
            return Err(Dispatch::NotDispatched(NO_POLICY_CODE.into()));
        };
        let roots = policy.allowed_roots();
        let opened = match spec.launch.as_deref() {
            Some(id) => LaunchDir::reopen(id),
            None => LaunchDir::open_in(roots, Path::new(&spec.cwd)),
        };
        // Linux: the identity and the /proc path; elsewhere the check only
        // (design §6).
        let result = opened.and_then(|dir| {
            #[cfg(target_os = "linux")]
            {
                let pinned = dir.cwd()?;
                let mut spec = spec.clone();
                spec.launch = Some(Box::new(dir.identity().clone()));
                Ok(Launch {
                    spec,
                    pinned,
                    _dir: dir,
                })
            }
            #[cfg(not(target_os = "linux"))]
            {
                let pinned = dir.path();
                Ok(Launch {
                    spec: spec.clone(),
                    pinned,
                    _dir: dir,
                })
            }
        });
        match result {
            Ok(launch) => Ok(launch),
            Err(e) => {
                self.start_failed(&effect.session_id, &e.to_string()).await;
                Err(Dispatch::NotDispatched(e.code().into()))
            }
        }
    }
}

impl SessionTarget {
    /// The Git options of the provisioning of a start (B9, DL-200): the
    /// station's own, run under `fleetdeck confine-exec` with the current
    /// allowed roots when the station knows its executable and has a policy.
    /// Without them (tests, other systems) Git runs as before and the
    /// identity check after Git detects a swap.
    pub(in crate::manager) fn provision_options(&self) -> GitOptions {
        let mut options = self.git.clone();
        if let (Some(exe), Some(policy)) = (self.confine_exe.get(), self.current_policy()) {
            options.confine = Some(Confine {
                exe: exe.clone(),
                roots: policy.allowed_roots().to_vec(),
            });
        }
        options
    }

    /// Opens `path` below the current allowed roots without a link (B9
    /// §3.3). A failure is `(text, code)`; the caller fails the start.
    pub(in crate::manager) async fn open_checked(
        &self,
        path: &Path,
    ) -> Result<LaunchDir, (String, &'static str)> {
        #[cfg(any(test, feature = "fault-injection"))]
        self.launch_hook.hold().await;
        let Some(policy) = self.current_policy() else {
            return Err((NO_POLICY.to_owned(), NO_POLICY_CODE));
        };
        LaunchDir::open_in(policy.allowed_roots(), path).map_err(|e| (e.to_string(), e.code()))
    }

    /// The checked folder for a child of the session (B9 §3.3 item 2): the
    /// identity of `spec` reopened, or `spec.cwd` opened below the roots. The
    /// path is `/proc/self/fd/<n>` on Linux, the folder path elsewhere; keep
    /// the `LaunchDir` alive until the spawn returned.
    pub(in crate::manager) async fn child_dir(
        &self,
        spec: &SessionSpec,
    ) -> Result<(LaunchDir, PathBuf), (String, &'static str)> {
        #[cfg(any(test, feature = "fault-injection"))]
        self.launch_hook.hold().await;
        let dir = match spec.launch.as_deref() {
            Some(id) => LaunchDir::reopen(id).map_err(|e| (e.to_string(), e.code()))?,
            None => self.open_checked(Path::new(&spec.cwd)).await?,
        };
        let path = pinned_path(&dir).map_err(|e| (e.to_string(), e.code()))?;
        Ok((dir, path))
    }

    /// Reopens the identity of `spec` after a Codex `session.started` (B9
    /// §3.5): a detection that the folder is still the checked object.
    pub(in crate::manager) async fn launch_still_checked(
        &self,
        spec: &SessionSpec,
    ) -> Result<(), LaunchDirError> {
        #[cfg(any(test, feature = "fault-injection"))]
        self.launch_hook.hold().await;
        match spec.launch.as_deref() {
            Some(id) => LaunchDir::reopen(id).map(|_| ()),
            None => Ok(()),
        }
    }

    /// Fails the start of `session_id` with the text of `e` (B9 §3.5) and
    /// returns the dispatch to return.
    pub(in crate::manager) async fn launch_changed(
        &self,
        session_id: &SessionId,
        e: LaunchDirError,
    ) -> Dispatch {
        self.start_failed(session_id, &e.to_string()).await;
        Dispatch::NotDispatched(e.code().into())
    }

    /// The base commit of a start (R1.1). A worktree plan reads it in the
    /// pinned source repository (B9, DL-200): the `LaunchDir` stays open
    /// until the read returned. A failure fails the start and returns the
    /// dispatch to return.
    pub(in crate::manager) async fn start_base(
        &self,
        effect: &Effect,
        workspace: Option<&WorkspacePlan>,
        spec: &SessionSpec,
        launch: Option<&Launch>,
        wants_base: bool,
    ) -> Result<Option<SessionBase>, Dispatch> {
        if !wants_base {
            return Ok(None);
        }
        if let Some(l) = launch {
            let pinned = l.pinned.to_string_lossy();
            return Ok(base::planned(&self.git, workspace, &pinned, None).await);
        }
        let mut source = None;
        if let Some(WorkspacePlan::Worktree { repo, .. }) = workspace {
            let opened = match self.open_checked(Path::new(repo)).await {
                Ok(dir) => pinned_path(&dir)
                    .map(|path| (dir, path))
                    .map_err(|e| (e.to_string(), e.code())),
                Err(refused) => Err(refused),
            };
            match opened {
                Ok(found) => source = Some(found),
                Err((text, code)) => {
                    self.start_failed(&effect.session_id, &text).await;
                    return Err(Dispatch::NotDispatched(code.into()));
                }
            }
        }
        let repo_dir = source.as_ref().map(|(_, path)| path.as_path());
        Ok(base::planned(&self.git, workspace, &spec.cwd, repo_dir).await)
    }
}

/// The path for a Git read in `dir`: `/proc/self/fd/<n>` on Linux, the
/// folder path elsewhere (design §6).
fn pinned_path(dir: &LaunchDir) -> Result<PathBuf, LaunchDirError> {
    #[cfg(target_os = "linux")]
    {
        dir.cwd()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(dir.path())
    }
}

impl SessionManager {
    /// Sets the `fleetdeck` executable that confines the Git of a worktree
    /// or a clone (B9, DL-200). A second call is ignored.
    pub fn set_confine_exe(&self, exe: PathBuf) {
        let _ = self.target.confine_exe.set(exe);
    }
}

impl std::fmt::Debug for SessionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTarget")
            .field("adapters", &self.adapters.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "launch_more_tests.rs"]
mod more_tests;

/// The refusal of a launch without an identity (B9 §3.3 item 5), Linux only.
pub(super) fn launch_refusal(spec: &SessionSpec) -> Option<(&'static str, &'static str)> {
    (cfg!(target_os = "linux") && spec.launch.is_none())
        .then_some((LAUNCH_MISSING_CODE, LAUNCH_MISSING))
}
