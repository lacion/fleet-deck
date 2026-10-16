//! The checks of an import before the provider launches (B7, design
//! `native-sessions.md` §2.4).
//!
//! - With a pin (an import from the native-session list) the station verifies
//!   the transcript: home, trusted path, supported filesystem, `(dev, ino)`,
//!   the per-record identity rules, and a capture through the same descriptor.
//!   Any failure fails the start.
//! - Without a pin the import is as before: the capture stays best effort.
//! - For every import, a Codex child thread fails the start.
//! - An unpinned Codex import whose child-thread check could not finish
//!   (the history worker is busy, or the lookup or the first record hit
//!   the deadline)
//!   fails the start too (B7); a missing or unreadable transcript goes on.

use std::fs::File;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::HistoryUnavailable;
use fleetdeck_proto::native::TranscriptPin;
use serde_json::Value;

use super::browse::HEAD_BYTES;
use super::browse_preview::scan;
use super::capture;
use super::lines::{Line, Lines, ReadError};
use super::locate::{self, Trust, resolve_home};
use super::trust::{Identity, Mounts, trusted_home, trusted_path};
use super::{Captured, Fail, Limits, env_for, row_of};
use crate::manager::Policy;
use crate::store::Store;
use crate::store::transcripts::Capture;
use crate::workspace::PrivatePaths;

/// The text of a pinned import that did not verify.
pub const CHANGED: &str = "The transcript changed or could not be verified. Browse again.";
/// The text of an import of a Codex child thread.
pub const SUBAGENT: &str = "This is a subagent thread; continue its parent.";
/// The text of an import whose child-thread check could not finish (B7).
pub const BUSY: &str = "The station is busy checking the session. Try again in a moment.";

/// Why an import must not launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A pinned import did not verify.
    Unverified,
    /// A Codex child thread.
    Subagent,
    /// The child-thread check of an unpinned Codex import could not finish.
    Busy,
}

impl Refusal {
    /// The text of the failed start.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Unverified => CHANGED,
            Self::Subagent => SUBAGENT,
            Self::Busy => BUSY,
        }
    }

    /// The reason of the `NotDispatched` answer.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Unverified => "transcript_unverified",
            Self::Subagent => "subagent_thread",
            Self::Busy => "import_check_busy",
        }
    }
}

/// What the blocking part found.
#[derive(Default)]
struct Outcome {
    subagent: bool,
    /// The child-thread check could not finish (B7).
    busy: bool,
    captured: Option<Captured>,
}

/// What the blocking part needs.
struct Job {
    session_id: SessionId,
    kind: ProviderKind,
    cwd: String,
    native: String,
    home: PathBuf,
    private: Arc<PrivatePaths>,
    dir: Option<PathBuf>,
    /// The session already has a transcript row: no second capture.
    has_row: bool,
    pin: Option<TranscriptPin>,
}

