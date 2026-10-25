//! `project.ensure_home` and `project.home_status` on the station (design
//! §8.4, FG-5): make a home of a project by a clone, or adopt a complete
//! clone that already exists. The deck resends the same request until it
//! gets an answer, so every step is idempotent by `attempt_id`.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::fleet_git_digest::{ensure_home_digest, home_folder_name};
use fleetdeck_proto::fleet_git::{
    EnsureHomeParams, GitProjectView, GitSettings, HomeAttemptState, HomeCheckState, HomeStatus,
    HomeStatusParams, LocalRemotes, check_settings, code, remote_identity,
};
use tokio::sync::OnceCell;

use super::identity::{READ_TIMEOUT, check_home, read, read_git};
use super::redact::redact;
use crate::manager::policy::{Policy, WorkspaceError};
use crate::store::Store;
use crate::store::home_attempts::{
    BeginHome, HomeAttemptRow, HomeOutcome, HomeRefusal, NewHomeAttempt,
};
use crate::workspace::provision;
use crate::workspace::{Git, GitOptions};

/// The most bytes of the answer of a small read.
const SMALL_CAP: usize = 4 * 1024;
/// The text of the code `unknown_attempt` (design S3).
const UNKNOWN_ATTEMPT: &str = "The station does not know this home attempt.";
/// The code of an adopt path whose folder does not exist (never cloned into).
const HOME_MISSING: &str = "home_missing";
/// The most bytes of an adopt path.
const ADOPT_PATH_MAX: usize = 4096;

fn refusal(code: &'static str, text: impl Into<String>) -> HomeRefusal {
    HomeRefusal {
        code,
        text: text.into(),
    }
}

fn invalid(text: impl Into<String>) -> HomeRefusal {
    refusal(code::INVALID_PARAMS, text)
}

fn internal(e: impl std::fmt::Display) -> HomeRefusal {
    refusal(code::INTERNAL, e.to_string())
}

/// The state that `project.ensure_home` and `project.home_status` answer.
/// The verified path is only for `ready` and `adopted`; the reserved folder
/// is for `cloning`, `failed` and `interrupted`.
fn status_of(row: &HomeAttemptRow) -> HomeStatus {
    let done = matches!(
        row.state,
        HomeAttemptState::Ready | HomeAttemptState::Adopted
    );
    let reserved = matches!(
        row.state,
        HomeAttemptState::Cloning | HomeAttemptState::Failed | HomeAttemptState::Interrupted
    );
    HomeStatus {
        attempt_id: row.attempt_id.clone(),
        generation: row.generation,
        state: row.state,
        path: row.path.clone().filter(|_| done),
        reserved_path: reserved.then(|| row.dest_path.clone()),
        code: row.code.clone(),
        message: row.message.clone(),
    }
}

/// The checks of design S4, in order. Each refusal is `invalid_params`.
fn validate(policy: &Policy, params: &EnsureHomeParams) -> Result<(), HomeRefusal> {
    let id = &params.project_id;
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err(invalid("The project id is not valid."));
    }
    let name = &params.project_name;
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid("The project name is not valid."));
    }
    if params.folder != home_folder_name(name) {
        return Err(invalid("The folder name does not match the project name."));
    }
    if let Some(p) = &params.adopt_path
        && (!p.starts_with('/') || p.len() > ADOPT_PATH_MAX || p.contains('\0'))
    {
        return Err(invalid("Give an absolute folder path."));
    }
    let remote_settings = GitSettings {
        push_remote: params.remote_name.clone(),
        ..GitSettings::default()
    };
    if let Err(e) = check_settings(&remote_settings) {
        return Err(invalid(e.message));
    }
    let inside = |p: &Path| policy.workspace(&p.to_string_lossy()).ok();
    provision::check_url(&params.url, inside).map_err(|r| invalid(r.message))?;
    let local = if policy.allow_local_remotes() {
        LocalRemotes::Allow
    } else {
        LocalRemotes::Deny
    };
    match remote_identity(&params.url, local) {
        Err(e) => Err(invalid(e.to_string())),
        Ok(seen) if seen != params.remote => {
            Err(invalid("The remote identity does not match the URL."))
        }
        Ok(_) => {
            if ensure_home_digest(params) == params.request_digest {
                Ok(())
            } else {
                Err(refusal(
                    code::ATTEMPT_REUSED,
                    "This attempt id was used for another request.",
                ))
            }
        }
    }
}

