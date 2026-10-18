//! Coordinator grants (M3 6-5, ADR-006 §4; PLAN §10 table `tokens`).
//!
//! The deck is the authority for a coordinator session: its classes, its
//! target stations, its spawn cap with the durable `spawn_used` counter, its
//! mode ceilings per provider family, its idle TTL and its expiry. The
//! binding fields come from the station's `coordinator.bound` facts; a call
//! is valid only with the same binding (§6.2 step 3).

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::coordinator_desktop_text::limits;
use fleetdeck_proto::mutation::{CommandAuthority, CoordinatorBinding};
use fleetdeck_proto::{ProviderInstanceId, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::display_directives_coordinator::invalidate_coordinator_directives;
use super::grant_scopes::sync_coordinate_scopes;
pub(super) use super::grants_lifecycle::{apply_projection, record_lifecycle, renew_generation};
use super::{DeckError, DeckStore, Result, call, new_id};

#[path = "fleet_answers.rs"]
mod fleet_answers;
pub use fleet_answers::FleetAnswers;

/// The default spawn cap (ADR-006 §4).
pub const DEFAULT_SPAWN_CAP: u32 = 3;
/// The highest spawn cap.
pub const MAX_SPAWN_CAP: u32 = 50;
/// The default idle TTL: 30 min.
pub const DEFAULT_IDLE_TTL_MS: i64 = 30 * 60 * 1000;
/// The longest idle TTL: 24 h.
pub const MAX_IDLE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// The default lifetime: 12 h.
pub const DEFAULT_TTL_MS: i64 = 12 * 60 * 60 * 1000;
/// The longest lifetime: 7 days.
pub const MAX_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// The default answer cap of a grant with the `answer` class (ADR-008 §3).
pub const DEFAULT_ANSWER_CAP: u32 = 500;
/// The highest answer cap.
pub const MAX_ANSWER_CAP: u32 = 5000;

/// A class of coordinator operations (ADR-006 §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantClass {
    Read,
    Mail,
    Drive,
    Spawn,
    /// Every new read query (ADR-008 §3, fleet control).
    Inspect,
    /// Resume, rename, set mode, move to a project, start in a project
    /// (fleet control).
    Manage,
    /// Answer approvals and questions (fleet control).
    Answer,
    /// The coordinator desktop tools: notify, preview, wall directives
    /// (R13 P6 §2.1; fleet control, OD-3).
    Desktop,
    /// The coordinator screenshot tools (R13 P6 §2.1; fleet control, OD-3).
    Capture,
}

impl GrantClass {
    /// True for the classes of fleet control (ADR-008 §2, R13 P6 OD-3).
    #[must_use]
    pub fn is_fleet_control(self) -> bool {
        matches!(
            self,
            Self::Inspect | Self::Manage | Self::Answer | Self::Desktop | Self::Capture
        )
    }

    /// True for the classes that change something; a session that holds an
    /// active grant with one of them is a coordinator (ADR-008 §4.2 rule 3).
    #[must_use]
    pub fn is_mutating(self) -> bool {
        !matches!(self, Self::Read | Self::Inspect)
    }
}

/// The state of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantState {
    Active,
    Revoked,
    Expired,
}