/// Runs before the provider launches an import. `Ok` lets the launch go on
/// (the caller then runs `at_import`, which does nothing for a session that
/// a verified import already captured).
///
/// # Errors
/// [`Refusal`]: the start must fail with its message.
pub async fn before_import(
    store: &Store,
    policy: Option<&Policy>,
    private: &Arc<PrivatePaths>,
    session_id: &SessionId,
    native: &str,
    cwd: &str,
    pin: Option<&TranscriptPin>,
) -> Result<(), Refusal> {
    let pinned = pin.is_some();
    // A pinned import fails closed on any doubt; an unpinned one goes on.
    let doubt = if pinned {
        Err(Refusal::Unverified)
    } else {
        Ok(())
    };
    let facts = match store.history_facts(session_id.clone()).await {
        Ok(Some(facts)) => facts,
        Ok(None) => return doubt,
        Err(e) => {
            tracing::warn!(error = %e, session = %session_id, "import check: session facts");
            return doubt;
        }
    };
    // Only a pin or a Codex transcript needs work.
    if !pinned && facts.provider != ProviderKind::Codex {
        return Ok(());
    }
    let env = env_for(policy, &facts.provider_instance);
    let Some(home) = resolve_home(&env, facts.provider).filter(|h| h.is_absolute()) else {
        tracing::info!(session = %session_id, "import check: no absolute provider home");
        return doubt;
    };
    let has_row = match store.transcript(session_id.clone()).await {
        Ok(row) => row.is_some(),
        Err(e) => {
            tracing::warn!(error = %e, session = %session_id, "import check: row");
            return doubt;
        }
    };
    let history = store.history();
    let hook = history
        .shared
        .seams
        .after_copy
        .lock()
        .ok()
        .and_then(|h| h.clone());
    let uid = history.uid();
    let limits = history.limits().clone();
    let job = Job {
        session_id: session_id.clone(),
        kind: facts.provider,
        cwd: cwd.to_owned(),
        native: native.to_owned(),
        home,
        private: Arc::clone(private),
        dir: history.dir().cloned(),
        has_row,
        pin: pin.cloned(),
    };
    let deadline = Instant::now() + limits.deadline;
    let worked = history
        .run(deadline, move |deadline| {
            if job.pin.is_some() {
                pinned_blocking(&job, &limits, uid, deadline, hook)
            } else {
                Ok(unpinned_blocking(&job, &limits, uid, deadline))
            }
        })
        .await;
    let outcome = match worked {
        Ok(outcome) => outcome,
        // An unpinned Codex import fails closed when the check cannot run (B7).
        Err(Fail::Busy) if !pinned => {
            tracing::info!(session = %session_id, "import check: busy");
            return Err(Refusal::Busy);
        }
        Err(fail) => {
            tracing::info!(session = %session_id, ?fail, "import check: not verified");
            return doubt;
        }
    };
    if outcome.subagent {
        return Err(Refusal::Subagent);
    }
    if outcome.busy {
        return Err(Refusal::Busy);
    }
    if let Some(captured) = &outcome.captured {
        let row = row_of(
            session_id,
            Capture::Import,
            Some(captured),
            None,
            store.now_ms(),
        );
        if let Err(e) = store.put_transcript(row).await {
            tracing::warn!(error = %e, session = %session_id, "import check: row not stored");
            return Err(Refusal::Unverified);
        }
    }
    Ok(())
}

/// The first record of a Codex transcript names a parent thread. `None`
/// when the read hit the deadline: the answer is not known (B7).
fn names_parent(file: &File, size: u64, limits: &Limits, deadline: Instant) -> Option<bool> {
    let mut lines = Lines::new(file, 0, size, limits.line_max).with_deadline(deadline);
    let first = match lines.next_line() {
        Ok(Some((_, Line::Record(bytes)))) => bytes,
        Err(ReadError::Deadline) => return None,
        _ => return Some(false),
    };
    let Ok(value) = serde_json::from_slice::<Value>(first) else {
        return Some(false);
    };
    Some(
        value.get("type").and_then(Value::as_str) == Some("session_meta")
            && value
                .get("payload")
                .and_then(|p| p.get("parent_thread_id"))
                .and_then(Value::as_str)
                .is_some_and(|parent| !parent.is_empty()),
    )
}

/// An unpinned import: only the child-thread rule, and only when the
/// transcript is there and readable.
fn unpinned_blocking(job: &Job, limits: &Limits, uid: u32, deadline: Instant) -> Outcome {
    if job.kind != ProviderKind::Codex {
        return Outcome::default();
    }
    let trust = Trust {
        private: &job.private,
        uid,
        limits,
        deadline,
    };
    let opened = match locate::locate_any_size(&job.home, job.kind, &job.cwd, &job.native, &trust) {
        Ok(opened) => opened,
        // The lookup hit the deadline or the entry cap: the check could not
        // finish (B7).
        Err(Fail::Busy | Fail::Unavailable(HistoryUnavailable::Busy)) => {
            return Outcome {
                busy: true,
                ..Outcome::default()
            };
        }
        // A missing or unreadable transcript: the import goes on.
        Err(_) => return Outcome::default(),
    };
    match names_parent(&opened.file, opened.size, limits, deadline) {
        Some(subagent) => Outcome {
            subagent,
            ..Outcome::default()
        },
        // The check could not finish: the import does not launch (B7).
        None => Outcome {
            busy: true,
            ..Outcome::default()
        },
    }
}

