//! The station side of the dispatch authority of `desktop.act` (R13 §5E item
//! 1, §5F items 1 and 2, §5G item 1, §5E item 5; feature
//! `desktop_authority.v1`).
//!
//! Two steps:
//! - [`admit`] runs at receipt. It checks the authority (the epoch of this
//!   connection, the clock skew, the hash of the action, the quiet-hours
//!   string and that the connection is current) and consumes the
//!   `action_id` in one atomic step. An id is accepted once for the whole
//!   connection epoch, at most [`MAX_USED_IDS`] ids for each epoch.
//! - [`Start::begin`] runs right before the first program of the action. It
//!   checks the connection again, the hash of the action that will run, the
//!   quiet hours (it reads `date +%z` again), the deadline on the wall clock
//!   (exact, no grace) and the 10 s monotonic limit since receipt. The
//!   monotonic limit does not depend on the wall clock, so a clock change
//!   cannot extend an authority.
//!
//! "Execution starts" is the spawn of the first program of the action. Every
//! wait before it (the per-display lock, the info reads) counts against the
//! deadline. After `desktop_control` turns off, an action that was
//! authorized can still start within the link delivery time plus at most
//! 10 s; this interval is accepted and stated (§5F item 1).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Fingerprint;
use fleetdeck_link::control::Control;
use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_link::methods::code as link_code;
use fleetdeck_proto::coordinator_desktop_wire::{AuthorityPhase, CoordinatorContext};
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::desktop_authority::{
    AUTHORITY_REFUSED, CLOCK_SKEW_MS, DesktopAuthority, MAX_USED_IDS,
};

use super::coordinator::{DispatchCheck, Extras, Gates};
use super::quiet::{self, QuietHours};
use super::{Fail, code};

/// The longest time between receipt and the first side effect (§5F item 1).
pub const START_LIMIT: Duration = Duration::from_secs(10);
/// The refusal for clocks that differ too much.
pub const SKEW_REFUSED: &str =
    "The station and the deck clocks differ too much for a desktop action.";
/// The failure for an authority that ran out.
pub const EXPIRED: &str = "The authorization of this desktop action expired.";
/// The failure while quiet hours are active (or cannot be checked).
pub const QUIET_ACTIVE: &str = "Quiet hours are active on this station.";
/// The failure when the id set of the connection epoch is full.
pub const TOO_MANY: &str = "Too many desktop actions on this link; it reconnects.";

fn refused() -> Fail {
    Fail::new(code::FORBIDDEN, AUTHORITY_REFUSED)
}

type Check = Arc<dyn Fn() -> bool + Send + Sync>;