/// An owner-authorized grant before it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGrant {
    pub station_id: StationId,
    pub session_id: SessionId,
    pub classes: BTreeSet<GrantClass>,
    /// The target stations; the coordinator's own station by default.
    pub stations: BTreeSet<StationId>,
    pub spawn_cap: u32,
    /// Provider family (`claude`, `codex`) → the most permissive native
    /// mode of a spawned child. A family without an entry cannot spawn.
    pub max_modes: BTreeMap<String, String>,
    pub idle_ttl_ms: i64,
    /// The lifetime from creation.
    pub ttl_ms: i64,
    /// The owner's session start that creates the grant (DL-217): the
    /// session id stays empty until the station names the session, and a
    /// resend of the same start returns the same grant.
    pub start_command_id: Option<String>,
    /// The reach of the grant (ADR-008 §3).
    pub scope: GrantScope,
    /// The lifetime answer cap; `None` without the `answer` class.
    pub answer_cap: Option<u32>,
    /// The desktop action cap (R13 P6 §8.1); `None` without the `desktop` or
    /// `capture` class. `create_grant` fills the default 300.
    pub desktop_cap: Option<u32>,
    /// The screenshot cap; `None` without the `capture` class (default 20).
    pub capture_cap: Option<u32>,
    /// The image-read byte cap; `None` without the `capture` class (default
    /// 64 MiB).
    pub image_byte_cap: Option<u64>,
    /// The recording count cap (R13 P5b-2 §13.2); 0 is no recording
    /// allowance. A non-zero cap needs the `capture` class.
    pub recording_cap: u32,
    /// The recording seconds cap; 0 is no recording allowance.
    pub recording_second_cap: u32,
}

impl NewGrant {
    /// The default grant of a coordinator session: `read` and `mail` on
    /// its own station, no spawn ceiling, 30 min idle TTL, 12 h lifetime.
    #[must_use]
    pub fn new(station_id: StationId, session_id: SessionId) -> Self {
        Self {
            stations: BTreeSet::from([station_id.clone()]),
            station_id,
            session_id,
            classes: BTreeSet::from([GrantClass::Read, GrantClass::Mail]),
            spawn_cap: DEFAULT_SPAWN_CAP,
            max_modes: BTreeMap::new(),
            idle_ttl_ms: DEFAULT_IDLE_TTL_MS,
            ttl_ms: DEFAULT_TTL_MS,
            start_command_id: None,
            scope: GrantScope::Lineage,
            answer_cap: None,
            desktop_cap: None,
            capture_cap: None,
            image_byte_cap: None,
            recording_cap: 0,
            recording_second_cap: 0,
        }
    }
}

/// A stored grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub grant_id: String,
    pub station_id: StationId,
    pub session_id: SessionId,
    pub classes: BTreeSet<GrantClass>,
    pub stations: BTreeSet<StationId>,
    pub spawn_cap: u32,
    pub spawn_used: u32,
    pub max_modes: BTreeMap<String, String>,
    pub idle_ttl_ms: i64,
    pub created_at: i64,
    pub expires_at: i64,
    pub state: GrantState,
    pub revoked_reason: Option<String>,
    /// `None` until the station reports the binding.
    pub binding: Option<CoordinatorBinding>,
    pub last_used_at: Option<i64>,
    /// The reach of the grant (ADR-008 §3).
    pub scope: GrantScope,
    /// The lifetime answer cap; `None` without the `answer` class.
    pub answer_cap: Option<u32>,
    /// The answers that the grant made, for good (never pruned).
    pub answer_used: u32,
    /// The desktop action cap (R13 P6 §8.1); `None` without the `desktop` or
    /// `capture` class.
    pub desktop_cap: Option<u32>,
    /// The desktop actions that the grant made, for good.
    pub desktop_used: u32,
    /// The screenshot cap; `None` without the `capture` class.
    pub capture_cap: Option<u32>,
    /// The screenshots that the grant made, for good.
    pub capture_used: u32,
    /// The image-read byte cap; `None` without the `capture` class.
    pub image_byte_cap: Option<u64>,
    /// The image bytes that the grant reserved, for good.
    pub image_byte_used: u64,
    /// The distinct well-formed mutation attempts, for good (§8.1).
    pub desktop_attempts: u32,
    /// The recording count cap (R13 P5b-2 §13.2); 0 for a grant without the
    /// recording allowance.
    pub recording_cap: u32,
    /// The recordings that the grant made, for good.
    pub recording_used: u32,
    /// The recording seconds cap; 0 for a grant without the allowance.
    pub recording_second_cap: u32,
    /// The recording seconds that the grant charged, for good.
    pub recording_second_used: u32,
}

