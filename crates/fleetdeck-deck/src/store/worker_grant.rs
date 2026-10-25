//! The default worker grant of an eligible session start (MCP P4a, design
//! `docs/v2/design/mcp-p4a.md` §3, §5, §6.4, §7).
//!
//! - [`DeckStore::submit_start_with_worker_decision`] decides in one write
//!   transaction whether a start gets a worker grant, stores the decision
//!   (`worker_attach`), the grant and the pending command together, and
//!   answers a retry of the same command ID from the stored decision.
//! - The grant creation step `create_grant_tx` and the desktop cap rules live
//!   here so that `grants.rs` stays small; `prepare_grant` is in `grant_rules.rs`.
//! - A worker grant is revoked without the owner when the station skipped it
//!   or its start failed, and the retention of §7 removes old rows.

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_core::config_grant_limits::GrantLimits;
use fleetdeck_core::config_worker_grant::WorkerGrantConfig;
use fleetdeck_core::ids::new_id;
use fleetdeck_proto::command::{
    CommandOrigin, ControlSkill, CoordinatorSpec, GrantKind, GrantScope, RETRY_WINDOW_MS,
    SessionCommand,
};
use fleetdeck_proto::coordinator_desktop_text::limits;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandEnvelope, CommandId, StationId, grant_limit::NO_LIMIT};
use rusqlite::{Connection, OptionalExtension, params};

use super::coordinator::grant_narrowing::{self, Removal};
use super::display_directives_coordinator::invalidate_coordinator_directives;
use super::grant_scopes::{active_grants, in_effect, sync_coordinate_scopes};
use super::grants::grant_rules::prepare_grant;
use super::grants::{Grant, GrantClass, NewGrant, get_grant};
use super::grants_input::scope_text;
use super::{DeckCommand, DeckError, DeckStore, LinkFeature, NewCommand, Result, call, commands};

#[path = "schedules_author_schema.rs"]
pub(super) mod schedules_author_schema;
#[path = "tokens_kind_schema.rs"]
pub(super) mod tokens_kind_schema;
#[path = "worker_attach_schema.rs"]
pub(super) mod worker_attach_schema;

/// The default of the most worker grants in effect deck-wide (§7); the limit
/// is `grant_limits.worker_grants`.
pub const WORKER_GRANT_CAP: usize = 200;
/// The most stations of one worker grant (the limit of an owner grant).
const MAX_WORKER_STATIONS: usize = 16;
/// The default of the highest spawn cap of a worker grant (§2); the limit is
/// `grant_limits.worker_spawn_cap`.
pub const MAX_WORKER_SPAWN_CAP: u32 = 10;
/// How long the retention keeps a terminal worker grant and a `none`
/// decision (§7).
pub const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Where the authority of a worker grant comes from (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// The owner's board login started the session.
    Owner,
    /// The owner's login asked for an issue batch.
    IssueBatch,
    /// A schedule that the owner's login saved last.
    Schedule,
}

impl Provenance {
    /// The audit actor and the `worker_attach.reason` of a grant.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::IssueBatch => "issue_batch",
            Self::Schedule => "schedule",
        }
    }
}

/// One start for the attachment function.
#[derive(Debug, Clone)]
pub struct WorkerStart {
    /// The canonical station ID.
    pub station: StationId,
    /// The start as the board sent it (`coordinator: None`).
    pub command: SessionCommand,
    pub command_id: CommandId,
    pub provenance: Provenance,
    /// The author of the stored command row.
    pub author: Option<String>,
    /// The live `worker_grant` setting.
    pub setting: WorkerGrantConfig,
}

/// The stored command and whether this call created it.
#[derive(Debug, Clone)]
pub struct SubmitOutcome {
    pub command: DeckCommand,
    pub new: bool,
}

