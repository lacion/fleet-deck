//! The supervisor of `fleetdeck provision-supervise` (design
//! `board-gaps.md` 4.3): it runs the bootstrap, and when the bootstrap stops
//! in a terminal state before the link, it reports that state to the pinned
//! deck and, when the owner allowed one retry on the board, runs the
//! bootstrap once with the retry authorization.
//!
//! The supervisor holds only its own lock. It never takes `provision.lock`
//! (the bootstrap takes it), never creates an identity, never changes the
//! provisioning file and never weakens a check of the bootstrap. A failure
//! to report is a diagnostic line and never changes the retained error.
//! Stdout stays empty: every line goes through `Effects::progress`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::private_file::try_lock_private_below;
use fleetdeck_link::hello::ReportStatus;
use fleetdeck_link::{ReportReply, ValidReport};

use super::binding::{self, Authorization, Binding};
use super::effects::Effects;
use super::retry::{LONG, SHORT};
use super::run::{self, Exit, Input};
use super::state::{self, Phase};

/// The wait after a busy bootstrap.
const BUSY_WAIT: Duration = SHORT;
/// The wait when nothing can be reported, and after a failed report.
const NOT_READY_WAIT: Duration = LONG;
/// The least wait after a reply of the deck.
const MIN_REPLY_WAIT: Duration = Duration::from_secs(60);
/// The longest wait that a deck reply can ask for: the supervisor checks the
/// condition again at least once an hour.
const MAX_REPLY_WAIT: Duration = Duration::from_secs(3600);
/// The wait after a `retry` reply that is not for the current binding.
const STALE_WAIT: Duration = Duration::from_secs(60);
/// The longest jitter that is added to a wait.
const JITTER_MAX: Duration = Duration::from_secs(60);

/// What the supervisor does after a report step.
enum Next {
    /// Wait, then run the bootstrap again.
    Wait(Duration),
    /// Run the bootstrap once with the authorization at this path.
    Retry(PathBuf),
}

/// The terminal condition that a report is about.
struct Condition {
    input: Input,
    error: String,
    phase: Phase,
    binding: Binding,
}

fn line(effects: &dyn Effects, phase: &str, code: &str) {
    effects.progress(&format!("provision: {phase}: {code}"));
}

/// A wait with a jitter of 0 to 60 s.
fn jittered(effects: &dyn Effects, wait: Duration) -> Duration {
    wait.checked_add(effects.jitter(JITTER_MAX))
        .unwrap_or(Duration::MAX)
}

/// Runs the bootstrap in a loop until it is complete, fails unexpectedly, or
/// another supervisor holds the lock (exit 0 at once).
pub async fn supervise(effects: &dyn Effects, file: &Path) -> Exit {
    if effects.ensure_station_dir().is_err() {
        line(effects, "supervise", "unexpected_error");
        return Exit::Unexpected;
    }
    let dir = effects.station_dir();
    let root = effects.trust_root();
    let _lock = match try_lock_private_below(&root, &binding::supervise_lock_path(&dir)) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Exit::Busy,
        Err(_) => {
            line(effects, "supervise", "unexpected_error");
            return Exit::Unexpected;
        }
    };
    // An authorization that an earlier supervisor wrote and nobody consumed
    // is kept: the bootstrap checks it under `provision.lock`.
    let pending = binding::authorization_path(&dir);
    let mut authorization: Option<PathBuf> = effects
        .read_authorization(&pending)
        .is_ok()
        .then_some(pending);
    loop {
        let path = authorization.take();
        let exit = run::run(effects, file, false, path.as_deref()).await;
        match exit {
            Exit::Unexpected => return Exit::Unexpected,
            Exit::Busy => {
                // The authorization stays for the next run.
                authorization = path;
                effects.sleep(BUSY_WAIT).await;
                continue;
            }
            Exit::Complete | Exit::Terminal => {
                if let Some(path) = &path {
                    remove_authorization(effects, path);
                }
            }
        }
        if exit == Exit::Complete {
            return Exit::Complete;
        }
        match report_step(effects, file, &root, &dir).await {
            Next::Wait(wait) => effects.sleep(wait).await,
            Next::Retry(path) => authorization = Some(path),
        }
    }
}