impl Grant {
    /// A fleet-control grant has the fleet scope or a fleet-control class
    /// (ADR-008 §2).
    #[must_use]
    pub fn is_fleet_control(&self) -> bool {
        self.scope == GrantScope::Fleet || self.classes.iter().any(|c| c.is_fleet_control())
    }

    /// The authority that a fleet-control intent carries (ADR-008 §6.1);
    /// `None` for a grant that is not bound yet.
    #[must_use]
    pub fn authority(&self) -> Option<CommandAuthority> {
        self.binding.as_ref().map(|b| CommandAuthority {
            grant_id: self.grant_id.clone(),
            caller_station: self.station_id.clone(),
            caller_session: self.session_id.clone(),
            generation: b.generation,
        })
    }
}

pub(crate) const GRANT_COLUMNS: &str = "grant_id, station_id, session_id, classes, stations, \
    spawn_cap, spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state, revoked_reason, \
    bound_generation, bound_native_id, bound_provider_instance, binding_id, last_used_at, \
    scope, answer_cap, answer_used, desktop_cap, desktop_used, capture_cap, capture_used, \
    image_byte_cap, image_byte_used, desktop_attempts, recording_cap, recording_used, \
    recording_second_cap, recording_second_used";

fn state_of(text: &str) -> GrantState {
    match text {
        "active" => GrantState::Active,
        "revoked" => GrantState::Revoked,
        // Unknown text fails closed.
        _ => GrantState::Expired,
    }
}

pub(crate) fn grant_row(r: &Row<'_>) -> rusqlite::Result<(Grant, String, String, String)> {
    let generation: Option<i64> = r.get(13)?;
    let native: Option<String> = r.get(14)?;
    let instance: Option<String> = r.get(15)?;
    let binding_id: Option<String> = r.get(16)?;
    let grant_id: String = r.get(0)?;
    let binding = match (generation, native, instance, binding_id) {
        (Some(g), Some(native_id), Some(instance), Some(binding_id)) => Some(CoordinatorBinding {
            binding_id,
            grant_id: grant_id.clone(),
            generation: u32::try_from(g).unwrap_or(u32::MAX),
            provider_instance: ProviderInstanceId(instance),
            native_id,
        }),
        _ => None,
    };
    let state: String = r.get(11)?;
    let cap: i64 = r.get(5)?;
    let used: i64 = r.get(6)?;
    let scope: String = r.get(18)?;
    let answer_cap: Option<i64> = r.get(19)?;
    let answer_used: i64 = r.get(20)?;
    let desktop_cap: Option<i64> = r.get(21)?;
    let desktop_used: i64 = r.get(22)?;
    let capture_cap: Option<i64> = r.get(23)?;
    let capture_used: i64 = r.get(24)?;
    let image_byte_cap: Option<i64> = r.get(25)?;
    let image_byte_used: i64 = r.get(26)?;
    let desktop_attempts: i64 = r.get(27)?;
    let recording_cap: i64 = r.get(28)?;
    let recording_used: i64 = r.get(29)?;
    let recording_second_cap: i64 = r.get(30)?;
    let recording_second_used: i64 = r.get(31)?;
    Ok((
        Grant {
            grant_id,
            station_id: StationId(r.get(1)?),
            session_id: SessionId(r.get(2)?),
            classes: BTreeSet::new(),
            stations: BTreeSet::new(),
            spawn_cap: u32::try_from(cap).unwrap_or(0),
            spawn_used: u32::try_from(used).unwrap_or(u32::MAX),
            max_modes: BTreeMap::new(),
            idle_ttl_ms: r.get(8)?,
            created_at: r.get(9)?,
            expires_at: r.get(10)?,
            state: state_of(&state),
            revoked_reason: r.get(12)?,
            binding,
            last_used_at: r.get(17)?,
            // Unknown text is the narrow scope (fail closed).
            scope: if scope == "fleet" {
                GrantScope::Fleet
            } else {
                GrantScope::Lineage
            },
            answer_cap: answer_cap.map(|c| u32::try_from(c).unwrap_or(0)),
            answer_used: u32::try_from(answer_used).unwrap_or(u32::MAX),
            desktop_cap: desktop_cap.map(|c| u32::try_from(c).unwrap_or(0)),
            desktop_used: u32::try_from(desktop_used).unwrap_or(u32::MAX),
            capture_cap: capture_cap.map(|c| u32::try_from(c).unwrap_or(0)),
            capture_used: u32::try_from(capture_used).unwrap_or(u32::MAX),
            image_byte_cap: image_byte_cap.map(|c| u64::try_from(c).unwrap_or(0)),
            image_byte_used: u64::try_from(image_byte_used).unwrap_or(u64::MAX),
            desktop_attempts: u32::try_from(desktop_attempts).unwrap_or(u32::MAX),
            recording_cap: u32::try_from(recording_cap).unwrap_or(0),
            recording_used: u32::try_from(recording_used).unwrap_or(u32::MAX),
            recording_second_cap: u32::try_from(recording_second_cap).unwrap_or(0),
            recording_second_used: u32::try_from(recording_second_used).unwrap_or(u32::MAX),
        },
        r.get(3)?,
        r.get(4)?,
        r.get(7)?,
    ))
}

