//! The launch identity of a start (B9, DL-200): the station opens the
//! checked folder below the allowed roots without a link, and the provider
//! starts in exactly that directory object, or does not start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::launch_dir::{LaunchDir, LaunchDirError};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::SessionSpec;
use fleetdeck_proto::fleet_git::{FleetGitStart, GitSettings, SkippedFile, check_settings};
use fleetdeck_proto::session::SessionBase;
use tokio::sync::watch;

use super::super::SessionManager;
use super::super::turns::SessionTurns;
use super::SessionTarget;
use crate::fleet_git_runtime::FleetGitRuntime;
use crate::store::{Dispatch, Effect, NewEvent, WorkspacePlan};
use crate::workspace::GitOptions;
use crate::workspace::base;
use crate::workspace::fleet_git::{self, BaseStale};
use crate::workspace::git::Confine;
use crate::workspace::provision::ProvisionError;
use crate::workspace::setup::STOPPED_MESSAGE;

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
        #[allow(clippy::bind_instead_of_map, reason = "only the Linux branch can fail")]
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
    /// The checked folder for the checkpoint start-up of `spec` (B9): the
    /// identity reopened and its pinned path. `None` on any error or without
    /// an identity: checkpoints are optional (a failed `SessionTurns::open`
    /// already means no checkpoints).
    pub(in crate::manager) fn turns_dir(&self, spec: &SessionSpec) -> Option<(LaunchDir, PathBuf)> {
        let dir = LaunchDir::reopen(spec.launch.as_deref()?).ok()?;
        let path = pinned_path(&dir).ok()?;
        Some((dir, path))
    }

    /// The checkpoint state of a new session of `spec` (B9). With an identity
    /// the preparation Git of `SessionTurns::open` runs in the reopened
    /// folder object, and its toplevel must be below the allowed roots
    /// without a link; when the folder cannot be reopened the session runs
    /// without checkpoints. Without an identity (tests) the plain path.
    pub(in crate::manager) async fn open_turns(
        &self,
        session_id: &SessionId,
        spec: &SessionSpec,
    ) -> Arc<SessionTurns> {
        // `dir` lives until the start-up Git returned.
        let (dir, path, roots) = if spec.launch.is_none() {
            (None, Some(PathBuf::from(&spec.cwd)), None)
        } else if let Some((dir, path)) = self.turns_dir(spec) {
            let roots = self
                .current_policy()
                .map(|policy| policy.allowed_roots().to_vec());
            (Some(dir), Some(path), roots)
        } else {
            tracing::warn!(
                "checkpoints are off for this session: its folder could not be reopened"
            );
            (None, None, None)
        };
        let turns = SessionTurns::open(
            self.store.clone(),
            Arc::clone(&self.ids),
            self.git.clone(),
            self.limits,
            &self.fences,
            session_id.clone(),
            path.as_deref(),
            roots.as_deref(),
            Arc::clone(&self.private),
        )
        .await;
        drop(dir);
        turns
    }

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
pub(in crate::manager) fn pinned_path(dir: &LaunchDir) -> Result<PathBuf, LaunchDirError> {
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

/// True when a start has an artifact share: its artifact step runs in the
/// task of the start, so the start is deferred (FG-12, design §15.5).
#[allow(clippy::ref_option)]
pub(in crate::manager) fn artifacts(fleet_git: &Option<Box<FleetGitStart>>) -> bool {
    crate::artifact_share::wanted(fleet_git.as_deref())
}

/// Adds the artifact variables to the provider spec (`extra_env`) and to the
/// environment of the setup command.
fn add_artifact_vars(
    spec: &mut SessionSpec,
    env: Option<&mut ChildEnv>,
    vars: &BTreeMap<String, String>,
) {
    if vars.is_empty() {
        return;
    }
    let mut extra = spec.extra_env.as_deref().cloned().unwrap_or_default();
    extra.extend(vars.clone());
    spec.extra_env = Some(Box::new(extra));
    if let Some(env) = env {
        for (name, value) in vars {
            if let Err(e) = env.set(name, value) {
                tracing::warn!(variable = %name, error = %e, "the artifact variable is dropped");
            }
        }
    }
}

impl SessionTarget {
    /// Gives the target the Fleet Git runtime for the artifact share (FG-12).
    pub(in crate::manager) fn set_artifacts_runtime(&self, runtime: Arc<FleetGitRuntime>) {
        let _ = self.artifacts_runtime.set(runtime);
    }

    /// The artifact step of a session (design §15.5, decision C1): the
    /// authorization, then the folder or the address. It appends
    /// `artifacts.ready` or `artifacts.unavailable` and adds the variables to
    /// `spec` and to the setup command `env`. It never fails the start.
    async fn run_artifact_step(
        &self,
        session_id: &SessionId,
        project: (&str, &str),
        share: &str,
        spec: &mut SessionSpec,
        env: Option<&mut ChildEnv>,
    ) {
        use crate::artifact_share::{StepInput, report_unavailable, start_step};
        let Some(runtime) = self.artifacts_runtime.get() else {
            let unverified = fleetdeck_proto::fleet_git::code::SHARE_UNVERIFIED;
            report_unavailable(&self.store, session_id, unverified).await;
            return;
        };
        let callsign = match self.store.summary(session_id.clone()).await {
            Ok(Some(summary)) => summary.callsign.unwrap_or_default(),
            _ => String::new(),
        };
        let input = StepInput {
            session_id,
            project_id: project.0,
            project_name: project.1,
            share,
            callsign: &callsign,
        };
        let vars = start_step(runtime, &self.store, &input).await;
        add_artifact_vars(spec, env, &vars);
    }

    /// The artifact step of a new session that has an artifact share
    /// (`prepare_then_start`, before the setup command).
    pub(in crate::manager) async fn artifact_step(
        &self,
        session_id: &SessionId,
        fg: &FleetGitStart,
        spec: &mut SessionSpec,
        env: Option<&mut ChildEnv>,
    ) {
        let Some(share) = fg.settings.artifact_share.as_deref() else {
            return;
        };
        let project = (fg.project_id.as_str(), fg.project_name.as_str());
        self.run_artifact_step(session_id, project, share, spec, env)
            .await;
    }

    /// The artifact step of a resume (design §15.5 item 5): only a session
    /// that has an artifact folder or address. The project's current share
    /// must be the share of the stored authorization (decision C7); else the
    /// session resumes without the variables.
    pub(in crate::manager) async fn resume_artifacts(
        &self,
        effect: &Effect,
        mut launch: Launch,
    ) -> Launch {
        use crate::artifact_share::report_unavailable;
        let session_id = &effect.session_id;
        let Ok(Some(row)) = self.store.artifacts_row(session_id.clone()).await else {
            return launch;
        };
        if row.path.is_none() && row.url.is_none() {
            return launch;
        }
        let view = match self.store.git_project(row.project_id.clone()).await {
            Ok(found) => found.map(|p| p.view),
            Err(_) => None,
        };
        let stored = row.auth.as_ref().map(|a| a.share.as_str());
        let share = view
            .as_ref()
            .and_then(|v| v.settings.artifact_share.as_deref())
            .filter(|current| Some(*current) == stored);
        let (Some(view), Some(share)) = (&view, share) else {
            let unverified = fleetdeck_proto::fleet_git::code::SHARE_UNVERIFIED;
            report_unavailable(&self.store, session_id, unverified).await;
            return launch;
        };
        let project = (view.project_id.as_str(), view.project_name.as_str());
        self.run_artifact_step(session_id, project, share, &mut launch.spec, None)
            .await;
        launch
    }
}

impl std::fmt::Debug for SessionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTarget")
            .field("adapters", &self.adapters.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// What the Git work of a synced start found (FG-1, FG-4).
#[derive(Default)]
pub(in crate::manager) struct Prepared {
    /// The fetch failed: the session started from the local `HEAD`.
    pub(in crate::manager) stale: Option<BaseStale>,
    /// The base of the fetched start point.
    pub(in crate::manager) base: Option<SessionBase>,
    /// The ignored files that were not copied (at most 50).
    pub(in crate::manager) skipped: Vec<SkippedFile>,
}

/// The names of `worktree_env` that the station accepts, after the same rule
/// as the deck (design §7.3). A refused name is dropped with a log line.
fn accepted_worktree_env(settings: &GitSettings) -> BTreeMap<String, String> {
    let mut accepted = BTreeMap::new();
    for (name, value) in settings.worktree_env.iter().take(32) {
        let one = GitSettings {
            worktree_env: std::iter::once((name.clone(), value.clone())).collect(),
            ..GitSettings::default()
        };
        if check_settings(&one).is_err() {
            tracing::warn!(variable = %name, "worktree_env: a refused variable is dropped");
        } else {
            accepted.insert(name.clone(), value.clone());
        }
    }
    accepted
}

/// Adds the names of `worktree_env` that the station accepts. A name that the
/// environment has already (the provider instance `env`) keeps its value.
pub(in crate::manager) fn add_worktree_env(env: &mut ChildEnv, settings: &GitSettings) {
    for (name, value) in accepted_worktree_env(settings) {
        if env.get(&name).is_none()
            && let Err(e) = env.set(&name, &value)
        {
            tracing::warn!(variable = %name, error = %e, "worktree_env: a variable is dropped");
        }
    }
}

/// The spec of a start with the `worktree_env` of its project in `extra_env`
/// (FG-4, O15), for the provider process. `None`: the start has no Fleet Git
/// data or no accepted variable, and the spec stays as it is.
pub(in crate::manager) fn spec_with_worktree_env(
    spec: &SessionSpec,
    fleet_git: Option<&FleetGitStart>,
) -> Option<SessionSpec> {
    let accepted = accepted_worktree_env(&fleet_git?.settings);
    (!accepted.is_empty()).then(|| SessionSpec {
        extra_env: Some(Box::new(accepted)),
        ..spec.clone()
    })
}

impl SessionTarget {
    /// The fetch before the worktree (FG-1), the worktree, and the copy of the
    /// ignored files (FG-4). A plan without Fleet Git data is only the
    /// worktree or the clone.
    pub(in crate::manager) async fn prepare_synced(
        &self,
        effect: &Effect,
        plan: &WorkspacePlan,
        fleet: Option<&FleetGitStart>,
        wants_base: bool,
        cancel: &watch::Receiver<bool>,
    ) -> Result<Prepared, (String, &'static str)> {
        let mut prepared = Prepared::default();
        let mut run = plan.clone();
        if let Some(fg) = fleet {
            (run, prepared) = self.fetch_before(plan, fg, wants_base, cancel).await?;
            if prepared.stale.is_some()
                && let Err(e) = self
                    .store
                    .git_set_base_stale(effect.session_id.clone())
                    .await
            {
                tracing::warn!(error = %e, "base_stale was not stored");
            }
        }
        let created =
            matches!(plan, WorkspacePlan::Worktree { path, .. } if !Path::new(path).exists());
        self.provision_plan(&run, cancel).await?;
        if let (true, Some(fg), WorkspacePlan::Worktree { path, .. }) = (created, fleet, plan)
            && !fg.settings.worktree_include.is_empty()
        {
            prepared.skipped = fleet_git::copy_includes(
                Path::new(&fg.home_path),
                Path::new(path),
                &fg.settings.worktree_include,
                &self.provision_options(),
            )
            .await;
        }
        Ok(prepared)
    }

    /// The fetch of a synced worktree plan (FG-1, design §4.2 step 2): the
    /// plan that starts from the fetched remote, and the base to record. A
    /// plan without `sync` is unchanged.
    pub(in crate::manager) async fn fetch_before(
        &self,
        plan: &WorkspacePlan,
        fg: &FleetGitStart,
        wants_base: bool,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(WorkspacePlan, Prepared), (String, &'static str)> {
        use crate::workspace::{base, provision};
        let WorkspacePlan::Worktree {
            repo,
            branch,
            base: owner_base,
            sync: Some(sync),
            ..
        } = plan
        else {
            return Ok((plan.clone(), Prepared::default()));
        };
        if *cancel.borrow() {
            return Err((STOPPED_MESSAGE.to_owned(), "setup_stopped"));
        }
        let dir = self.open_checked(Path::new(repo)).await?;
        let pinned = pinned_path(&dir).map_err(|e| (e.to_string(), e.code()))?;
        let options = self.provision_options();
        let group = Arc::new(AtomicU32::new(0));
        let callsign = branch.rsplit('/').next().unwrap_or(branch);
        let mut stop = cancel.clone();
        let done = tokio::select! {
            r = fleet_git::fetch_for_start(
                &options, &group, &pinned, &sync.remote, &fg.settings, &fg.slug, callsign,
                owner_base.as_deref(),
            ) => Some(r),
            _ = stop.wait_for(|c| *c) => None,
        };
        let Some(point) = done else {
            return Err(
                match provision::after_cancel(group.load(Ordering::SeqCst), Duration::from_secs(5))
                    .await
                {
                    ProvisionError::Stopped => (STOPPED_MESSAGE.to_owned(), "setup_stopped"),
                    ProvisionError::Failed(e) => (
                        format!("the workspace was not created: {e}"),
                        "workspace_failed",
                    ),
                },
            );
        };
        let point = point.map_err(|refusal| (refusal.text, refusal.code))?;
        let mut next = plan.clone();
        if let (WorkspacePlan::Worktree { base, .. }, Some(rev)) = (&mut next, point.rev) {
            *base = Some(rev);
        }
        // The base of the session is read after the fetch, before the
        // worktree exists (R1.1).
        let session_base = if wants_base {
            base::planned(&self.git, Some(&next), "", Some(&pinned)).await
        } else {
            None
        };
        let prepared = Prepared {
            stale: point.stale,
            base: session_base,
            skipped: Vec::new(),
        };
        Ok((next, prepared))
    }

    /// The events of the Git work of a synced start (FG-1, FG-4), after the
    /// provider start (design §4.2 step 3).
    pub(in crate::manager) async fn record_prepared(
        &self,
        session_id: &SessionId,
        prepared: &Prepared,
    ) {
        let mut events = Vec::new();
        if let Some(stale) = &prepared.stale {
            events.push(NewEvent::from(fleetdeck_proto::Event::WorkspaceBaseStale {
                code: stale.code.to_owned(),
                text: stale.reason.clone(),
            }));
        }
        if !prepared.skipped.is_empty() {
            events.push(NewEvent::from(
                fleetdeck_proto::Event::WorkspaceIncludeSkipped {
                    files: prepared.skipped.clone(),
                },
            ));
        }
        if events.is_empty() {
            return;
        }
        if let Err(e) = self.store.append(session_id.clone(), events).await {
            tracing::warn!(error = %e, "the Fleet Git start events were not stored");
        }
    }
}

#[cfg(test)]
#[path = "launch_more_tests.rs"]
mod more_tests;

#[cfg(test)]
#[path = "launch_fix_tests.rs"]
mod fix_tests;

/// The refusal of a launch without an identity (B9 §3.3 item 5), Linux only.
pub(super) fn launch_refusal(spec: &SessionSpec) -> Option<(&'static str, &'static str)> {
    (cfg!(target_os = "linux") && spec.launch.is_none())
        .then_some((LAUNCH_MISSING_CODE, LAUNCH_MISSING))
}