/// Removes an authorization file that a finished run left (a refused or a
/// replayed one). A failure is a diagnostic line.
fn remove_authorization(effects: &dyn Effects, path: &Path) {
    if effects.read_authorization(path).is_ok() && effects.delete_authorization(path).is_err() {
        line(effects, "retry", "authorization_not_removed");
    }
}

/// The terminal condition now, or the fixed word of what is missing. It
/// stores a new binding when the condition changed.
fn condition(
    effects: &dyn Effects,
    file: &Path,
    root: &Path,
    dir: &Path,
) -> Result<Condition, &'static str> {
    let input = run::read_provision_input(effects, file).ok_or("file_invalid")?;
    let state = state::load(&state::state_path(dir))
        .ok()
        .flatten()
        .ok_or("no_condition")?;
    let error = state.error.clone().ok_or("no_condition")?;
    if !effects.identity_loads() {
        return Err("no_identity");
    }
    let digest = binding::digest(
        &error,
        state.phase,
        &input.config_sha256,
        &input.file.deck_fingerprint,
        state.station_fingerprint.as_deref(),
    );
    let binding = binding::ensure(root, dir, &digest).map_err(|_| "binding_failed")?;
    Ok(Condition {
        input,
        error,
        phase: state.phase,
        binding,
    })
}

/// One report of the retained terminal condition.
async fn report_step(effects: &dyn Effects, file: &Path, root: &Path, dir: &Path) -> Next {
    let wait_long = |effects: &dyn Effects| Next::Wait(jittered(effects, NOT_READY_WAIT));
    let cond = match condition(effects, file, root, dir) {
        Ok(cond) => cond,
        Err(word) => {
            line(effects, "report", word);
            return wait_long(effects);
        }
    };
    let Ok(report) = ValidReport::validate(
        &cond.binding.terminal_id,
        &cond.binding.state_digest,
        &cond.error,
        cond.phase.word(),
        &cond.input.file.name,
    ) else {
        line(effects, "report", "report_invalid");
        return wait_long(effects);
    };
    let sent = effects
        .report(
            &cond.input.host,
            cond.input.port,
            &cond.input.file.deck_fingerprint,
            &report,
        )
        .await;
    match sent {
        Err(failure) => {
            line(effects, "report", failure.0);
            wait_long(effects)
        }
        Ok(reply) if reply.status == ReportStatus::Retry => {
            retry(effects, file, root, dir, &cond, reply)
        }
        Ok(reply) => {
            let wait = Duration::from_secs(reply.retry_after_seconds)
                .clamp(MIN_REPLY_WAIT, MAX_REPLY_WAIT);
            Next::Wait(jittered(effects, wait))
        }
    }
}

/// A `retry` reply: it counts only for the binding that the station holds
/// now. The authorization is written privately and atomically.
fn retry(
    effects: &dyn Effects,
    file: &Path,
    root: &Path,
    dir: &Path,
    cond: &Condition,
    reply: ReportReply,
) -> Next {
    let (Some(retry_id), Some(terminal_id), Some(state_digest)) =
        (reply.retry_id, reply.terminal_id, reply.state_digest)
    else {
        line(effects, "report", "retry_ignored");
        return Next::Wait(STALE_WAIT);
    };
    // The state can change after the report: read the binding again.
    let current = match condition(effects, file, root, dir) {
        Ok(current) => current,
        Err(word) => {
            line(effects, "report", word);
            return Next::Wait(jittered(effects, NOT_READY_WAIT));
        }
    };
    let same = current.binding == cond.binding
        && current.binding.terminal_id == terminal_id
        && current.binding.state_digest == state_digest;
    if !same {
        line(effects, "report", "retry_ignored");
        return Next::Wait(STALE_WAIT);
    }
    let authorization = Authorization {
        retry_id,
        terminal_id,
        state_digest,
    };
    let path = binding::authorization_path(dir);
    if authorization.write(root, &path).is_err() {
        line(effects, "report", "authorization_failed");
        return Next::Wait(jittered(effects, NOT_READY_WAIT));
    }
    Next::Retry(path)
}
