//! The launch identity of a start (B9, DL-200): the station opens the
//! checked folder below the allowed roots without a link, and the provider
//! starts in exactly that directory object, or does not start.

use std::path::{Path, PathBuf};

use fleetdeck_core::launch_dir::LaunchDir;
use fleetdeck_proto::command::SessionSpec;

use super::SessionTarget;
use crate::store::{Dispatch, Effect};

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

/// The refusal of a launch without an identity (B9 §3.3 item 5), Linux only.
pub(super) fn launch_refusal(spec: &SessionSpec) -> Option<(&'static str, &'static str)> {
    (cfg!(target_os = "linux") && spec.launch.is_none())
        .then_some((LAUNCH_MISSING_CODE, LAUNCH_MISSING))
}
