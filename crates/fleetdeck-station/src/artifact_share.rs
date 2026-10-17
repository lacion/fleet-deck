//! The artifact share of a project on this station (design §15.5, §15.6,
//! FG-12, O38, O39): the authorization, the session folder, the variables
//! `FLEETDECK_ARTIFACTS` and `FLEETDECK_ARTIFACTS_URL`, the 5-minute
//! refresh, and the opening of the folder for the file service.
//!
//! Fail closed: only `ok` and `not_a_station` authorize. Every use asks for
//! its own authorization; a stored one is for display and audit only.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::fleet_git_digest::home_folder_name;
use fleetdeck_proto::fleet_git::{
    ArtifactAuthorization, ArtifactAuthorizeParams, ExposureState, FleetGitStart, SharePathState,
    ShareVerdict, ShareVerdictAnswer, TaildriveReport, TaildriveShare, code,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{Event, SessionId};
use rustix::fs::{Access, Mode, OFlags};

use crate::fleet_git_runtime::FleetGitRuntime;
use crate::store::artifacts_state::ArtifactsRecord;
use crate::store::{NewEvent, Store};
use crate::taildrive;

/// A mount path that does not exist.
pub const MOUNT_MISSING: &str = "mount_missing";
/// A mount path that is not a folder.
pub const MOUNT_NOT_FOLDER: &str = "mount_not_folder";
/// A mount path that is not writable.
pub const MOUNT_NOT_WRITABLE: &str = "mount_not_writable";
/// The tailnet name is not known: no address can be made.
pub const TAILNET_UNKNOWN: &str = "tailnet_unknown";

/// The variable with the folder (design §15.5 item 3).
pub const ENV_PATH: &str = "FLEETDECK_ARTIFACTS";
/// The variable with the address (design §15.5 item 4).
pub const ENV_URL: &str = "FLEETDECK_ARTIFACTS_URL";

/// An authorization is current for 10 minutes.
pub const AUTH_TTL_MS: i64 = 600_000;
/// The deadline of `fleet_git.artifact_authorize` (design §15.5).
pub const AUTHORIZE_DEADLINE: Duration = Duration::from_secs(10);
/// The refresh of the authorization of a live session.
pub const SESSION_REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);

/// The text of `artifacts.revoked` (design §15.5).
pub const REVOKED_TEXT: &str = "The artifact share is not verified now. Fleet Deck stops showing the artifact folder. The running agent keeps FLEETDECK_ARTIFACTS until the session ends.";
/// The text of a refused open of the artifact folder (design §15.6).
pub const UNAVAILABLE_TEXT: &str = "The artifact folder cannot be opened safely now.";

const WITHOUT_PATH: &str = "the session started without FLEETDECK_ARTIFACTS.";

/// The text of `artifacts.unavailable` for `code` (design §15.7). `path` is
/// the mount path of the three mount codes.
#[must_use]
pub fn unavailable_text(code_name: &str, path: &str) -> String {
    match code_name {
        code::SHARE_EXPOSED => "The artifact share exposes protected folders; the session started without FLEETDECK_ARTIFACTS.".to_owned(),
        MOUNT_MISSING => format!("The artifact mount {path} does not exist; {WITHOUT_PATH}"),
        MOUNT_NOT_FOLDER => format!("The artifact mount {path} is not a folder; {WITHOUT_PATH}"),
        MOUNT_NOT_WRITABLE => {
            format!("The artifact mount {path} is not writable; {WITHOUT_PATH}")
        }
        TAILNET_UNKNOWN => {
            "The tailnet name is not known; the session started without FLEETDECK_ARTIFACTS_URL."
                .to_owned()
        }
        _ => format!("Fleet Deck cannot check the artifact share now; {WITHOUT_PATH}"),
    }
}

/// Why an authorization is not current: the code of `artifacts.unavailable`
/// and its text, and the authorization when there was one.
#[derive(Debug, Clone, PartialEq)]
pub struct Denied {
    pub code: &'static str,
    pub text: String,
    pub auth: Option<Box<ArtifactAuthorization>>,
}

impl Denied {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            text: unavailable_text(code, ""),
            auth: None,
        }
    }

    fn mount(code: &'static str, path: &Path) -> Self {
        Self {
            code,
            text: unavailable_text(code, &path.to_string_lossy()),
            auth: None,
        }
    }
}