/// The rules of the desktop caps of a new grant (R13 P6 §8.1), the same for
/// the board request and for `create_grant`: the class rule first, then the
/// range, then the multiple. The text is fixed and shows no value.
///
/// # Errors
/// The fixed text of the first rule that fails.
pub fn check_desktop_caps(
    classes: &BTreeSet<GrantClass>,
    desktop_cap: Option<u32>,
    capture_cap: Option<u32>,
    image_byte_cap: Option<u64>,
) -> std::result::Result<(), &'static str> {
    let capture = classes.contains(&GrantClass::Capture);
    let desktop = capture || classes.contains(&GrantClass::Desktop);
    if desktop_cap.is_some() && !desktop {
        return Err("desktop_cap needs the desktop or capture class");
    }
    if (capture_cap.is_some() || image_byte_cap.is_some()) && !capture {
        return Err("capture_cap and image_byte_cap need the capture class");
    }
    if desktop_cap.is_some_and(|c| c > limits::DESKTOP_CAP_MAX) {
        return Err("desktop_cap is 0 to 5000");
    }
    if capture_cap.is_some_and(|c| c > limits::CAPTURE_CAP_MAX) {
        return Err("capture_cap is 0 to 5000");
    }
    if image_byte_cap.is_some_and(|c| c > limits::IMAGE_BYTE_CAP_MAX) {
        return Err("image_byte_cap is 0 to 1073741824");
    }
    if image_byte_cap.is_some_and(|c| c % limits::IMAGE_BYTE_CAP_UNIT != 0) {
        return Err("image_byte_cap is a multiple of 1048576");
    }
    Ok(())
}

/// The rules of the recording caps of a new grant (R13 P5b-2 §13.2), the same
/// for the board request and for `create_grant`: the class rule first (a
/// non-zero cap needs the `capture` class), then the ranges. The text is
/// fixed and shows no value.
///
/// # Errors
/// The fixed text of the first rule that fails.
pub fn check_recording_caps(
    classes: &BTreeSet<GrantClass>,
    recording_cap: u32,
    recording_second_cap: u32,
) -> std::result::Result<(), &'static str> {
    if (recording_cap != 0 || recording_second_cap != 0) && !classes.contains(&GrantClass::Capture)
    {
        return Err("recording caps need the capture class");
    }
    if recording_cap > limits::RECORDING_CAP_MAX {
        return Err("recording_cap is 0 to 5000");
    }
    if recording_second_cap > limits::RECORDING_SECOND_CAP_MAX {
        return Err("recording_second_cap is 0 to 600000");
    }
    Ok(())
}

