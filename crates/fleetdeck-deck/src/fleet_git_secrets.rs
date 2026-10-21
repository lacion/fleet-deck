//! The sync of the secret files of a project (Fleet Sync FS-3, design
//! `docs/v2/design/fleet-sync.md` §3.4 with the revisions R3 to R6 of the
//! brief): newest copy wins, through the deck relay.
//!
//! - [`decide`] is pure: it picks the winning digest, the source station and
//!   the targets of one file.
//! - [`sync_project`] runs one project: a scan of every ready home, then for
//!   each file the read from the source and the writes to the targets. One
//!   run of a project at a time ([`lock_project`]).
//! - [`spawn`] starts the periodic task.
//!
//! The deck holds a content only in memory, as the base64 text of one file,
//! for the time of its transfers. It never stores or logs a content, a digest
//! or a salt. A log line names the project, the path, the station and an
//! outcome code. The error of a failed call is logged with its code only.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::GitHomeState;
use fleetdeck_proto::fleet_git_secrets::{
    SecretFileKey, SecretFileReport, SecretFileState, SecretOutcome, SecretPresence,
    SecretReadAnswer, SecretReadParams, SecretScanAnswer, SecretScanParams, SecretStationReport,
    SecretStationState, SecretSyncReport, SecretWriteAnswer, SecretWriteParams,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use tokio::sync::watch;

use crate::link::DeckLink;
use crate::store::secret_files::SecretFileRow;
use crate::store::{AuditResult, DeckError, DeckStore, LinkFeature, NewAudit};

/// The deadline of one call to a station.
const STEP: Duration = Duration::from_secs(30);
/// The first periodic run, after the start.
const FIRST_RUN: Duration = Duration::from_secs(60);
/// The pause between two periodic runs.
const EVERY: Duration = Duration::from_secs(15 * 60);
/// The characters of a digest that an audit row shows.
const DIGEST_PREFIX: usize = 12;

const METHOD_SCAN: &str = "project.secret_scan";
const METHOD_READ: &str = "project.secret_read";
const METHOD_WRITE: &str = "project.secret_write";

/// Why a run did not start.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("unknown project")]
    UnknownProject,
    /// The project has no `remote_dest`: the stations cannot check the
    /// repository.
    #[error("the project has no repository destination")]
    NeedsRemote,
    #[error(transparent)]
    Store(#[from] DeckError),
}

/// The copy that wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Winner {
    pub digest: String,
    pub source: StationId,
    /// Two homes changed the file; the newest copy won.
    pub conflict: bool,
}

/// The outcome of [`decide`] for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub winner: Option<Winner>,
    /// The stations that get the winning copy, in position order, with the
    /// digest that the write must find (`None`: the file must be missing).
    pub targets: Vec<(StationId, Option<String>)>,
}

/// One station as [`decide`] sees it: its id and position, the digest that
/// the relay last saw or wrote there (`seen`) and the scanned state.
pub type Scanned = (StationId, u32, Option<String>, SecretFileState);

/// The newest copy of `copies` (`(position, mtime, digest, station)`): the
/// highest `mtime_ms`; a tie goes to the lowest position.
fn newest<'a>(copies: &[&'a Copy]) -> Option<&'a Copy> {
    copies
        .iter()
        .copied()
        .max_by_key(|c| (c.mtime_ms, std::cmp::Reverse(c.position)))
}

/// A present copy with a digest.
#[derive(Debug)]
struct Copy {
    station: StationId,
    position: u32,
    seen: Option<String>,
    digest: String,
    size: u64,
    mtime_ms: i64,
}

