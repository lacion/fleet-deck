//! The owner's recount of an old usage count (B20, DL-448, design
//! `usage-cost.md` §4.2).
//!
//! `SessionManager::submit` holds the per-session lock of `stop_prepare`
//! (a resume, a reconcile and a stop of the session wait), reads the facts
//! outside the writer transaction ([`SessionManager::recount_prepare`]:
//! the execution rule of a resume, the provider record through the trusted
//! locator), and [`decide`] rechecks the database state in the command
//! transaction.

use std::path::Path;
use std::time::{Duration, Instant};

use fleetdeck_proto::command::{CommandResult, SessionCommand};
use fleetdeck_proto::event::{Event, ProviderKind, USAGE_SCHEME, Usage, UsageSource};
use fleetdeck_proto::history::HistoryUnavailable;
use fleetdeck_proto::{ProviderInstanceId, RejectCode, SessionId};
use fleetdeck_providers::claude::cost_state::last_cost_state;

use super::decide::{reject, result};
use super::{SessionManager, shims};
use crate::history::locate::{self, Trust};
use crate::history::{self, Fail, Limits, usage_record};
use crate::store::resumes::Purpose;
use crate::store::transcripts::FirstEffect;
use crate::store::{DecideError, Decision};

/// The time for the open and the scan of the provider record together
/// (design §4.2 step 2.e; under the 5 s receipt wait of the deck).
pub(super) const RECORD_DEADLINE: Duration = Duration::from_secs(3);

/// The refusal codes of a recount (design §4.2).
mod code {
    pub(super) const UNSUPPORTED_PROVIDER: &str = "unsupported_provider";
    pub(super) const COUNTED_SINCE_UPGRADE: &str = "counted_since_upgrade";
    pub(super) const OTHER_HISTORY: &str = "other_history";
    pub(super) const ORIGIN_UNPROVEN: &str = "origin_unproven";
    pub(super) const SESSION_RUNNING: &str = "session_running";
    pub(super) const SESSION_CHANGED: &str = "session_changed";
    pub(super) const RECORD_UNREADABLE: &str = "record_unreadable";
}

/// What the manager reads before the transaction (design §4.2 step 2).
pub(super) struct RecountFacts {
    pub provider: Option<ProviderKind>,
    pub native: Option<String>,
    pub session_seq: u64,
    /// The process of the session is in the manager's live map.
    pub live: bool,
    /// Why an execution of the session or of its conversation may still run
    /// (the resume rule), after the reboot rows were revalidated.
    pub blocked: Option<String>,
    /// The shim ids whose reboot proof was revalidated here.
    pub revalidated: Vec<String>,
    /// The record, or the refusal of step 2.e.
    pub record: Result<(Usage, UsageSource), DecideError>,
}

/// The refusal of a recount that reaches `decide::decide` (a direct call has
/// no session lock and no provider record reader).
pub(super) fn refuse_direct() -> DecideError {
    reject(
        RejectCode::InvalidState,
        "usage_recount_direct: The session manager decides a usage recount.",
    )
}

fn refuse(code: &str, message: &str) -> DecideError {
    let class = if code == self::code::UNSUPPORTED_PROVIDER {
        RejectCode::Unsupported
    } else {
        RejectCode::InvalidState
    };
    reject(class, format!("{code}: {message}"))
}

fn session_running(why: &str) -> DecideError {
    refuse(
        code::SESSION_RUNNING,
        &format!("This session may still run ({why}). It keeps its old count."),
    )
}

fn record_unreadable(word: &str) -> DecideError {
    refuse(
        code::RECORD_UNREADABLE,
        &format!("Fleet Deck could not read the provider record: {word}."),
    )
}

fn unsupported_provider() -> DecideError {
    refuse(
        code::UNSUPPORTED_PROVIDER,
        "Only Claude and Codex sessions have a provider record.",
    )
}

/// The total of a session with no provider record: unknown, and a lower
/// bound.
fn unknown_total() -> Usage {
    Usage {
        scheme: Some(USAGE_SCHEME),
        tokens_partial: true,
        cost_partial: true,
        ..Usage::default()
    }
}

fn supported(kind: Option<ProviderKind>) -> Option<ProviderKind> {
    kind.filter(|k| matches!(k, ProviderKind::Claude | ProviderKind::Codex))
}