/// True when a start of `fleet_git` has an artifact share to set up.
#[must_use]
pub fn wanted(fleet_git: Option<&FleetGitStart>) -> bool {
    fleet_git.is_some_and(|fg| fg.settings.artifact_share.is_some())
}

/// `<machine>/<share>` as its two parts.
#[must_use]
pub fn split_share(share: &str) -> Option<(&str, &str)> {
    share
        .split_once('/')
        .filter(|(machine, name)| !machine.is_empty() && !name.is_empty())
}

/// Percent-encodes everything except the RFC 3986 unreserved characters
/// (decision C11).
#[must_use]
pub fn encode_segment(segment: &str) -> String {
    use std::fmt::Write;
    segment.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// `FLEETDECK_ARTIFACTS_URL` (design §15.5 item 4).
#[must_use]
pub fn url_of(tailnet: &str, machine: &str, share: &str, folder: &str, callsign: &str) -> String {
    let segments = [tailnet, machine, share, folder, callsign].map(encode_segment);
    format!("http://100.100.100.100:8080/{}/", segments.join("/"))
}

// ------------------------------------------------------------ authorization

/// The request of one authorization.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    pub project_id: &'a str,
    pub session_id: &'a SessionId,
    pub share: &'a str,
}

/// `Some(code)` when `auth` does not authorize now (design §15.5, fail
/// closed).
#[must_use]
pub fn refusal_of(auth: &ArtifactAuthorization, now_ms: i64) -> Option<&'static str> {
    match auth.verdict {
        ShareVerdict::Exposed => Some(code::SHARE_EXPOSED),
        ShareVerdict::Ok | ShareVerdict::NotAStation if auth.expires_at > now_ms => None,
        _ => Some(code::SHARE_UNVERIFIED),
    }
}

/// The mount path of `<machine>/<share>` when `[taildrive_mounts]` names one
/// of the candidates of an ambiguous share (design §15.3, decision C9).
fn resolved_mount<'a>(
    mounts: &'a BTreeMap<String, PathBuf>,
    machine: &str,
    share: &TaildriveShare,
) -> Option<&'a PathBuf> {
    mounts
        .get(&format!("{machine}/{}", share.name))
        .filter(|path| {
            share
                .candidates
                .iter()
                .any(|c| Path::new(c) == path.as_path())
        })
}

/// `[taildrive_mounts]` of the station (empty without a source).
fn mounts_of(runtime: &FleetGitRuntime) -> BTreeMap<String, PathBuf> {
    runtime
        .taildrive_source()
        .map(|s| s.mounts.clone())
        .unwrap_or_default()
}

/// The authorization of a share on this station, from its own report
/// (`exporting_station = None`, decision Q4).
fn local_auth(
    report: &TaildriveReport,
    machine: &str,
    name: &str,
    mounts: &BTreeMap<String, PathBuf>,
    ask: &Ask<'_>,
    now: i64,
) -> Result<ArtifactAuthorization, Denied> {
    let share = report.shares.iter().find(|s| s.name == name);
    let (verdict, exposed, text) = match share {
        None => (ShareVerdict::Unknown, Vec::new(), None),
        Some(s) if s.exposure_state == ExposureState::Exposed => {
            (ShareVerdict::Exposed, s.exposed.clone(), None)
        }
        Some(s) if s.exposure_state != ExposureState::Clear => {
            (ShareVerdict::Unknown, Vec::new(), None)
        }
        Some(s) => match s.path_state {
            SharePathState::Ok => (ShareVerdict::Ok, Vec::new(), None),
            SharePathState::Ambiguous if resolved_mount(mounts, machine, s).is_some() => {
                (ShareVerdict::Ok, Vec::new(), None)
            }
            SharePathState::Ambiguous => (
                ShareVerdict::Unknown,
                Vec::new(),
                Some(format!(
                    "Fleet Deck cannot tell the exact path of the share {name}. Add it to [taildrive_mounts] in station.toml."
                )),
            ),
            _ => (ShareVerdict::Unknown, Vec::new(), None),
        },
    };
    let auth = ArtifactAuthorization {
        share: ask.share.to_owned(),
        exporting_station: None,
        inventory_revision: Some(report.inventory_revision),
        verdict,
        exposed,
        checked_at: Some(report.checked_at),
        issued_at: now,
        expires_at: now.saturating_add(AUTH_TTL_MS),
    };
    match refusal_of(&auth, now) {
        None => Ok(auth),
        Some(code) => {
            let mut denied = Denied::new(code);
            if let Some(text) = text {
                denied.text = text;
            }
            denied.auth = Some(Box::new(auth));
            Err(denied)
        }
    }
}