fn changed() -> Fail {
    Fail::Unavailable(fleetdeck_proto::history::HistoryUnavailable::NotTrusted)
}

fn read_fail(e: ReadError) -> Fail {
    match e {
        ReadError::Deadline => Fail::Busy,
        ReadError::Io(_) => Fail::Io,
    }
}

/// A pinned import (design §2.4): every check, then the capture through the
/// verified descriptor.
fn pinned_blocking(
    job: &Job,
    limits: &Limits,
    uid: u32,
    deadline: Instant,
    hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
) -> Result<Outcome, Fail> {
    let pin = job.pin.as_ref().ok_or_else(changed)?;
    // Any failure of the home fails a pinned import, also an unverifiable
    // one (B7 PRE-PR r1).
    let mounts = Mounts::load();
    let home = trusted_home(&job.home, uid, &mounts).map_err(|_| changed())?;
    let trust = Trust {
        private: &job.private,
        uid,
        limits,
        deadline,
    };
    let opened = match locate::locate(&home, job.kind, &job.cwd, &job.native, &trust) {
        Ok(opened) => opened,
        // A too-large transcript fails as before, but a child thread of it
        // gets the subagent text (B7 PRE-PR r1).
        Err(Fail::Unavailable(HistoryUnavailable::TooLarge)) if job.kind == ProviderKind::Codex => {
            let big = locate::locate_any_size(&home, job.kind, &job.cwd, &job.native, &trust)?;
            if names_parent(&big.file, big.size, limits, deadline) == Some(true) {
                return Ok(Outcome {
                    subagent: true,
                    ..Outcome::default()
                });
            }
            return Err(Fail::Unavailable(HistoryUnavailable::TooLarge));
        }
        Err(e) => return Err(e),
    };
    // The child-thread rule comes first, as for an unpinned import.
    if job.kind == ProviderKind::Codex
        && names_parent(&opened.file, opened.size, limits, deadline) == Some(true)
    {
        return Ok(Outcome {
            subagent: true,
            ..Outcome::default()
        });
    }
    let meta = opened.file.metadata().map_err(|_| Fail::Io)?;
    if home.to_str() != Some(pin.home.as_str())
        || opened.home != home
        || meta.dev() != pin.dev
        || meta.ino() != pin.ino
    {
        return Err(changed());
    }
    // An unverifiable path fails too: a pin is sent for a verified entry only.
    let trusted = |path: &Path| trusted_path(path, uid, &mounts).map_err(|_| changed());
    trusted(&home.join(&opened.path))?;
    trusted(Path::new(&job.cwd))?;
    // The per-record identity rules on the head window.
    let dir_name = (job.kind == ProviderKind::Claude)
        .then(|| opened.path.iter().nth(1).and_then(|n| n.to_str()))
        .flatten();
    let mut identity = Identity::new(job.kind, &job.native, dir_name);
    let mut bad = false;
    scan(
        &opened.file,
        0,
        opened.size.min(HEAD_BYTES),
        limits,
        deadline,
        |offset, line| {
            match line {
                Line::TooLong => bad = offset == 0 && job.kind == ProviderKind::Codex,
                Line::Record(bytes) => bad = identity.record(bytes).is_err(),
            }
            !bad
        },
    )
    .map_err(read_fail)?;
    if bad {
        return Err(changed());
    }
    let found = identity.finish().map_err(|_| changed())?;
    if found.cwd != job.cwd {
        return Err(changed());
    }
    if job.has_row {
        return Ok(Outcome::default());
    }
    let dir = job.dir.as_ref().ok_or(Fail::NoDir)?;
    let snapshot = capture::capture_file(
        &opened.file,
        opened.size,
        dir,
        job.session_id.as_str(),
        deadline,
        hook.as_deref(),
    )?;
    Ok(Outcome {
        subagent: false,
        busy: false,
        captured: Some(Captured {
            home: opened.home,
            path: opened.path,
            snapshot,
        }),
    })
}