/// Decodes the JSON columns. Unknown class names fail closed: the grant
/// loads with no classes.
pub(crate) fn finish(raw: (Grant, String, String, String)) -> Result<Grant> {
    let (mut grant, classes, stations, modes) = raw;
    grant.classes = serde_json::from_str(&classes).unwrap_or_default();
    grant.stations = serde_json::from_str(&stations)?;
    grant.max_modes = serde_json::from_str(&modes)?;
    Ok(grant)
}

pub(crate) fn get_grant(conn: &Connection, grant_id: &str) -> Result<Option<Grant>> {
    conn.query_row(
        &format!("SELECT {GRANT_COLUMNS} FROM tokens WHERE grant_id = ?1"),
        [grant_id],
        grant_row,
    )
    .optional()?
    .map(finish)
    .transpose()
}

/// The newest grant of a coordinator session.
pub(crate) fn caller_grant(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
) -> Result<Option<Grant>> {
    conn.query_row(
        &format!(
            "SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1 AND session_id = ?2
             ORDER BY created_at DESC, grant_id DESC LIMIT 1"
        ),
        params![station.as_str(), session.0],
        grant_row,
    )
    .optional()?
    .map(finish)
    .transpose()
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

impl DeckStore {
    /// Stores an owner-authorized grant (ADR-006 §4). The limits are
    /// checked here: cap ≤ 50, idle TTL ≤ 24 h, lifetime ≤ 7 days.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a value out of range, or a database error.
    pub async fn create_grant(&self, mut grant: NewGrant) -> Result<Grant> {
        if grant.spawn_cap > MAX_SPAWN_CAP
            || !(1..=MAX_IDLE_TTL_MS).contains(&grant.idle_ttl_ms)
            || !(1..=MAX_TTL_MS).contains(&grant.ttl_ms)
            || grant.stations.is_empty()
        {
            return Err(DeckError::Invalid(
                "a grant needs a cap of at most 50, an idle TTL of at most 24 h, a lifetime of \
                 at most 7 days and at least one station"
                    .to_owned(),
            ));
        }
        // The answer cap (ADR-008 §3): 0 to 5000, only with the `answer`
        // class, 500 when the class has no value.
        let answers = grant.classes.contains(&GrantClass::Answer);
        match grant.answer_cap {
            Some(_) if !answers => {
                return Err(DeckError::Invalid(
                    "answer_cap needs the answer class".to_owned(),
                ));
            }
            Some(cap) if cap > MAX_ANSWER_CAP => {
                return Err(DeckError::Invalid("answer_cap is 0 to 5000".to_owned()));
            }
            None if answers => grant.answer_cap = Some(DEFAULT_ANSWER_CAP),
            _ => {}
        }
        // The desktop caps (R13 P6 §8.1).
        check_desktop_caps(
            &grant.classes,
            grant.desktop_cap,
            grant.capture_cap,
            grant.image_byte_cap,
        )
        .map_err(|m| DeckError::Invalid(m.to_owned()))?;
        check_recording_caps(
            &grant.classes,
            grant.recording_cap,
            grant.recording_second_cap,
        )
        .map_err(|m| DeckError::Invalid(m.to_owned()))?;
        if grant.classes.contains(&GrantClass::Desktop)
            || grant.classes.contains(&GrantClass::Capture)
        {
            grant.desktop_cap.get_or_insert(limits::DESKTOP_CAP_DEFAULT);
        }
        if grant.classes.contains(&GrantClass::Capture) {
            grant.capture_cap.get_or_insert(limits::CAPTURE_CAP_DEFAULT);
            grant
                .image_byte_cap
                .get_or_insert(limits::IMAGE_BYTE_CAP_DEFAULT);
        }
        let now = self.now_ms();
        let grant_id: String = new_id(self.inner.ids.as_ref());
        let (stored, changed) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(start) = grant.start_command_id.as_deref() {
                let found: Option<String> = tx
                    .query_row(
                        "SELECT grant_id FROM tokens WHERE start_command_id = ?1",
                        [start],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = found {
                    let found = get_grant(&tx, &id)?
                        .ok_or_else(|| DeckError::Corrupt("a found grant is gone".to_owned()))?;
                    return Ok((found, BTreeSet::new()));
                }
            }
            tx.execute(
                "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations,
                     spawn_cap, max_modes, idle_ttl_ms, created_at, expires_at, state,
                     start_command_id, scope, answer_cap, desktop_cap, capture_cap,
                     image_byte_cap, recording_cap, recording_second_cap)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18)",
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
                    grant.recording_second_cap
                ],
            )?;
            let stored = get_grant(&tx, &grant_id)?
                .ok_or_else(|| DeckError::Corrupt("an inserted grant is gone".to_owned()))?;
            // The owner's grant lets each station that it names relay
            // coordinator calls: the derived scope `coordinate` follows in
            // the same transaction (DL-211, design §2.3 item 1).
            let changed = sync_coordinate_scopes(&tx, now)?;
            tx.commit()?;
            Ok((stored, changed))
        })
        .await?;
        self.note_scope_changes(changed);
        Ok(stored)
    }

    /// One grant.
    ///
    /// # Errors
    /// A database error.
    pub async fn grant(&self, grant_id: String) -> Result<Option<Grant>> {
        call(&self.inner.db, move |conn| get_grant(conn, &grant_id)).await
    }

    /// Applies the station's binding facts (`coordinator.bound`, §6.1) to
    /// an active grant. A generation lower than or equal to the stored one
    /// is refused (it never becomes valid again), except the same binding.
    /// Returns `false` when nothing changed.
    ///
    /// # Errors
    /// A database error.
    pub async fn bind_grant(&self, binding: CoordinatorBinding) -> Result<bool> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let Some(grant) = get_grant(conn, &binding.grant_id)? else {
                return Ok(false);
            };
            if grant.state != GrantState::Active {
                return Ok(false);
            }
            if let Some(old) = &grant.binding
                && (old.generation >= binding.generation
                    || renew_generation(conn, &binding.grant_id)?
                        != Some(i64::from(binding.generation)))
            {
                return Ok(false);
            }
            let n = conn.execute(
                "UPDATE tokens SET bound_generation = ?2, bound_native_id = ?3,
                     bound_provider_instance = ?4, binding_id = ?5, last_used_at = ?6
                 WHERE grant_id = ?1 AND state = 'active'",
                params![
                    binding.grant_id,
                    binding.generation,
                    binding.native_id,
                    binding.provider_instance.0,
                    binding.binding_id,
                    now
                ],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// The owner renews an idle-expired grant (ADR-006 §5.5, test 25b).
    /// Only a grant that is `expired` for `idle`, still inside its
    /// lifetime and bound once gets a pending renewal generation (the bound
    /// generation + 1). The grant stays expired until the station
    /// acknowledges that generation with `coordinator.bound`. Returns the
    /// pending generation, or `None` when the grant cannot be renewed. A
    /// repeated renewal returns the same generation.
    ///
    /// # Errors
    /// A database error.
    pub async fn renew_grant(&self, grant_id: String) -> Result<Option<u32>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let Some(grant) = get_grant(conn, &grant_id)? else {
                return Ok(None);
            };
            let idle = grant.state == GrantState::Expired
                && grant.revoked_reason.as_deref() == Some("idle");
            let Some(binding) = grant.binding.filter(|_| idle && grant.expires_at > now) else {
                return Ok(None);
            };
            let next = binding.generation.saturating_add(1);
            conn.execute(
                "UPDATE tokens SET renew_generation = ?2
                 WHERE grant_id = ?1 AND state = 'expired'",
                params![grant_id, next],
            )?;
            Ok(Some(next))
        })
        .await
    }

    /// The newest grants, at most `limit`.
    ///
    /// # Errors
    /// A database error.
    pub async fn grants(&self, limit: u32) -> Result<Vec<Grant>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {GRANT_COLUMNS} FROM tokens ORDER BY created_at DESC, grant_id DESC LIMIT ?1"
            ))?;
            let rows = stmt
                .query_map([limit], grant_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(finish).collect()
        })
        .await
    }

    /// The owner revokes a grant. From this transaction on, every decision
    /// for it is `forbidden` (ADR-006 §6.7).
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_grant(&self, grant_id: String, reason: String) -> Result<bool> {
        let now = self.now_ms();
        let (revoked, changed) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute(
                "UPDATE tokens SET state = 'revoked', revoked_reason = ?2
                 WHERE grant_id = ?1 AND state != 'revoked'",
                params![grant_id, reason],
            )?;
            invalidate_coordinator_directives(&tx, &grant_id)?;
            let changed = sync_coordinate_scopes(&tx, now)?;
            tx.commit()?;
            Ok((n == 1, changed))
        })
        .await?;
        self.note_scope_changes(changed);
        Ok(revoked)
    }

    /// Records that `session` on `station` was started by `grant_id` (the
    /// lineage of `interrupt` and `stop`, §6.3).
    ///
    /// # Errors
    /// A database error.
    pub async fn add_lineage(
        &self,
        grant_id: String,
        station: StationId,
        session: SessionId,
        command_id: String,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO coordinator_lineage
                     (command_id, grant_id, station_id, session_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (command_id) DO UPDATE SET session_id = excluded.session_id",
                params![command_id, grant_id, station.as_str(), session.0, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Deletes coordinator receipts older than `before` (Unix ms; the 24 h
    /// retention of ADR-004 §9.3). Grants and their `spawn_used` stay.
    ///
    /// # Errors
    /// A database error.
    pub async fn prune_coordinator_receipts(&self, before: i64) -> Result<usize> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute("DELETE FROM coordinator_receipts WHERE at < ?1", [before])?)
        })
        .await
    }

    /// The number of start intents that grants wrote (lineage rows).
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_starts(&self) -> Result<u64> {
        call(&self.inner.db, |conn| {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM coordinator_lineage WHERE start_outbox_id IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            super::to_u64(n)
        })
        .await
    }
}

#[cfg(test)]
#[path = "grants_desktop_tests.rs"]
mod desktop_tests;