/// The authorization of a share on another machine: the deck answers
/// `fleet_git.artifact_authorize` (design §15.5). An error, no answer in
/// time, another share or an expired answer is `share_unverified`.
async fn remote_auth(
    runtime: &FleetGitRuntime,
    ask: &Ask<'_>,
    now: i64,
) -> Result<ArtifactAuthorization, Denied> {
    let Some(control) = runtime.deck() else {
        return Err(Denied::new(code::SHARE_UNVERIFIED));
    };
    let params = ArtifactAuthorizeParams {
        project_id: ask.project_id.to_owned(),
        session_id: ask.session_id.clone(),
        share: ask.share.to_owned(),
    };
    let Ok(params) = serde_json::to_value(params) else {
        return Err(Denied::new(code::SHARE_UNVERIFIED));
    };
    let call = control.request("fleet_git.artifact_authorize", None, params);
    let auth = match tokio::time::timeout(AUTHORIZE_DEADLINE, call).await {
        Ok(Ok(Outcome::Ok(value))) => serde_json::from_value::<ArtifactAuthorization>(value).ok(),
        Ok(Ok(Outcome::Err(e))) => {
            tracing::debug!(code = %e.code, "the deck refused the artifact authorization");
            None
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "the artifact authorization request failed");
            None
        }
        Err(_) => None,
    };
    let Some(auth) = auth.filter(|a| a.share == ask.share) else {
        return Err(Denied::new(code::SHARE_UNVERIFIED));
    };
    match refusal_of(&auth, now) {
        None => Ok(auth),
        Some(code) => Err(Denied {
            auth: Some(Box::new(auth)),
            ..Denied::new(code)
        }),
    }
}

/// A fresh authorization for `ask` (design §15.5 item 0). Local: from the
/// own report, made again when it is older than 60 s. Remote: through the
/// deck. `Ok` only when it is current.
///
/// # Errors
/// [`Denied`] with the code of `artifacts.unavailable`.
pub async fn authorize(
    runtime: &FleetGitRuntime,
    store: &Store,
    ask: &Ask<'_>,
) -> Result<ArtifactAuthorization, Denied> {
    let mounts = mounts_of(runtime);
    let Some((machine, name)) = split_share(ask.share) else {
        return Err(Denied::new(code::SHARE_UNVERIFIED));
    };
    let report = taildrive::fresh(runtime, store).await;
    let now = store.now_ms();
    match report {
        Some(report)
            if report
                .machine
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(machine)) =>
        {
            let this = report.machine.clone().unwrap_or_default();
            local_auth(&report, &this, name, &mounts, ask, now)
        }
        _ => remote_auth(runtime, ask, now).await,
    }
}

/// The answer of `fleet_git.share_verdict` (decision S5, C9): the share of
/// `report`, or `missing` when it is not listed, or `unknown` without a
/// report.
#[must_use]
pub fn verdict_answer(
    report: Option<&TaildriveReport>,
    name: &str,
    mounts: &BTreeMap<String, PathBuf>,
    now_ms: i64,
) -> ShareVerdictAnswer {
    let Some(report) = report else {
        return ShareVerdictAnswer {
            inventory_revision: 0,
            checked_at: now_ms,
            path_state: SharePathState::Unknown,
            exposure_state: ExposureState::Unknown,
            exposed: Vec::new(),
        };
    };
    let mut answer = ShareVerdictAnswer {
        inventory_revision: report.inventory_revision,
        checked_at: report.checked_at,
        path_state: SharePathState::Missing,
        exposure_state: ExposureState::Unknown,
        exposed: Vec::new(),
    };
    if let Some(share) = report.shares.iter().find(|s| s.name == name) {
        let machine = report.machine.as_deref().unwrap_or_default();
        answer.path_state = match share.path_state {
            SharePathState::Ambiguous if resolved_mount(mounts, machine, share).is_some() => {
                SharePathState::Ok
            }
            state => state,
        };
        answer.exposure_state = share.exposure_state;
        answer.exposed = share.exposed.clone();
    }
    answer
}