/// The insert of a prepared grant in the caller's transaction, with the
/// derived `coordinate` scopes. A start command that has a grant returns it
/// (DL-217). Returns the grant and the stations whose scopes changed.
pub(super) fn create_grant_tx(
    tx: &Connection,
    grant_id: &str,
    grant: &NewGrant,
    now: i64,
) -> Result<(Grant, BTreeSet<StationId>)> {
    if let Some(start) = grant.start_command_id.as_deref() {
        let found: Option<String> = tx
            .query_row(
                "SELECT grant_id FROM tokens WHERE start_command_id = ?1",
                [start],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = found {
            let found = get_grant(tx, &id)?
                .ok_or_else(|| DeckError::Corrupt("a found grant is gone".to_owned()))?;
            return Ok((found, BTreeSet::new()));
        }
    }
    tx.execute(
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations,
             spawn_cap, max_modes, idle_ttl_ms, created_at, expires_at, state,
             start_command_id, scope, answer_cap, desktop_cap, capture_cap,
             image_byte_cap, recording_cap, recording_second_cap, allow_input,
             input_scope, input_cap, input_character_cap, allow_click, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', ?11, ?12, ?13,
             ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
        params![
            grant_id,
            grant.station_id.as_str(),
            grant.session_id.0,
            serde_json::to_string(&grant.classes)?,
            serde_json::to_string(&grant.stations)?,
            grant.spawn_cap,
            serde_json::to_string(&grant.max_modes)?,
            grant.idle_ttl_ms,
            now,
            now.saturating_add(grant.ttl_ms),
            grant.start_command_id,
            if grant.scope == GrantScope::Fleet {
                "fleet"
            } else {
                "lineage"
            },
            grant.answer_cap,
            grant.desktop_cap,
            grant.capture_cap,
            // SQLite integers are signed 64-bit; the cap is at most 1 GiB.
            grant.image_byte_cap.and_then(|c| i64::try_from(c).ok()),
            grant.recording_cap,
            grant.recording_second_cap,
            grant.allow_input,
            grant.input_scope.map(scope_text),
            grant.input_cap,
            grant.input_character_cap,
            grant.allow_click,
            if grant.kind == GrantKind::Worker {
                "worker"
            } else {
                "coordinator"
            },
        ],
    )?;
    let stored = get_grant(tx, grant_id)?
        .ok_or_else(|| DeckError::Corrupt("an inserted grant is gone".to_owned()))?;
    // The grant lets each station that it names relay coordinator calls: the
    // derived scope `coordinate` follows in the same transaction (DL-211,
    // design §2.3 item 1).
    let changed = sync_coordinate_scopes(tx, now)?;
    Ok((stored, changed))
}

/// An audit row in the caller's transaction.
fn audit_tx(
    tx: &Connection,
    now: i64,
    (actor, action): (&str, &str),
    (target, command_id, detail): (&str, &str, &str),
) -> Result<()> {
    tx.execute(
        "INSERT INTO audit (at, actor, action, target, command_id, result, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, 'ok', ?6)",
        params![now, actor, action, target, command_id, detail],
    )?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The digest of the board's command (§5, shape 7c).
fn original_digest(station: &StationId, command: &SessionCommand) -> Result<String> {
    let bytes = serde_json::to_vec(&(station, command))?;
    Ok(hex(
        ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref()
    ))
}

/// The stations of a new worker grant (§2, decision 12): the start's own
/// station, then the configured ones that are paired. `["all"]` is a
/// snapshot of every paired station. Above 16 stations the own station and
/// the first 15 others by name stay.
fn grant_stations(
    tx: &Connection,
    own: &StationId,
    setting: &WorkerGrantConfig,
) -> Result<BTreeSet<StationId>> {
    let mut stmt = tx.prepare("SELECT name, station_id FROM stations ORDER BY name")?;
    let paired = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut others: BTreeMap<String, StationId> = BTreeMap::new();
    for (name, id) in paired {
        let wanted =
            setting.all_stations() || setting.stations.iter().any(|e| *e == name || *e == id);
        if wanted && id != own.as_str() {
            others.insert(name, StationId::from(id));
        }
    }
    let mut set = BTreeSet::from([own.clone()]);
    set.extend(others.into_values().take(MAX_WORKER_STATIONS - 1));
    Ok(set)
}

/// The worker grants in effect now (§7).
pub(super) fn worker_grants_in_effect(tx: &Connection, now: i64) -> Result<usize> {
    Ok(active_grants(tx)?
        .iter()
        .filter(|g| g.kind == GrantKind::Worker && in_effect(g, now))
        .count())
}

/// True when the worker grants in effect reach `grant_limits.worker_grants`
/// (never with no limit).
pub(super) fn over_worker_grants(tx: &Connection, now: i64, limits: &GrantLimits) -> Result<bool> {
    let Some(most) = limits.worker_grants else {
        return Ok(false);
    };
    Ok(worker_grants_in_effect(tx, now)? >= usize::try_from(most).unwrap_or(usize::MAX))
}

/// The stored command of `command_id` when it is the same request as
/// `original` (the deck's worker `CoordinatorSpec` is not part of the
/// request), or [`DeckError::CommandIdReused`] (shape 7e).
fn same_request(
    old: DeckCommand,
    station: &StationId,
    original: &serde_json::Value,
) -> Result<DeckCommand> {
    let mut stored = old.envelope.get("command").cloned().unwrap_or_default();
    if let Some(spec) = stored.get_mut("spec").and_then(|s| s.as_object_mut()) {
        let worker = spec
            .get("coordinator")
            .and_then(|c| c.get("kind"))
            .and_then(|k| k.as_str())
            == Some("worker");
        if worker {
            spec.remove("coordinator");
        }
    }
    if old.station_id == *station && old.method == "session.start" && stored == *original {
        Ok(old)
    } else {
        Err(DeckError::CommandIdReused(old.command_id.0))
    }
}

/// The worker `CoordinatorSpec` of a stored worker grant, from the stored
/// values only (§4).
fn worker_spec(g: &Grant) -> CoordinatorSpec {
    CoordinatorSpec {
        grant_id: g.grant_id.clone(),
        classes: ["read", "mail", "drive", "spawn"]
            .map(str::to_owned)
            .to_vec(),
        idle_ttl_ms: u64::try_from(g.idle_ttl_ms).unwrap_or(0),
        expires_at: g.expires_at,
        control_skill: ControlSkill {
            version: crate::http::grant_routes::CONTROL_SKILL_VERSION.to_owned(),
            text: crate::http::grant_routes::CONTROL_SKILL.to_owned(),
        },
        scope: GrantScope::Lineage,
        answer_cap: None,
        desktop_cap: None,
        capture_cap: None,
        image_byte_cap: None,
        allow_recording: false,
        recording_cap: None,
        recording_second_cap: None,
        allow_input: false,
        input_scope: None,
        allow_click: false,
        input_cap: None,
        input_character_cap: None,
        stations: Vec::new(),
        spawn_cap: None,
        max_modes: BTreeMap::new(),
        kind: g.kind,
    }
}

/// What one decision did, for the notes after the commit.
struct Attached {
    command: DeckCommand,
    new: bool,
    scopes: BTreeSet<StationId>,
}

/// The decision of §3 and its reason.
enum Decision {
    Grant,
    None(&'static str),
}

impl DeckStore {
    /// Decides in ONE write transaction whether the `Start` of `req` gets a
    /// worker grant, and stores the decision (`worker_attach`), the grant
    /// (`kind = worker`, `start_command_id` = the command ID, the derived
    /// `coordinate` scopes), the audit row and the pending command. A retry
    /// of the same command ID returns the stored command and reuses the
    /// stored decision; the same ID with another start is
    /// [`DeckError::CommandIdReused`] (§5).
    ///
    /// # Errors
    /// [`DeckError::Invalid`] when the command is not a start without a
    /// coordinator request; [`DeckError::CommandIdReused`];
    /// [`DeckError::UnknownStation`]; [`DeckError::NotSynced`]; a database
    /// error.
    pub async fn submit_start_with_worker_decision(
        &self,
        req: WorkerStart,
    ) -> Result<SubmitOutcome> {
        let SessionCommand::Start { spec, .. } = &req.command else {
            return Err(DeckError::Invalid(
                "a worker decision needs a start".to_owned(),
            ));
        };
        if spec.coordinator.is_some() {
            return Err(DeckError::Invalid(
                "a worker decision needs a start without a coordinator request".to_owned(),
            ));
        }
        let now = self.now_ms();
        let links = [feature::WORKER_GRANT, feature::COORDINATOR]
            .iter()
            .all(|f| self.link_feature(&req.station, f) == LinkFeature::Negotiated);
        let grant_id: String = new_id(self.inner.ids.as_ref());
        let limits = self.knobs().grant;
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let out = attach(&tx, &req, (links, &limits), &grant_id, now)?;
            tx.commit()?;
            Ok(out)
        })
        .await?;
        self.note_scope_changes(out.scopes);
        if out.new {
            self.commands_changed();
        }
        Ok(SubmitOutcome {
            command: out.command,
            new: out.new,
        })
    }
}

/// The body of [`DeckStore::submit_start_with_worker_decision`].
fn attach(
    tx: &Connection,
    req: &WorkerStart,
    (links, limits): (bool, &GrantLimits),
    grant_id: &str,
    now: i64,
) -> Result<Attached> {
    let digest = original_digest(&req.station, &req.command)?;
    let original = serde_json::to_value(&req.command)?;
    let id = &req.command_id;
    // A retry reads the stored decision first (§5).
    let stored: Option<String> = tx
        .query_row(
            "SELECT original_sha256 FROM worker_attach WHERE command_id = ?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let existing = commands::get(tx, id)?;
    match (stored, existing) {
        (Some(sha), _) if sha != digest => {
            return Err(DeckError::CommandIdReused(id.0.clone()));
        }
        (Some(_), Some(command)) => {
            return Ok(Attached {
                command,
                new: false,
                scopes: BTreeSet::new(),
            });
        }
        (Some(_), None) => {
            return Err(DeckError::Corrupt(
                "a stored decision has no command".to_owned(),
            ));
        }
        // A pruned decision, or a start that took the plain path: the
        // retained command row answers (shape 7e).
        (None, Some(old)) => {
            return Ok(Attached {
                command: same_request(old, &req.station, &original)?,
                new: false,
                scopes: BTreeSet::new(),
            });
        }
        (None, None) => {}
    }
    let rec = super::stations::station_by_id(tx, &req.station)?
        .ok_or_else(|| DeckError::UnknownStation(req.station.0.clone()))?;
    let target = rec
        .incarnation
        .ok_or_else(|| DeckError::NotSynced(rec.name.clone()))?;
    let decision = if !req.setting.enabled {
        Decision::None("setting_off")
    } else if !links {
        Decision::None("link_features")
    } else if over_worker_grants(tx, now, limits)? {
        audit_tx(
            tx,
            now,
            (req.provenance.name(), "coordinator.worker_grant_skipped"),
            (rec.station_id.as_str(), id.as_str(), "worker_cap"),
        )?;
        Decision::None("worker_cap")
    } else {
        Decision::Grant
    };
    let mut command = req.command.clone();
    let mut scopes = BTreeSet::new();
    let (reason, grant_ref) = match decision {
        Decision::None(reason) => (reason, None),
        Decision::Grant => {
            let w = &req.setting;
            let mut grant = NewGrant::new(req.station.clone(), "".into());
            grant.kind = GrantKind::Worker;
            grant.classes = BTreeSet::from([
                GrantClass::Read,
                GrantClass::Mail,
                GrantClass::Drive,
                GrantClass::Spawn,
            ]);
            grant.stations = grant_stations(tx, &req.station, w)?;
            grant.spawn_cap = w.spawn_cap.unwrap_or(NO_LIMIT);
            grant.max_modes = BTreeMap::from([
                ("claude".to_owned(), w.claude_mode.clone()),
                ("codex".to_owned(), w.codex_mode.clone()),
            ]);
            grant.ttl_ms = i64::from(w.lifetime_hours) * 3_600_000;
            grant.idle_ttl_ms =
                Option::map_or(w.idle_minutes, grant.ttl_ms, |m| i64::from(m) * 60_000);
            grant.start_command_id = Some(id.0.clone());
            let grant = prepare_grant(grant, limits, w.allow_bypass)?;
            let (g, changed) = create_grant_tx(tx, grant_id, &grant, now)?;
            scopes = changed;
            if let SessionCommand::Start { spec, .. } = &mut command {
                spec.coordinator = Some(Box::new(worker_spec(&g)));
            }
            let named = g
                .stations
                .iter()
                .map(StationId::as_str)
                .collect::<Vec<_>>()
                .join(",");
            audit_tx(
                tx,
                now,
                (req.provenance.name(), "coordinator.worker_grant_created"),
                (
                    &g.grant_id,
                    id.as_str(),
                    &crate::http::grant_routes::audit_detail(&g, &named),
                ),
            )?;
            (req.provenance.name(), Some(g.grant_id))
        }
    };
    tx.execute(
        "INSERT INTO worker_attach (command_id, original_sha256, decision, reason, grant_id,
             created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id.as_str(),
            digest,
            if grant_ref.is_some() { "grant" } else { "none" },
            reason,
            grant_ref,
            now
        ],
    )?;
    let retry_until = now.saturating_add(RETRY_WINDOW_MS);
    let method = command.method().to_owned();
    let envelope = serde_json::to_value(CommandEnvelope {
        authority: None,
        command_id: id.clone(),
        target_incarnation: Some(target),
        retry_until,
        origin: CommandOrigin::Board,
        command,
    })?;
    commands::insert(
        tx,
        &NewCommand {
            command_id: id.clone(),
            station_id: req.station.clone(),
            method,
            origin: crate::link::router::origin_name(CommandOrigin::Board).to_owned(),
            envelope,
            retry_until,
            author: req.author.clone(),
        },
        now,
    )?;
    let command = commands::get(tx, id)?
        .ok_or_else(|| DeckError::Corrupt("an inserted command is gone".to_owned()))?;
    Ok(Attached {
        command,
        new: true,
        scopes,
    })
}

/// Revokes a grant in the caller's transaction. Returns whether it changed
/// and the stations whose scopes changed.
pub(super) fn revoke_tx(
    tx: &Connection,
    grant_id: &str,
    reason: &str,
    now: i64,
) -> Result<(bool, BTreeSet<StationId>)> {
    let n = tx.execute(
        "UPDATE tokens SET state = 'revoked', revoked_reason = ?2
         WHERE grant_id = ?1 AND state != 'revoked'",
        params![grant_id, reason],
    )?;
    if n == 1 {
        let none = BTreeSet::new(); // a revoked grant allows nothing (MCP P4b §7.2)
        grant_narrowing::fail_removed_tx(tx, grant_id, &none, Removal::Revoked, now)?;
    }
    invalidate_coordinator_directives(tx, grant_id)?;
    let mut changed = sync_coordinate_scopes(tx, now)?;
    changed.extend(super::grants_lifecycle::ended_station(tx, grant_id, n)?);
    Ok((n == 1, changed))
}

/// Revokes the worker grant of a start command that did not run
/// (`undelivered`, `start_failed`). Returns the stations whose scopes
/// changed. A start without a worker grant changes nothing.
pub(super) fn revoke_for_start(
    tx: &Connection,
    command_id: &CommandId,
    reason: &str,
    now: i64,
) -> Result<BTreeSet<StationId>> {
    let found: Option<String> = tx
        .query_row(
            "SELECT grant_id FROM tokens
             WHERE start_command_id = ?1 AND kind = 'worker' AND state != 'revoked'",
            [command_id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    match found {
        Some(grant_id) => Ok(revoke_tx(tx, &grant_id, reason, now)?.1),
        None => Ok(BTreeSet::new()),
    }
}

/// Folds `coordinator.worker_outcome` of `station` (§6.4): only a worker
/// grant of the same station and the same start command is revoked
/// (`handoff_skipped`); any other event is ignored and logged.
pub(super) fn apply_outcome(
    tx: &Connection,
    station: &StationId,
    (grant_id, start_command_id): (&str, &CommandId),
    now: i64,
) -> Result<BTreeSet<StationId>> {
    let grant = get_grant(tx, grant_id)?;
    let named = tx
        .query_row(
            "SELECT 1 FROM tokens WHERE grant_id = ?1 AND kind = 'worker' AND station_id = ?2
                 AND start_command_id = ?3",
            params![grant_id, station.as_str(), start_command_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if grant.is_none() || !named {
        tracing::warn!(
            station = %station,
            grant = grant_id,
            "worker outcome for a grant that is not a worker grant of this start; ignored"
        );
        return Ok(BTreeSet::new());
    }
    Ok(revoke_tx(tx, grant_id, "handoff_skipped", now)?.1)
}

/// The retention of §7, run by `sweep_grants`: (a) worker grants that are
/// terminal for more than 7 days with their `worker_attach` and lineage rows,
/// and (b) every `none` decision older than 7 days.
pub(super) fn prune(tx: &Connection, now: i64) -> Result<()> {
    let before = now.saturating_sub(RETENTION_MS);
    let old = "SELECT grant_id FROM tokens WHERE kind = 'worker'
               AND state IN ('revoked', 'expired') AND expires_at < ?1";
    tx.execute(
        &format!("DELETE FROM coordinator_lineage WHERE grant_id IN ({old})"),
        [before],
    )?;
    tx.execute(
        &format!("DELETE FROM worker_attach WHERE grant_id IN ({old})"),
        [before],
    )?;
    tx.execute(
        "DELETE FROM tokens WHERE kind = 'worker'
         AND state IN ('revoked', 'expired') AND expires_at < ?1",
        [before],
    )?;
    tx.execute(
        "DELETE FROM worker_attach WHERE decision = 'none' AND created_at < ?1",
        [before],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "worker_grant_tests.rs"]
mod tests;