/// What the check needs to know of one connection.
#[derive(Clone)]
pub(super) struct Conn {
    /// The `Hello.desktop_epoch` that this station sent on the connection.
    epoch: Option<String>,
    /// Whether the connection is still the current one of its peer.
    current: Check,
    /// Whether the connection has not ended.
    alive: Check,
    /// Closes the connection with a protocol error.
    close: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Conn {
    pub(super) fn new(
        epoch: Option<String>,
        current: Check,
        alive: Check,
        close: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        Self {
            epoch,
            current,
            alive,
            close,
        }
    }

    fn of(control: &Control) -> Self {
        let current = control.clone();
        let alive = control.clone();
        let close = control.clone();
        Self::new(
            control.params().local_desktop_epoch.clone(),
            Arc::new(move || current.is_current()),
            Arc::new(move || alive.ended().is_none()),
            Arc::new(move |detail| close.close(link_code::PROTOCOL_ERROR, detail)),
        )
    }
}

/// The two clocks of the check: the wall clock in Unix ms and a monotonic
/// instant.
#[derive(Clone)]
pub(super) struct Time {
    wall: Arc<dyn Fn() -> i64 + Send + Sync>,
    mono: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl Time {
    pub(super) fn new(
        wall: Arc<dyn Fn() -> i64 + Send + Sync>,
        mono: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self { wall, mono }
    }

    fn system() -> Self {
        Self::new(Arc::new(quiet::now_ms), Arc::new(Instant::now))
    }
}

/// The result of consuming one id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Consume {
    New,
    Repeat,
    Full,
}

struct Entry {
    alive: Check,
    ids: HashSet<String>,
}

/// The consumed `action_id`s of every live connection epoch.
pub(super) struct Registry {
    max: usize,
    epochs: HashMap<String, Entry>,
}

impl Registry {
    pub(super) fn new(max: usize) -> Self {
        Self {
            max,
            epochs: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn epoch_count(&self) -> usize {
        self.epochs.len()
    }

    /// Consumes `id` for `epoch`: one lock, insert-if-absent. The epochs of
    /// connections that ended are dropped first.
    pub(super) fn consume(&mut self, epoch: &str, id: &str, alive: &Check) -> Consume {
        self.epochs.retain(|_, entry| (entry.alive)());
        let entry = self
            .epochs
            .entry(epoch.to_owned())
            .or_insert_with(|| Entry {
                alive: Arc::clone(alive),
                ids: HashSet::new(),
            });
        if entry.ids.contains(id) {
            Consume::Repeat
        } else if entry.ids.len() >= self.max {
            Consume::Full
        } else {
            entry.ids.insert(id.to_owned());
            Consume::New
        }
    }
}

static REGISTRY: LazyLock<Mutex<Registry>> =
    LazyLock::new(|| Mutex::new(Registry::new(MAX_USED_IDS)));

#[derive(Clone)]
struct Checked {
    conn: Conn,
    time: Time,
    /// The monotonic instant of receipt.
    received: Instant,
    deadline_ms: i64,
    hash: String,
    station_node: String,
    quiet: Option<QuietHours>,
    /// The coordinator that the action runs for (R13 P6 §5.4); the hash
    /// covers it.
    coordinator: Option<CoordinatorContext>,
    /// The remote check right before the effect (§6.3 item 4).
    remote: Option<Arc<dyn DispatchCheck>>,
    /// The live local gates of the action, read again after the remote check.
    gates: Option<Gates>,
    /// What the target needs of the authenticated link.
    extras: Option<Extras>,
}

enum Kind {
    Checked(Box<Checked>),
    /// A test value that every check accepts.
    #[cfg(test)]
    Open,
}

/// An action that the deck authorized. It holds what the start check needs.
#[derive(Clone)]
pub struct Admitted(Arc<Kind>);

impl std::fmt::Debug for Admitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admitted").finish_non_exhaustive()
    }
}

impl Admitted {
    /// An authorization that every check accepts (tests of the actions).
    #[cfg(test)]
    pub(crate) fn open() -> Self {
        Self(Arc::new(Kind::Open))
    }

    /// The same authorization with the checks of a coordinator action (R13
    /// P6 §6.3): the remote dispatch check, the live local gates and the
    /// link extras. Intake calls it right after [`admit`] for an authority
    /// that carries a coordinator. A test value stays open.
    #[must_use]
    pub fn coordinated(self, remote: Arc<dyn DispatchCheck>, gates: Gates, extras: Extras) -> Self {
        match &*self.0 {
            Kind::Checked(c) => {
                let mut c = (**c).clone();
                c.remote = Some(remote);
                c.gates = Some(gates);
                c.extras = Some(extras);
                Self(Arc::new(Kind::Checked(Box::new(c))))
            }
            #[cfg(test)]
            Kind::Open => self,
        }
    }

    /// The coordinator context of the authority, if it has one.
    #[must_use]
    pub fn coordinator(&self) -> Option<&CoordinatorContext> {
        match &*self.0 {
            Kind::Checked(c) => c.coordinator.as_ref(),
            #[cfg(test)]
            Kind::Open => None,
        }
    }

    /// The live local gates of a coordinator action, read now. `Ok` for an
    /// action without gates.
    ///
    /// # Errors
    /// The failure of the first gate that is off.
    pub fn local_gates(&self) -> Result<(), Fail> {
        match &*self.0 {
            Kind::Checked(c) => c.gates.as_ref().map_or(Ok(()), |gates| gates()),
            #[cfg(test)]
            Kind::Open => Ok(()),
        }
    }