/// The scan failure of an I/O error: a passed deadline is `busy`.
fn fail_of(e: &std::io::Error) -> Fail {
    if e.kind() == std::io::ErrorKind::TimedOut {
        Fail::Busy
    } else {
        Fail::Io
    }
}

/// Opens the provider record through the trusted locator and reads its
/// total. `Ok(None)`: the file holds no usage record.
fn scan(
    kind: ProviderKind,
    home: &Path,
    job: &Job,
    trust: &Trust<'_>,
) -> Result<Option<(Usage, UsageSource)>, Fail> {
    let mut opened = locate::locate(home, kind, &job.cwd, &job.native, trust)?;
    match kind {
        ProviderKind::Claude => {
            let found = last_cost_state(&mut opened.file, &job.native, u64::MAX, trust.deadline)
                .map_err(|e| fail_of(&e))?;
            Ok(found.map(|totals| {
                let mut usage = totals.usage();
                usage.tokens_partial = true;
                usage.cost_partial = true;
                (usage, UsageSource::ClaudeTranscript)
            }))
        }
        ProviderKind::Codex => {
            let found = usage_record::codex_usage(
                &opened.file,
                job.model.as_deref(),
                trust.limits,
                trust.deadline,
            )
            .map_err(|e| fail_of(&e))?;
            Ok(found.map(|usage| (usage, UsageSource::CodexRollout)))
        }
        ProviderKind::Unknown => Ok(None),
    }
}

/// What the blocking scan needs.
struct Job {
    cwd: String,
    native: String,
    model: Option<String>,
}

impl SessionManager {
    /// The facts of `session.usage_recount`, read outside the writer
    /// transaction, under the lock of the session. `None` for another
    /// command.
    pub(super) async fn recount_prepare(&self, command: &SessionCommand) -> Option<RecountFacts> {
        let SessionCommand::UsageRecount { session_id } = command else {
            return None;
        };
        Some(self.recount_facts(session_id).await)
    }

    async fn recount_facts(&self, session: &SessionId) -> RecountFacts {
        // a. The session row and its summary.
        let summary = match self.store.summary(session.clone()).await {
            Ok(Some(summary)) => summary,
            Ok(None) => return Self::facts_without_session(None),
            Err(e) => return Self::facts_without_session(Some(format!("store: {e}"))),
        };
        let native = match self.store.history_facts(session.clone()).await {
            Ok(facts) => facts.and_then(|f| f.native),
            Err(e) => return Self::facts_without_session(Some(format!("store: {e}"))),
        };
        // b. The live map.
        let live = self.target.live_pids().iter().any(|(id, _)| id == session);
        let provider = summary.provider;
        let mut facts = RecountFacts {
            provider,
            native: native.clone(),
            session_seq: summary.session_seq,
            live,
            blocked: None,
            revalidated: Vec::new(),
            // d. Another provider has no record.
            record: Err(unsupported_provider()),
        };
        let Some(kind) = supported(provider) else {
            return facts;
        };
        // c. The resume rule, with the reboot rows revalidated.
        match self
            .store
            .execution_exclusion(
                Some(session.clone()),
                kind,
                native.clone(),
                Some(session.clone()),
                Purpose::Resume,
            )
            .await
        {
            Err(e) => facts.blocked = Some(format!("store: {e}")),
            Ok(found) if found.blocked.is_some() => facts.blocked = found.blocked,
            Ok(found) if found.reboot_rows.is_empty() => {}
            Ok(found) => match shims::revalidate(&self.boot_reader(), &found.reboot_rows) {
                Some(why) => facts.blocked = Some(why),
                None => {
                    facts.revalidated = found.reboot_rows.into_iter().map(|r| r.shim_id).collect();
                }
            },
        }
        // e. The provider record.
        facts.record = match (native, summary.provider_instance, summary.cwd) {
            (Some(native), Some(instance), Some(cwd)) => {
                self.recount_record(kind, &instance, cwd, native, summary.model)
                    .await
            }
            _ => Ok((unknown_total(), UsageSource::None)),
        };
        facts
    }

    /// The facts of a session that could not be read: the decision then
    /// refuses (`unknown_session`, or `session_running` for a store error).
    fn facts_without_session(blocked: Option<String>) -> RecountFacts {
        RecountFacts {
            provider: None,
            native: None,
            session_seq: 0,
            live: false,
            blocked,
            revalidated: Vec::new(),
            record: Err(unsupported_provider()),
        }
    }