/// Picks the winning copy of one file (revision R5), without any I/O.
///
/// - Candidates are the present copies. An empty file is a candidate only
///   when no copy has content.
/// - One distinct digest wins.
/// - Else `changed` are the candidates that differ from the digest that the
///   relay last saw on that station (all of them at a first sync: no station
///   has a seen digest). One distinct digest in `changed` wins; several: the
///   newest wins and the file has a conflict.
/// - Else the newest of the `settled` copies (equal to their seen digest)
///   wins; with none, the newest copy.
///
/// A tie of `mtime_ms` goes to the lowest position. The source is the newest
/// copy that has the winning digest. The targets are every present copy with
/// another digest (also an empty file) and every missing copy.
#[must_use]
pub fn decide(states: &[Scanned]) -> Decision {
    let copies: Vec<Copy> = states
        .iter()
        .filter(|(_, _, _, s)| s.state == SecretPresence::Present)
        .filter_map(|(station, position, seen, s)| {
            Some(Copy {
                station: station.clone(),
                position: *position,
                seen: seen.clone(),
                digest: s.digest.clone()?,
                size: s.size.unwrap_or(0),
                mtime_ms: s.mtime_ms.unwrap_or(0),
            })
        })
        .collect();
    let any_content = copies.iter().any(|c| c.size > 0);
    let candidates: Vec<&Copy> = copies
        .iter()
        .filter(|c| !any_content || c.size > 0)
        .collect();
    if candidates.is_empty() {
        return Decision {
            winner: None,
            targets: Vec::new(),
        };
    }
    let mut distinct: Vec<&str> = candidates.iter().map(|c| c.digest.as_str()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let (digest, conflict) = if let [only] = distinct.as_slice() {
        ((*only).to_owned(), false)
    } else {
        let first_sync = states.iter().all(|(_, _, seen, _)| seen.is_none());
        let changed: Vec<&Copy> = candidates
            .iter()
            .copied()
            .filter(|c| first_sync || c.seen.as_deref().is_some_and(|s| s != c.digest))
            .collect();
        if changed.is_empty() {
            let settled: Vec<&Copy> = candidates
                .iter()
                .copied()
                .filter(|c| c.seen.as_deref() == Some(c.digest.as_str()))
                .collect();
            let pool = if settled.is_empty() {
                &candidates
            } else {
                &settled
            };
            (newest_digest(pool), false)
        } else {
            let mut kinds: Vec<&str> = changed.iter().map(|c| c.digest.as_str()).collect();
            kinds.sort_unstable();
            kinds.dedup();
            (newest_digest(&changed), kinds.len() > 1)
        }
    };
    let same: Vec<&Copy> = candidates
        .iter()
        .copied()
        .filter(|c| c.digest == digest)
        .collect();
    let Some(source) = newest(&same) else {
        return Decision {
            winner: None,
            targets: Vec::new(),
        };
    };
    let mut targets: Vec<(u32, StationId, Option<String>)> = states
        .iter()
        .filter_map(|(station, position, _, s)| match s.state {
            SecretPresence::Present => match &s.digest {
                Some(d) if *d != digest => Some((*position, station.clone(), Some(d.clone()))),
                _ => None,
            },
            SecretPresence::Missing => Some((*position, station.clone(), None)),
            _ => None,
        })
        .collect();
    targets.sort_by_key(|(position, _, _)| *position);
    Decision {
        winner: Some(Winner {
            digest,
            source: source.station.clone(),
            conflict,
        }),
        targets: targets.into_iter().map(|(_, s, e)| (s, e)).collect(),
    }
}

/// The digest of the newest of `pool` (a tie: the lowest position).
fn newest_digest(pool: &[&Copy]) -> String {
    newest(pool).map(|c| c.digest.clone()).unwrap_or_default()
}

type ProjectLocks = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;
static LOCKS: LazyLock<ProjectLocks> = LazyLock::new(ProjectLocks::default);

/// Waits for the per-project lock of the runs. A run holds it for its whole
/// time; the change of a secret file list holds it too (R6).
pub async fn lock_project(project: &str) -> tokio::sync::OwnedMutexGuard<()> {
    let lock = {
        let mut map = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
        // A lock that no call holds or awaits is not needed.
        map.retain(|_, lock| Arc::strong_count(lock) > 1);
        Arc::clone(map.entry(project.to_owned()).or_default())
    };
    lock.lock_owned().await
}

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A home that the run uses.
struct Home {
    station: StationId,
    generation: u64,
    position: u32,
}

/// What the scan of one station gave.
enum Scan {
    /// The station is offline for this run.
    Offline,
    /// One state for each file, in the order of the files.
    States(Vec<SecretFileState>),
}

/// The link error of a call to a station.
struct CallError {
    code: String,
    message: String,
}

impl CallError {
    fn timeout() -> Self {
        Self {
            code: "timeout".to_owned(),
            message: "The station did not answer in time.".to_owned(),
        }
    }

    fn link() -> Self {
        Self {
            code: "link_down".to_owned(),
            message: "The link to the station ended.".to_owned(),
        }
    }

    fn bad_answer() -> Self {
        Self {
            code: "bad_answer".to_owned(),
            message: "The station answer does not parse.".to_owned(),
        }
    }
}

/// One call to a station with the deadline [`STEP`]. A refusal carries the
/// code and the text of the station.
async fn call(
    links: &Arc<DeckLink>,
    station: &StationId,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CallError> {
    let Some(control) = links.control_for(station) else {
        return Err(CallError::link());
    };
    match tokio::time::timeout(STEP, control.request(method, None, params)).await {
        Ok(Ok(Outcome::Ok(value))) => Ok(value),
        Ok(Ok(Outcome::Err(e))) => Err(CallError {
            code: e.code,
            message: e.message,
        }),
        Ok(Err(_)) => Err(CallError::link()),
        Err(_) => Err(CallError::timeout()),
    }
}

/// The shared facts of the calls of one run.
struct Run<'a> {
    store: &'a DeckStore,
    links: &'a Arc<DeckLink>,
    project: &'a str,
    dest: &'a str,
    remote_name: &'a str,
}

impl Run<'_> {
    /// The scan of one home. A station without the feature, without a link,
    /// with a failed or unusable scan is offline (decision 26).
    async fn scan(&self, home: &Home, rows: &[SecretFileRow]) -> Scan {
        let negotiated = self
            .store
            .link_feature(&home.station, feature::FLEET_GIT_SECRETS)
            == LinkFeature::Negotiated;
        if !negotiated || self.links.control_for(&home.station).is_none() {
            return Scan::Offline;
        }
        let params = SecretScanParams {
            project_id: self.project.to_owned(),
            generation: home.generation,
            dest: self.dest.to_owned(),
            remote_name: self.remote_name.to_owned(),
            files: rows
                .iter()
                .map(|r| SecretFileKey {
                    path: r.path.clone(),
                    salt: lower_hex(&r.salt),
                })
                .collect(),
        };
        let Ok(value) = serde_json::to_value(&params) else {
            return Scan::Offline;
        };
        let value = match call(self.links, &home.station, METHOD_SCAN, value).await {
            Ok(value) => value,
            Err(e) => {
                tracing::debug!(project = self.project, station = %home.station, code = %e.code, "secret scan failed");
                return Scan::Offline;
            }
        };
        let Ok(answer) = serde_json::from_value::<SecretScanAnswer>(value) else {
            tracing::debug!(project = self.project, station = %home.station, "secret scan answer does not parse");
            return Scan::Offline;
        };
        let same_paths = answer.files.len() == rows.len()
            && answer.files.iter().zip(rows).all(|(s, r)| s.path == r.path);
        // A present file needs its digest, size and time.
        let usable = answer.files.iter().all(|s| {
            s.state != SecretPresence::Present
                || (s.digest.is_some() && s.size.is_some() && s.mtime_ms.is_some())
        });
        if !same_paths || !usable {
            tracing::debug!(project = self.project, station = %home.station, "secret scan answer is unusable");
            return Scan::Offline;
        }
        Scan::States(answer.files)
    }

    /// The read from the source and the writes to the targets of one file.
    /// Each target gets `Ok(())` or the failure.
    async fn transfer(
        &self,
        row: &SecretFileRow,
        homes: &[&Home],
        winner: &Winner,
        targets: &[(StationId, Option<String>)],
    ) -> Vec<Result<(), CallError>> {
        let generation = |station: &StationId| {
            homes
                .iter()
                .find(|h| h.station == *station)
                .map_or(0, |h| h.generation)
        };
        let salt = lower_hex(&row.salt);
        let read = SecretReadParams {
            project_id: self.project.to_owned(),
            generation: generation(&winner.source),
            dest: self.dest.to_owned(),
            remote_name: self.remote_name.to_owned(),
            path: row.path.clone(),
            salt: salt.clone(),
            digest: winner.digest.clone(),
        };
        let content = match self.read(&winner.source, &read).await {
            Ok(content) => content,
            Err(e) => {
                tracing::debug!(project = self.project, path = %row.path, station = %winner.source, code = %e.code, "secret read failed");
                return targets
                    .iter()
                    .map(|_| {
                        Err(CallError {
                            code: e.code.clone(),
                            message: e.message.clone(),
                        })
                    })
                    .collect();
            }
        };
        let mut results = Vec::with_capacity(targets.len());
        for (station, expect) in targets {
            let write = SecretWriteParams {
                project_id: self.project.to_owned(),
                generation: generation(station),
                dest: self.dest.to_owned(),
                remote_name: self.remote_name.to_owned(),
                path: row.path.clone(),
                salt: salt.clone(),
                digest: winner.digest.clone(),
                content_b64: content.clone(),
                expect: expect.clone(),
            };
            let result = self.write(station, write).await;
            match &result {
                Ok(()) => {
                    tracing::info!(project = self.project, path = %row.path, station = %station, outcome = "written", "secret file synced");
                }
                Err(e) => {
                    tracing::debug!(project = self.project, path = %row.path, station = %station, code = %e.code, "secret write failed");
                }
            }
            results.push(result);
        }
        results
    }

    async fn read(
        &self,
        station: &StationId,
        params: &SecretReadParams,
    ) -> Result<String, CallError> {
        let value = serde_json::to_value(params).map_err(|_| CallError::bad_answer())?;
        let answer = call(self.links, station, METHOD_READ, value).await?;
        serde_json::from_value::<SecretReadAnswer>(answer)
            .map(|a| a.content_b64)
            .map_err(|_| CallError::bad_answer())
    }

    async fn write(&self, station: &StationId, params: SecretWriteParams) -> Result<(), CallError> {
        let value = serde_json::to_value(&params).map_err(|_| CallError::bad_answer())?;
        drop(params);
        let answer = call(self.links, station, METHOD_WRITE, value).await?;
        serde_json::from_value::<SecretWriteAnswer>(answer)
            .map(|_| ())
            .map_err(|_| CallError::bad_answer())
    }
}