    fn extras(&self) -> Option<&Extras> {
        match &*self.0 {
            Kind::Checked(c) => c.extras.as_ref(),
            #[cfg(test)]
            Kind::Open => None,
        }
    }

    /// A coordinator authorization with the hash of `action` that accepts
    /// every connection check, with the given remote check, gates and link
    /// extras (tests of the coordinator actions outside this module).
    #[cfg(test)]
    pub(crate) fn open_coordinated(
        action: &DesktopAction,
        node: &str,
        coordinator: CoordinatorContext,
        remote: Arc<dyn DispatchCheck>,
        gates: Gates,
        extras: Extras,
    ) -> Self {
        let hash = action_hash_for(action, node, Some(&coordinator));
        Self(Arc::new(Kind::Checked(Box::new(Checked {
            conn: Conn::new(None, Arc::new(|| true), Arc::new(|| true), Arc::new(|_| {})),
            time: Time::system(),
            received: Instant::now(),
            deadline_ms: i64::MAX,
            hash,
            station_node: node.to_owned(),
            quiet: None,
            coordinator: Some(coordinator),
            remote: Some(remote),
            gates: Some(gates),
            extras: Some(extras),
        }))))
    }
}

/// The 26 ASCII alphanumeric characters of an `action_id` (ULID form).
fn id_shape_ok(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Checks the authority of `action` that arrived on `control`, and consumes
/// its `action_id`.
///
/// # Errors
/// `forbidden` with [`AUTHORITY_REFUSED`] for a missing or invalid authority,
/// `forbidden` with [`SKEW_REFUSED`], `invalid_params` for a bad
/// `quiet_hours`, and `failed` with [`TOO_MANY`] when the id set is full (the
/// connection closes).
pub fn admit(
    control: &Control,
    station_node: &Fingerprint,
    authority: Option<DesktopAuthority>,
    action: &DesktopAction,
) -> Result<Admitted, Fail> {
    admit_with(
        &Conn::of(control),
        &Time::system(),
        &REGISTRY,
        &station_node.to_string(),
        authority,
        action,
    )
}

pub(super) fn admit_with(
    conn: &Conn,
    time: &Time,
    registry: &Mutex<Registry>,
    station_node: &str,
    authority: Option<DesktopAuthority>,
    action: &DesktopAction,
) -> Result<Admitted, Fail> {
    let authority = authority.ok_or_else(refused)?;
    if !id_shape_ok(&authority.action_id) {
        return Err(refused());
    }
    match &conn.epoch {
        Some(epoch) if *epoch == authority.desktop_epoch => {}
        _ => return Err(refused()),
    }
    let received = (time.mono)();
    let now = (time.wall)();
    if now.abs_diff(authority.deck_now_ms) > CLOCK_SKEW_MS.unsigned_abs() {
        return Err(Fail::new(code::FORBIDDEN, SKEW_REFUSED));
    }
    let hash = action_hash_for(action, station_node, authority.coordinator.as_ref());
    if hash != authority.action_hash {
        return Err(refused());
    }
    let quiet = match authority.quiet_hours.as_deref() {
        None => None,
        Some(text) => Some(
            QuietHours::parse(text)
                .ok_or_else(|| Fail::new(code::INVALID_PARAMS, "`quiet_hours` is not valid"))?,
        ),
    };
    // The connection check comes right before the consume (§5G item 1).
    if !(conn.current)() {
        return Err(refused());
    }
    let epoch = authority.desktop_epoch.as_str();
    let outcome = registry
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .consume(epoch, &authority.action_id, &conn.alive);
    match outcome {
        Consume::New => {}
        Consume::Repeat => return Err(refused()),
        Consume::Full => {
            (conn.close)("too many desktop actions on this link");
            return Err(Fail::new(code::FAILED, TOO_MANY));
        }
    }
    Ok(Admitted(Arc::new(Kind::Checked(Box::new(Checked {
        conn: conn.clone(),
        time: time.clone(),
        received,
        deadline_ms: authority.deadline_ms,
        hash: authority.action_hash,
        station_node: station_node.to_owned(),
        quiet,
        coordinator: authority.coordinator,
        remote: None,
        gates: None,
        extras: None,
    })))))
}

/// The start check of one action. [`Start::begin`] runs before every program
/// and every file change of the action; only the first call checks.
pub struct Start {
    admitted: Admitted,
    /// The hash of the action that will run.
    ran_hash: String,
    begun: AtomicBool,
}

impl Start {
    /// For the action that is about to run.
    #[must_use]
    pub fn new(admitted: Admitted, action: &DesktopAction) -> Self {
        let ran_hash = match &*admitted.0 {
            Kind::Checked(c) => action_hash_for(action, &c.station_node, c.coordinator.as_ref()),
            #[cfg(test)]
            Kind::Open => String::new(),
        };
        Self {
            admitted,
            ran_hash,
            begun: AtomicBool::new(false),
        }
    }

    /// The check right before the first side effect.
    ///
    /// # Errors
    /// `forbidden` (connection, hash), `failed` (expired, quiet hours).
    pub async fn begin(&self, env: &ChildEnv) -> Result<(), Fail> {
        if self.begun.load(Ordering::Acquire) {
            return Ok(());
        }
        match &*self.admitted.0 {
            Kind::Checked(c) => self.check(c, env, AuthorityPhase::Execute).await?,
            #[cfg(test)]
            Kind::Open => {}
        }
        self.begun.store(true, Ordering::Release);
        Ok(())
    }

    /// The same sequence as [`Start::begin`] for a later phase of a
    /// coordinator action (the screenshot `Import`, R13 P6 §6.3): it checks
    /// every time, with no `begun` short cut.
    ///
    /// # Errors
    /// As [`Start::begin`], and `authority_lost` or `authority_unknown`.
    pub async fn check_phase(&self, phase: AuthorityPhase, env: &ChildEnv) -> Result<(), Fail> {
        match &*self.admitted.0 {
            Kind::Checked(c) => self.check(c, env, phase).await,
            #[cfg(test)]
            Kind::Open => Ok(()),
        }
    }

    /// The coordinator context of the action, if it has one.
    #[must_use]
    pub fn coordinator(&self) -> Option<&CoordinatorContext> {
        self.admitted.coordinator()
    }

    /// The deck fingerprint and the tailnet name of a coordinator action.
    #[must_use]
    pub fn link_extras(&self) -> Option<&Extras> {
        self.admitted.extras()
    }

    async fn check(&self, c: &Checked, env: &ChildEnv, phase: AuthorityPhase) -> Result<(), Fail> {
        if self.ran_hash != c.hash {
            return Err(refused());
        }
        if let Some(quiet) = c.quiet {
            // The offset is read again; an unknown offset fails closed.
            // The runner of `date` has no start check; the box ends the type
            // recursion (runner, start, offset, runner).
            let offset = Box::pin(quiet::read_offset(env)).await;
            let inside = offset.is_none_or(|o| quiet.contains_at((c.time.wall)(), o));
            if inside {
                return Err(Fail::new(code::FAILED, QUIET_ACTIVE));
            }
        }
        // A coordinator action: the remote check, then the live local gates
        // read after that await. No other await follows before the spawn.
        if let Some(remote) = &c.remote {
            remote.check(phase, None).await?;
        }
        if let Some(gates) = &c.gates {
            gates()?;
        }
        // The last checks come right before the spawn, in one step.
        if !(c.conn.current)() {
            return Err(refused());
        }
        if (c.time.wall)() > c.deadline_ms
            || (c.time.mono)().saturating_duration_since(c.received) > START_LIMIT
        {
            return Err(Fail::new(code::FAILED, EXPIRED));
        }
        Ok(())
    }
}