// ------------------------------------------------------------- the folder

/// The local path of the share (design §15.5 item 1): the share path on this
/// station, else the mount path.
fn local_path(
    report: Option<&TaildriveReport>,
    mounts: &BTreeMap<String, PathBuf>,
    share_key: &str,
    name: &str,
) -> Option<PathBuf> {
    if let Some(report) = report
        && let Some((machine, _)) = split_share(share_key)
        && report
            .machine
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(machine))
    {
        let share = report.shares.iter().find(|s| s.name == name)?;
        return match share.path_state {
            SharePathState::Ok => Some(PathBuf::from(&share.path)),
            SharePathState::Ambiguous => {
                resolved_mount(mounts, report.machine.as_deref()?, share).cloned()
            }
            _ => None,
        };
    }
    mounts.get(share_key).cloned()
}

/// The checks of a local path (design §15.5 item 3): it exists, is a folder
/// (links followed) and is writable.
fn check_local(path: &Path) -> Result<(), Denied> {
    let meta = std::fs::metadata(path).map_err(|_| Denied::mount(MOUNT_MISSING, path))?;
    if !meta.is_dir() {
        return Err(Denied::mount(MOUNT_NOT_FOLDER, path));
    }
    rustix::fs::access(path, Access::WRITE_OK).map_err(|_| Denied::mount(MOUNT_NOT_WRITABLE, path))
}

fn segment_ok(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// Makes `<local>/<folder>/<callsign>/` with mode 0700 for each missing
/// level, without following a link. The answer is the folder with the
/// trailing `/`, its device and its inode.
fn make_folder(local: &Path, folder: &str, callsign: &str) -> Result<(String, u64, u64), String> {
    if !segment_ok(folder) || !segment_ok(callsign) {
        return Err("the folder name is not valid".to_owned());
    }
    let canon = std::fs::canonicalize(local).map_err(|e| e.to_string())?;
    let mut dir = fleetdeck_core::launch_dir::open_root(&canon).map_err(|e| e.to_string())?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for name in [folder, callsign] {
        match rustix::fs::mkdirat(&dir, name, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.to_string()),
        }
        dir = rustix::fs::openat(&dir, name, flags, Mode::empty()).map_err(|e| e.to_string())?;
    }
    let stat = rustix::fs::fstat(&dir).map_err(|e| e.to_string())?;
    #[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
    let ids = (stat.st_dev as u64, stat.st_ino as u64);
    let text = format!(
        "{}/{folder}/{callsign}/",
        canon.to_string_lossy().trim_end_matches('/')
    );
    Ok((text, ids.0, ids.1))
}

/// What a start of a session needs of the artifact share.
#[derive(Debug, Clone, Copy)]
pub struct StepInput<'a> {
    pub session_id: &'a SessionId,
    pub project_id: &'a str,
    pub project_name: &'a str,
    /// `<machine>/<share>`.
    pub share: &'a str,
    pub callsign: &'a str,
}