/// The report of one station that is not in play: offline or refused.
fn station_report(
    station: &StationId,
    state: SecretStationState,
    scanned: Option<&SecretFileState>,
) -> SecretStationReport {
    SecretStationReport {
        station_id: station.to_string(),
        state,
        code: None,
        message: None,
        mtime_ms: scanned.and_then(|s| s.mtime_ms),
        size: scanned.and_then(|s| s.size),
    }
}

/// The result of one file.
struct FileResult {
    report: SecretFileReport,
    baseline: Option<(String, String)>,
    seen: Vec<(String, String)>,
    targets: usize,
    failed: usize,
    digest: Option<String>,
}

impl Run<'_> {
    /// Decides, transfers and reports one file.
    async fn file(&self, row: &SecretFileRow, index: usize, scans: &[(&Home, Scan)]) -> FileResult {
        let mut offline = false;
        let mut refused = false;
        let mut homes: Vec<&Home> = Vec::new();
        let mut reports: BTreeMap<u32, SecretStationReport> = BTreeMap::new();
        let mut states: Vec<Scanned> = Vec::new();
        for (home, scan) in scans {
            match scan {
                Scan::Offline => {
                    offline = true;
                    let report = station_report(&home.station, SecretStationState::Offline, None);
                    reports.insert(home.position, report);
                }
                Scan::States(files) => {
                    let state = &files[index];
                    let seen = row
                        .seen
                        .iter()
                        .find(|(s, _)| *s == home.station.as_str())
                        .map(|(_, d)| d.clone());
                    homes.push(home);
                    match state.state {
                        SecretPresence::Refused | SecretPresence::Unknown => {
                            refused = true;
                            let mut report =
                                station_report(&home.station, SecretStationState::Refused, None);
                            report.code = Some(if state.state == SecretPresence::Unknown {
                                "unknown_state".to_owned()
                            } else {
                                state.code.clone().unwrap_or_default()
                            });
                            reports.insert(home.position, report);
                        }
                        _ => {
                            states.push((home.station.clone(), home.position, seen, state.clone()))
                        }
                    }
                }
            }
        }
        let decision = decide(&states);
        let mut written: Vec<StationId> = Vec::new();
        let mut failures: BTreeMap<StationId, CallError> = BTreeMap::new();
        if let Some(winner) = &decision.winner
            && !decision.targets.is_empty()
        {
            let results = self.transfer(row, &homes, winner, &decision.targets).await;
            for ((station, _), result) in decision.targets.iter().zip(results) {
                match result {
                    Ok(()) => written.push(station.clone()),
                    Err(e) => {
                        failures.insert(station.clone(), e);
                    }
                }
            }
        }
        for (station, position, _, state) in &states {
            let report = match &decision.winner {
                None => station_report(station, SecretStationState::Missing, None),
                Some(_) => {
                    if written.contains(station) {
                        station_report(station, SecretStationState::Written, None)
                    } else if let Some(e) = failures.get(station) {
                        let mut r =
                            station_report(station, SecretStationState::Failed, Some(state));
                        r.code = Some(e.code.clone());
                        r.message = Some(e.message.clone());
                        r
                    } else {
                        station_report(station, SecretStationState::Same, Some(state))
                    }
                }
            };
            reports.insert(*position, report);
        }
        let complete = failures.is_empty() && !offline;
        let outcome = match &decision.winner {
            None => SecretOutcome::Nothing,
            Some(_) if !complete => SecretOutcome::Partial,
            Some(_) if decision.targets.is_empty() => SecretOutcome::Same,
            Some(_) => SecretOutcome::Copied,
        };
        let baseline = decision
            .winner
            .as_ref()
            .filter(|_| complete && !refused)
            .map(|w| (w.digest.clone(), w.source.to_string()));
        let seen = match &decision.winner {
            Some(winner) => states
                .iter()
                .filter(|(station, _, _, state)| {
                    written.contains(station) || state.digest.as_deref() == Some(&winner.digest)
                })
                .map(|(station, ..)| (station.to_string(), winner.digest.clone()))
                .collect(),
            None => Vec::new(),
        };
        let report = SecretFileReport {
            path: row.path.clone(),
            synced_at: baseline.as_ref().map(|_| self.store.now_ms()),
            source: decision.winner.as_ref().map(|w| w.source.to_string()),
            conflict: decision.winner.as_ref().is_some_and(|w| w.conflict),
            outcome,
            stations: reports.into_values().collect(),
        };
        FileResult {
            report,
            baseline,
            seen,
            targets: decision.targets.len(),
            failed: failures.len(),
            digest: decision.winner.map(|w| w.digest),
        }
    }
}