    /// Step 2.e: opens the provider file with the trusted locator and reads
    /// the total, in a blocking task under [`RECORD_DEADLINE`].
    async fn recount_record(
        &self,
        kind: ProviderKind,
        instance: &ProviderInstanceId,
        cwd: String,
        native: String,
        model: Option<String>,
    ) -> Result<(Usage, UsageSource), DecideError> {
        let unknown = || Ok((unknown_total(), UsageSource::None));
        let policy = self.policy.current();
        let env = history::env_for(Some(&policy), instance);
        let Some(home) = locate::resolve_home(&env, kind).filter(|h| h.is_absolute()) else {
            return unknown();
        };
        let private = self.private_paths();
        let limits: Limits = self.store.history().limits().clone();
        let deadline = Instant::now() + RECORD_DEADLINE;
        let job = Job { cwd, native, model };
        let task = tokio::task::spawn_blocking(move || {
            let trust = Trust {
                private: &private,
                uid: rustix::process::getuid().as_raw(),
                limits: &limits,
                deadline,
            };
            scan(kind, &home, &job, &trust)
        });
        let until = tokio::time::Instant::from_std(deadline);
        let scanned = match tokio::time::timeout_at(until, task).await {
            Ok(Ok(scanned)) => scanned,
            Ok(Err(_)) => Err(Fail::Io),
            Err(_) => Err(Fail::Busy),
        };
        match scanned {
            Ok(Some(record)) => Ok(record),
            Ok(None) | Err(Fail::Unavailable(HistoryUnavailable::NotFound)) => unknown(),
            Err(fail) => Err(record_unreadable(&history::reason_name(fail.reason()))),
        }
    }
}

/// Decides `session.usage_recount` in the command transaction (design §4.2
/// step 3). The first check that applies refuses.
///
/// # Errors
/// A refusal (stored as the receipt), or a database error.
pub(super) fn decide(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    facts: Option<RecountFacts>,
) -> Result<CommandResult, DecideError> {
    let Some(summary) = d.summary(session_id)? else {
        return Err(reject(RejectCode::UnknownSession, "no such session"));
    };
    let Some(facts) = facts else {
        return Err(refuse_direct());
    };
    // a.
    let Some(kind) = supported(summary.provider) else {
        return Err(unsupported_provider());
    };
    // b. Only a total that is entirely an old count is recounted.
    if summary.usage.scheme.is_some() || summary.usage_legacy.is_some() {
        return Err(refuse(
            code::COUNTED_SINCE_UPGRADE,
            "Fleet Deck counts this session with the new rules. Only an old count is recounted.",
        ));
    }
    let native = facts.native.as_deref();
    let db = d.recount_db(session_id, kind, native)?;
    // c. Provenance.
    if db.imported || summary.moved_to.is_some() || summary.move_in.is_some() || db.same_native {
        return Err(refuse(
            code::OTHER_HISTORY,
            "The provider record of this session can hold usage of another session or station.",
        ));
    }
    if db.origin != FirstEffect::Start {
        return Err(refuse(
            code::ORIGIN_UNPROVEN,
            "Fleet Deck has no proof that this session started its conversation on this station (an import, or a start record older than 90 days).",
        ));
    }
    // d. The session may still run.
    if facts.live {
        return Err(session_running("it has a live process"));
    }
    if let Some(why) = &facts.blocked {
        return Err(session_running(why));
    }
    if db.busy {
        return Err(session_running("it has not ended"));
    }
    if let Some(why) = &db.blocked {
        return Err(session_running(why));
    }
    if let Some(native) = native
        && d.native_session_held(native)?.is_some()
    {
        return Err(session_running("a terminal holds its conversation"));
    }
    if db
        .reboot_rows
        .iter()
        .any(|shim| !facts.revalidated.contains(shim))
    {
        return Err(session_running("a previous run is not proven ended"));
    }
    // e.
    if summary.session_seq != facts.session_seq || summary.provider != facts.provider {
        return Err(refuse(
            code::SESSION_CHANGED,
            "The session changed during the recount. Recount it again.",
        ));
    }
    // f.
    let (usage, source) = facts.record?;
    // g.
    let command_id = d.command_id().clone();
    d.append(
        session_id,
        Event::UsageSet {
            usage,
            previous: Box::new(summary.usage),
            source,
            command_id,
        }
        .into(),
    )?;
    Ok(result(session_id, None))
}

#[cfg(test)]
#[path = "usage_recount_tests.rs"]
mod tests;