/// The canonical `clone_root` (design §8.4 step 8): an existing folder inside
/// the allowed roots, or a new one that is made with mode 0700 when its
/// parent exists and is an allowed root or inside one.
fn clone_root(policy: &Policy) -> Result<PathBuf, HomeRefusal> {
    let root = policy.clone_root();
    let text = root.to_string_lossy().into_owned();
    let missing = || {
        refusal(
            code::CLONE_ROOT_MISSING,
            format!("The clone folder {text} does not exist and cannot be made."),
        )
    };
    match policy.workspace(&text) {
        Ok(canonical) => return Ok(canonical),
        Err(WorkspaceError::Outside(_)) => return Err(missing()),
        Err(WorkspaceError::Missing(_)) => {}
    }
    let parent = root.parent().ok_or_else(missing)?;
    policy
        .workspace(&parent.to_string_lossy())
        .map_err(|_| missing())?;
    let made = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(root)
    };
    match made {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(missing()),
    }
    policy.workspace(&text).map_err(|_| missing())
}

/// `project.ensure_home` (design §8.4). The answer is the stored status of
/// the attempt: `cloning` at once for a clone, or the end of an adoption.
///
/// `options` are the confined Git options of a clone (the clone timeout of the
/// policy is set here); the home checks use them without the confinement.
///
/// # Errors
/// A [`HomeRefusal`]: a link error with its code and text.
pub async fn ensure_home(
    store: &Store,
    policy: Arc<Policy>,
    options: GitOptions,
    params: EnsureHomeParams,
) -> Result<HomeStatus, HomeRefusal> {
    validate(&policy, &params)?;
    // An adopt path is used as given and never gets a clone; else the folder
    // is below the `clone_root`.
    let (root, dest) = match &params.adopt_path {
        Some(p) => (None, PathBuf::from(p)),
        None => {
            let root = clone_root(&policy)?;
            let dest = root.join(&params.folder);
            (Some(root), dest)
        }
    };
    let begin = store
        .home_begin(NewHomeAttempt {
            attempt_id: params.attempt_id.clone(),
            project_id: params.project_id.clone(),
            generation: params.generation,
            request_digest: params.request_digest.clone(),
            retry_of: params.retry_of.clone(),
            dest_path: dest.to_string_lossy().into_owned(),
            url: params.url.clone(),
            remote_name: params.remote_name.clone(),
        })
        .await
        .map_err(internal)?;
    let row = match begin {
        BeginHome::Existing(row) => return Ok(status_of(&row)),
        BeginHome::Refused(r) => return Err(r),
        BeginHome::Created(row) => row,
    };
    if std::fs::symlink_metadata(&dest).is_ok() {
        // Step 6: the folder exists. Adopt it only when it is a complete clone.
        let outcome = match verify(&options, &policy, &dest, &params).await {
            Ok(v) => outcome_of(HomeAttemptState::Adopted, v),
            Err(f) => failed(f),
        };
        return finish(store, &row.attempt_id, outcome).await;
    }
    let Some(root) = root else {
        // An adopt path that does not exist: nothing is cloned or made.
        let outcome = failed(Failure {
            code: HOME_MISSING,
            text: format!(
                "The folder {} does not exist on this station.",
                dest.display()
            ),
        });
        return finish(store, &row.attempt_id, outcome).await;
    };
    // Step 7: the clone runs in a task; the answer is `cloning`.
    let task_store = store.clone();
    tokio::spawn(async move {
        let outcome = clone_home(&options, &policy, &root, &dest, &params).await;
        if let Err(e) = finish(&task_store, &params.attempt_id, outcome).await {
            tracing::warn!(attempt = %params.attempt_id, code = e.code, "the home attempt did not store its end");
        }
    });
    Ok(status_of(&row))
}

/// `project.home_status` (design §8.4): the stored status of an attempt.
///
/// # Errors
/// `unknown_attempt` when the station has no such attempt of the project.
pub async fn home_status(
    store: &Store,
    params: HomeStatusParams,
) -> Result<HomeStatus, HomeRefusal> {
    match store
        .home_attempt(params.project_id, params.attempt_id)
        .await
        .map_err(internal)?
    {
        Some(row) => Ok(status_of(&row)),
        None => Err(refusal(code::UNKNOWN_ATTEMPT, UNKNOWN_ATTEMPT)),
    }
}

async fn finish(
    store: &Store,
    attempt_id: &str,
    outcome: HomeOutcome,
) -> Result<HomeStatus, HomeRefusal> {
    store
        .home_finish(attempt_id.to_owned(), outcome)
        .await
        .map_err(internal)?
        .map(|row| status_of(&row))
        .ok_or_else(|| internal("the home attempt is gone"))
}

/// A folder that passed [`verify`].
struct Verified {
    path: String,
    dev: u64,
    ino: u64,
}