/// The variable, the record and the event of a successful step.
struct Placed {
    var: (&'static str, String),
    record: ArtifactsRecord,
    event: Event,
}

async fn plan(
    runtime: &FleetGitRuntime,
    store: &Store,
    input: &StepInput<'_>,
) -> Result<Placed, Denied> {
    let mounts = &mounts_of(runtime);
    let ask = Ask {
        project_id: input.project_id,
        session_id: input.session_id,
        share: input.share,
    };
    let auth = authorize(runtime, store, &ask).await?;
    let (machine, name) =
        split_share(input.share).ok_or_else(|| Denied::new(code::SHARE_UNVERIFIED))?;
    let folder = home_folder_name(input.project_name);
    let report = runtime.taildrive_report();
    let with_auth = |mut d: Denied| {
        d.auth = Some(Box::new(auth.clone()));
        d
    };
    let Some(local) = local_path(report.as_ref(), mounts, input.share, name) else {
        let tailnet = runtime
            .tailnet()
            .ok_or_else(|| with_auth(Denied::new(TAILNET_UNKNOWN)))?;
        let url = url_of(&tailnet, machine, name, &folder, input.callsign);
        return Ok(Placed {
            var: (ENV_URL, url.clone()),
            event: Event::ArtifactsReady {
                path: None,
                url: Some(url.clone()),
            },
            record: ArtifactsRecord {
                path: None,
                dev: None,
                ino: None,
                url: Some(url),
                auth,
            },
        });
    };
    // A mount path that is, contains or is inside a protected path (item 2).
    let Some(source) = runtime.taildrive_source() else {
        return Err(with_auth(Denied::new(code::SHARE_UNVERIFIED)));
    };
    let kinds = taildrive::path_exposure(&source, store, &local)
        .await
        .map_err(|_| with_auth(Denied::new(code::SHARE_UNVERIFIED)))?;
    if !kinds.is_empty() {
        return Err(with_auth(Denied::new(code::SHARE_EXPOSED)));
    }
    check_local(&local).map_err(with_auth)?;
    let made = {
        let (local, folder, callsign) = (local.clone(), folder, input.callsign.to_owned());
        tokio::task::spawn_blocking(move || make_folder(&local, &folder, &callsign)).await
    };
    let (path, dev, ino) = match made {
        Ok(Ok(made)) => made,
        Ok(Err(why)) => {
            tracing::warn!(error = %why, "the artifact folder was not made");
            return Err(with_auth(Denied::mount(MOUNT_NOT_WRITABLE, &local)));
        }
        Err(e) => {
            tracing::warn!(error = %e, "the artifact folder task failed");
            return Err(with_auth(Denied::mount(MOUNT_NOT_WRITABLE, &local)));
        }
    };
    Ok(Placed {
        var: (ENV_PATH, path.clone()),
        event: Event::ArtifactsReady {
            path: Some(path.clone()),
            url: None,
        },
        record: ArtifactsRecord {
            path: Some(path),
            dev: Some(dev),
            ino: Some(ino),
            url: None,
            auth,
        },
    })
}

async fn append(store: &Store, session: &SessionId, event: Event) {
    if let Err(e) = store
        .append(session.clone(), vec![NewEvent::from(event)])
        .await
    {
        tracing::warn!(error = %e, "an artifact event was not stored");
    }
}

/// The artifact step of a start or a resume (design §15.5): the
/// authorization first, then the folder or the address. The answer is the
/// variable for the provider and the setup command (empty when the share is
/// unavailable). The step appends `artifacts.ready` or
/// `artifacts.unavailable` and never fails the start.
pub async fn start_step(
    runtime: &FleetGitRuntime,
    store: &Store,
    input: &StepInput<'_>,
) -> BTreeMap<String, String> {
    match plan(runtime, store, input).await {
        Ok(placed) => {
            if let Err(e) = store
                .artifacts_record(input.session_id.clone(), placed.record)
                .await
            {
                tracing::warn!(error = %e, "the artifact folder was not recorded");
            }
            append(store, input.session_id, placed.event).await;
            BTreeMap::from([(placed.var.0.to_owned(), placed.var.1)])
        }
        Err(denied) => {
            if let Some(auth) = denied.auth
                && let Err(e) = store
                    .artifacts_set_auth(input.session_id.clone(), *auth)
                    .await
            {
                tracing::warn!(error = %e, "the artifact authorization was not recorded");
            }
            let event = Event::ArtifactsUnavailable {
                code: denied.code.to_owned(),
                text: denied.text,
            };
            append(store, input.session_id, event).await;
            BTreeMap::new()
        }
    }
}

/// Appends `artifacts.unavailable` for a step that could not even start
/// (no runtime, no project view).
pub async fn report_unavailable(store: &Store, session: &SessionId, code: &'static str) {
    let denied = Denied::new(code);
    let event = Event::ArtifactsUnavailable {
        code: denied.code.to_owned(),
        text: denied.text,
    };
    append(store, session, event).await;
}

// ----------------------------------------------------------------- access

/// Why the artifact folder of a session is not opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessError {
    /// The session has no artifact folder on this station.
    NoFolder,
    /// A check failed (design §15.6): the answer is `artifacts_unavailable`.
    Unavailable,
}

/// The current `artifact_share` of the project of `project_id` and the
/// project name (decision C7).
async fn project_share(store: &Store, project_id: &str) -> Option<(String, Option<String>)> {
    let row = store
        .git_project(project_id.to_owned())
        .await
        .ok()
        .flatten()?;
    let share = row.view.settings.artifact_share.clone();
    Some((row.view.project_name, share))
}