/// One file that did not run: its row is gone or has another salt.
fn skipped(path: &str) -> SecretFileReport {
    SecretFileReport {
        path: path.to_owned(),
        synced_at: None,
        source: None,
        conflict: false,
        outcome: SecretOutcome::NotRun,
        stations: Vec::new(),
    }
}

/// The audit row of a file that had a write or a conflict.
async fn audit(store: &DeckStore, actor: &str, project: &str, result: &FileResult) {
    let report = &result.report;
    if result.targets == 0 && !report.conflict {
        return;
    }
    let written = report
        .stations
        .iter()
        .filter(|s| s.state == SecretStationState::Written)
        .count();
    let digest: String = result
        .digest
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(DIGEST_PREFIX)
        .collect();
    let outcome = if report.outcome == SecretOutcome::Partial {
        AuditResult::Failed
    } else {
        AuditResult::Ok
    };
    let row = NewAudit::new(actor, "project.secret_sync", outcome)
        .target(project)
        .detail(format!(
            "{} from {} written {} failed {} conflict {} digest {}",
            report.path,
            report.source.as_deref().unwrap_or("-"),
            written,
            result.failed,
            report.conflict,
            digest
        ));
    store.audit_or_log(row).await;
}

/// Runs one project: scan, decide, transfer and report for each listed file.
///
/// # Errors
/// [`SyncError::UnknownProject`], [`SyncError::NeedsRemote`] and
/// [`SyncError::Store`]. A station that fails is part of the report.
pub async fn sync_project(
    store: &DeckStore,
    links: &Arc<DeckLink>,
    project: &str,
    actor: &str,
) -> Result<SecretSyncReport, SyncError> {
    let _lock = lock_project(project).await;
    let Some(found) = store.project(project.to_owned()).await? else {
        return Err(SyncError::UnknownProject);
    };
    let Some(dest) = found.remote_dest.clone() else {
        return Err(SyncError::NeedsRemote);
    };
    let rows = store.secret_files(project.to_owned()).await?;
    if rows.is_empty() {
        return Ok(SecretSyncReport { files: Vec::new() });
    }
    let homes: Vec<Home> = found
        .homes
        .iter()
        .filter(|h| h.state == GitHomeState::Ready && h.path.is_some())
        .map(|h| Home {
            station: h.station_id.clone(),
            generation: h.generation,
            position: h.position,
        })
        .collect();
    let run = Run {
        store,
        links,
        project,
        dest: &dest,
        remote_name: &found.git.push_remote,
    };
    let scans = futures_util::future::join_all(homes.iter().map(|h| run.scan(h, &rows))).await;
    let scans: Vec<(&Home, Scan)> = homes.iter().zip(scans).collect();
    let mut files = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        // The list can change between the scan and the transfer (R6).
        let current = store
            .secret_file(project.to_owned(), row.path.clone())
            .await?;
        let Some(current) = current.filter(|c| c.salt == row.salt) else {
            files.push(skipped(&row.path));
            continue;
        };
        let result = run.file(&current, index, &scans).await;
        let salt = current.salt.clone();
        audit(store, actor, project, &result).await;
        store
            .secret_file_done(
                project.to_owned(),
                current.path.clone(),
                salt.clone(),
                result.baseline.clone(),
                result.report.clone(),
            )
            .await?;
        if !result.seen.is_empty() {
            store
                .secret_seen_set(project.to_owned(), current.path.clone(), salt, result.seen)
                .await?;
        }
        tracing::debug!(project, path = %current.path, outcome = ?result.report.outcome, "secret file run");
        files.push(result.report);
    }
    Ok(SecretSyncReport { files })
}

/// Starts the periodic task: the first run 60 s after the start, then one
/// every 15 minutes, for every project with a secret file and two homes. It
/// ends when `shutdown` turns true.
pub fn spawn(store: DeckStore, links: Arc<DeckLink>, mut shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move {
        let mut wait = FIRST_RUN;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                    continue;
                }
            }
            wait = EVERY;
            if *shutdown.borrow() {
                return;
            }
            let projects = match store.secret_projects().await {
                Ok(projects) => projects,
                Err(e) => {
                    tracing::warn!(error = %e, "secret projects");
                    continue;
                }
            };
            for project in projects {
                if *shutdown.borrow() {
                    return;
                }
                match sync_project(&store, &links, &project, "deck").await {
                    Ok(_) | Err(SyncError::NeedsRemote | SyncError::UnknownProject) => {}
                    Err(e) => tracing::warn!(project = %project, error = %e, "secret sync"),
                }
            }
        }
    });
}

#[cfg(test)]
#[path = "fleet_git_secrets_tests.rs"]
mod tests;