/// Why a folder is not a home: a code and the text.
struct Failure {
    code: &'static str,
    text: String,
}

fn outcome_of(state: HomeAttemptState, v: Verified) -> HomeOutcome {
    HomeOutcome {
        state,
        path: Some(v.path),
        dev: Some(v.dev),
        ino: Some(v.ino),
        code: None,
        message: None,
    }
}

fn failed(f: Failure) -> HomeOutcome {
    HomeOutcome {
        state: HomeAttemptState::Failed,
        path: None,
        dev: None,
        ino: None,
        code: Some(f.code.to_owned()),
        message: Some(f.text),
    }
}

/// The checks of design §8.4 step 6: a real folder inside the allowed roots,
/// the top of a repository with a commit, and the home check of §3.8 for the
/// remote `remote_name`.
async fn verify(
    options: &GitOptions,
    policy: &Policy,
    dest: &Path,
    params: &EnsureHomeParams,
) -> Result<Verified, Failure> {
    let shown = dest.display();
    let conflict = || Failure {
        code: code::HOME_CONFLICT,
        text: format!(
            "{shown} exists and is not a complete clone of {}.",
            params.remote.repo
        ),
    };
    let real = std::fs::symlink_metadata(dest).is_ok_and(|m| m.file_type().is_dir());
    let inside = policy
        .workspace(&dest.to_string_lossy())
        .is_ok_and(|canonical| canonical == dest);
    if !real || !inside {
        return Err(conflict());
    }
    let git = read_git(options, dest, READ_TIMEOUT);
    let head = ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"];
    if !matches!(read(&git, &head, SMALL_CAP).await, Ok((0, _, _))) {
        return Err(conflict());
    }
    let view = GitProjectView {
        project_id: params.project_id.clone(),
        project_name: params.project_name.clone(),
        settings_revision: 0,
        settings: GitSettings {
            push_remote: params.remote_name.clone(),
            ..GitSettings::default()
        },
        remote: params.remote.clone(),
        forge: None,
        clone_url: params.url.clone(),
        home_generation: params.generation,
        home_path: dest.to_string_lossy().into_owned(),
        radar_home: false,
    };
    let check = check_home(options, dest, &view, policy.allow_local_remotes()).await;
    match check.state {
        HomeCheckState::Verified => {}
        HomeCheckState::NoRemote => {
            return Err(Failure {
                code: code::HOME_CONFLICT,
                text: format!("{shown} exists but has no remote {}.", params.remote_name),
            });
        }
        _ => return Err(conflict()),
    }
    let path = check
        .canonical_path
        .unwrap_or_else(|| dest.to_string_lossy().into_owned());
    let meta = std::fs::metadata(&path).map_err(|_| conflict())?;
    Ok(Verified {
        path,
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Step 7: `git clone --origin <remote_name>` into the reserved folder, then
/// the checks of step 6.
async fn clone_home(
    options: &GitOptions,
    policy: &Policy,
    root: &Path,
    dest: &Path,
    params: &EnsureHomeParams,
) -> HomeOutcome {
    let clone_options = GitOptions {
        timeout: policy.timeouts().clone,
        // A clone needs the owner's credential helpers and SSH settings.
        isolated: false,
        ..options.clone()
    };
    let dest_text = dest.to_string_lossy();
    let cloned = Git::new(root, clone_options)
        .run(&[
            "-c",
            "protocol.ext.allow=never",
            "clone",
            "--origin",
            &params.remote_name,
            "--no-recurse-submodules",
            "--",
            &params.url,
            &dest_text,
        ])
        .await;
    if let Err(e) = cloned {
        return failed(Failure {
            code: code::CLONE_FAILED,
            text: format!("The clone failed: {}", redact(&e.to_string())),
        });
    }
    match verify(options, policy, dest, params).await {
        Ok(v) => outcome_of(HomeAttemptState::Ready, v),
        Err(f) => failed(f),
    }
}

/// The Git version of this station: the text after `git version ` of
/// `git --version` (design §3.7), read once. `None` when it cannot be read.
pub async fn git_version(options: &GitOptions) -> Option<String> {
    static VERSION: OnceCell<Option<String>> = OnceCell::const_new();
    VERSION
        .get_or_init(|| async {
            let git = read_git(options, &std::env::temp_dir(), READ_TIMEOUT);
            match read(&git, &["--version"], 1024).await {
                Ok((0, out, _)) => out
                    .trim()
                    .strip_prefix("git version ")
                    .map(str::to_owned)
                    .filter(|v| !v.is_empty()),
                _ => None,
            }
        })
        .await
        .clone()
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