/// Opens the artifact folder of `session` for the file service (design
/// §15.6, decision C4): a fresh authorization for the project's current
/// share, which must be the share of the stored authorization; the folder
/// opened without following a link; the recorded device and inode; and the
/// local path checked again against the protected paths. The answer is the
/// open folder and its path.
///
/// # Errors
/// [`AccessError`].
pub async fn open_folder(
    runtime: &FleetGitRuntime,
    store: &Store,
    session: &SessionId,
) -> Result<(OwnedFd, PathBuf), AccessError> {
    let row = store
        .artifacts_row(session.clone())
        .await
        .map_err(|_| AccessError::Unavailable)?;
    let Some(row) = row else {
        return Err(AccessError::NoFolder);
    };
    let Some(path) = row.path.clone() else {
        return Err(AccessError::NoFolder);
    };
    let (Some(auth), Some(dev), Some(ino)) = (&row.auth, row.dev, row.ino) else {
        return Err(AccessError::Unavailable);
    };
    let current = project_share(store, &row.project_id)
        .await
        .and_then(|(_, share)| share);
    if current.as_deref() != Some(auth.share.as_str()) {
        return Err(AccessError::Unavailable);
    }
    let ask = Ask {
        project_id: &row.project_id,
        session_id: session,
        share: &auth.share,
    };
    authorize(runtime, store, &ask)
        .await
        .map_err(|_| AccessError::Unavailable)?;
    let folder = PathBuf::from(path.trim_end_matches('/'));
    let fd =
        fleetdeck_core::launch_dir::open_root(&folder).map_err(|_| AccessError::Unavailable)?;
    let stat = rustix::fs::fstat(&fd).map_err(|_| AccessError::Unavailable)?;
    #[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
    let same = (stat.st_dev as u64, stat.st_ino as u64) == (dev, ino);
    if !same {
        return Err(AccessError::Unavailable);
    }
    let local = folder
        .parent()
        .and_then(Path::parent)
        .ok_or(AccessError::Unavailable)?;
    let source = runtime.taildrive_source().ok_or(AccessError::Unavailable)?;
    let kinds = taildrive::path_exposure(&source, store, local)
        .await
        .map_err(|_| AccessError::Unavailable)?;
    if !kinds.is_empty() {
        return Err(AccessError::Unavailable);
    }
    Ok((fd, folder))
}

// ---------------------------------------------------------------- refresh

/// One pass of the 5-minute refresh (design §15.5, decision C8): each live
/// session with artifact variables gets a new authorization. A session whose
/// share is no longer authorized and still shows its folder gets
/// `artifacts.revoked`; a session that shows no folder and is authorized
/// again gets `artifacts.ready`.
pub async fn refresh_sessions(runtime: &FleetGitRuntime, store: &Store) {
    let rows = match store.artifacts_live().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "the live artifact sessions were not read");
            return;
        }
    };
    for row in rows {
        let Some(stored) = row.auth.as_ref().map(|a| a.share.clone()) else {
            continue;
        };
        let current = project_share(store, &row.project_id)
            .await
            .and_then(|(_, s)| s);
        let ask = Ask {
            project_id: &row.project_id,
            session_id: &row.session_id,
            share: &stored,
        };
        let verdict = if current.as_deref() == Some(stored.as_str()) {
            authorize(runtime, store, &ask).await
        } else {
            Err(Denied::new(code::SHARE_UNVERIFIED))
        };
        let shown = match store.summary(row.session_id.clone()).await {
            Ok(Some(summary)) => summary.artifacts_dir.is_some(),
            _ => continue,
        };
        match verdict {
            Err(denied) if shown => {
                let event = Event::ArtifactsRevoked {
                    code: denied.code.to_owned(),
                    text: REVOKED_TEXT.to_owned(),
                };
                append(store, &row.session_id, event).await;
            }
            Ok(auth) => {
                if let Err(e) = store.artifacts_set_auth(row.session_id.clone(), auth).await {
                    tracing::warn!(error = %e, "the artifact authorization was not recorded");
                }
                if !shown {
                    let event = Event::ArtifactsReady {
                        path: row.path.clone(),
                        url: row.url.clone(),
                    };
                    append(store, &row.session_id, event).await;
                }
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "artifact_share_tests.rs"]
pub(crate) mod tests;
