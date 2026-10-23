//! The relay of wall controls (B17 §3.1.3): memory only. The hub keeps, for
//! each display, a sequence number, the checkpoint of the current directive
//! generation, the last kept frames, the hold and the count of open display
//! sockets. A deck restart forgets it, and the new hub has a new
//! `incarnation`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::wall_control::{
    CONTROL_KEEP_MS, MAX_CONTROLS_KEPT, MAX_CONTROLS_PER_SECOND, REMOTE_HOLD_MS, WallControl,
    WallControlFrame, WallEvent, WallLink, WallRemoteState,
};
use tokio::sync::broadcast;

use crate::store::Result;
use crate::store::auth::random_token;

/// The capacity of the broadcast channel of one display.
const CHANNEL_CAPACITY: usize = 64;
/// The window of the rate limit.
const RATE_WINDOW: Duration = Duration::from_secs(1);
/// The time without a control after which the next one writes an audit row.
const AUDIT_QUIET: Duration = Duration::from_secs(10 * 60);

/// What a push relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushEvent {
    Navigate,
    Control(WallControl),
}

/// Why a push failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    /// More than [`MAX_CONTROLS_PER_SECOND`] in one second.
    TooMany,
}

/// The parts of a remote view that the hub knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteParts {
    pub incarnation: String,
    pub seq: u64,
    pub state: WallRemoteState,
    pub hold_left_ms: u64,
    pub complete: bool,
    pub controls: Vec<WallControlFrame>,
}

struct Slot {
    /// The last `seq` of this display.
    seq: u64,
    /// The generation of `state`.
    generation: u64,
    /// The checkpoint; no expiry.
    state: WallRemoteState,
    /// The last control or navigation, plus `REMOTE_HOLD_MS`.
    hold_until: Option<Instant>,
    kept: VecDeque<(Instant, WallControlFrame)>,
    tx: broadcast::Sender<WallControlFrame>,
    /// Open display sockets with the feature.
    ready: usize,
    /// Open display sockets without it.
    older: usize,
    /// The rate window.
    recent: VecDeque<Instant>,
    /// The last accepted control or navigation that asked [`WallControlHub::audit_due`].
    last_accept: Option<Instant>,
}

impl Slot {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            seq: 0,
            generation: 0,
            state: WallRemoteState::default(),
            hold_until: None,
            kept: VecDeque::new(),
            tx,
            ready: 0,
            older: 0,
            recent: VecDeque::new(),
            last_accept: None,
        }
    }

    /// The checkpoint of `generation`: the default for another generation.
    fn state_of(&self, generation: u64) -> WallRemoteState {
        if self.generation == generation {
            self.state.clone()
        } else {
            WallRemoteState::default()
        }
    }

    fn drop_old(&mut self, now: Instant) {
        let keep = Duration::from_millis(CONTROL_KEEP_MS);
        while self
            .kept
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > keep)
        {
            self.kept.pop_front();
        }
    }

    fn drop_old_rates(&mut self, now: Instant) {
        while self
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_WINDOW)
        {
            self.recent.pop_front();
        }
    }
}

type Slots = Arc<Mutex<HashMap<String, Slot>>>;

fn lock(slots: &Mutex<HashMap<String, Slot>>) -> MutexGuard<'_, HashMap<String, Slot>> {
    slots.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The relay of the wall controls of a deck.
#[derive(Debug)]
pub struct WallControlHub {
    incarnation: String,
    slots: Slots,
    /// Added to the clock; only a test moves it ([`Self::advance_clock`]).
    skew: Mutex<Duration>,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot").field("seq", &self.seq).finish()
    }
}

/// Keeps a display socket in the count of the hub until it drops.
#[derive(Debug)]
pub struct SocketGuard {
    slots: Slots,
    display_id: String,
    ready: bool,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Some(slot) = lock(&self.slots).get_mut(&self.display_id) {
            if self.ready {
                slot.ready = slot.ready.saturating_sub(1);
            } else {
                slot.older = slot.older.saturating_sub(1);
            }
        }
    }
}

impl WallControlHub {
    /// A hub with a new incarnation.
    ///
    /// # Errors
    /// The random source failed.
    pub fn new() -> Result<Self> {
        Ok(Self {
            incarnation: random_token()?,
            slots: Arc::default(),
            skew: Mutex::new(Duration::ZERO),
        })
    }

    /// The id of this relay.
    #[must_use]
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }

    /// The time that the routes pass to the hub.
    #[must_use]
    pub fn now(&self) -> Instant {
        Instant::now() + *self.skew.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves the clock of [`Self::now`] forward. For tests: the controls
    /// expire by time.
    pub fn advance_clock(&self, by: Duration) {
        *self.skew.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }

    fn frame(&self, slot: &Slot, generation: u64, event: WallEvent) -> WallControlFrame {
        WallControlFrame {
            incarnation: self.incarnation.clone(),
            seq: slot.seq,
            generation,
            event,
            state: slot.state_of(generation),
        }
    }

    /// A display socket that declared `wall_control.v1`: a receiver, the
    /// `sync` frame for `generation` (the current effective generation), and
    /// a guard that decrements `ready` on drop.
    pub fn attach_ready(
        &self,
        display_id: &str,
        generation: u64,
    ) -> (
        broadcast::Receiver<WallControlFrame>,
        WallControlFrame,
        SocketGuard,
    ) {
        let mut slots = lock(&self.slots);
        let slot = slots.entry(display_id.to_owned()).or_insert_with(Slot::new);
        slot.ready += 1;
        // Subscribe and read the state under one lock: no frame is lost
        // between the `sync` frame and the receiver.
        let rx = slot.tx.subscribe();
        let sync = self.frame(slot, generation, WallEvent::Sync);
        let guard = SocketGuard {
            slots: Arc::clone(&self.slots),
            display_id: display_id.to_owned(),
            ready: true,
        };
        (rx, sync, guard)
    }

    /// A display socket without the feature: a guard that decrements
    /// `older` on drop.
    pub fn attach_older(&self, display_id: &str) -> SocketGuard {
        let mut slots = lock(&self.slots);
        slots
            .entry(display_id.to_owned())
            .or_insert_with(Slot::new)
            .older += 1;
        SocketGuard {
            slots: Arc::clone(&self.slots),
            display_id: display_id.to_owned(),
            ready: false,
        }
    }

    /// The link of the wall page of a display now.
    #[must_use]
    pub fn link(&self, display_id: &str) -> WallLink {
        match lock(&self.slots).get(display_id) {
            Some(slot) if slot.ready > 0 => WallLink::Ready,
            Some(slot) if slot.older > 0 => WallLink::Older,
            _ => WallLink::Offline,
        }
    }

    /// The `sync` frame for `generation` (also after a lag).
    #[must_use]
    pub fn sync(&self, display_id: &str, generation: u64) -> WallControlFrame {
        match lock(&self.slots).get(display_id) {
            Some(slot) => self.frame(slot, generation, WallEvent::Sync),
            None => self.frame(&Slot::new(), generation, WallEvent::Sync),
        }
    }

    /// True when one more control or navigation fits in the rate window now.
    /// It records nothing.
    #[must_use]
    pub fn rate_ok(&self, display_id: &str, now: Instant) -> bool {
        match lock(&self.slots).get_mut(display_id) {
            Some(slot) => {
                slot.drop_old_rates(now);
                slot.recent.len() < MAX_CONTROLS_PER_SECOND
            }
            None => true,
        }
    }

    /// Rate check; when `generation` differs from the generation of the
    /// slot, the state resets to the default; `seq` + 1; the new state
    /// (`after` for a control, unchanged for a navigation); the hold; keep;
    /// broadcast. All of it under the slot lock, so the broadcast order is
    /// the `seq` order.
    ///
    /// # Errors
    /// [`PushError::TooMany`] above the rate.
    pub fn push(
        &self,
        display_id: &str,
        generation: u64,
        event: PushEvent,
        now: Instant,
    ) -> std::result::Result<WallControlFrame, PushError> {
        let mut slots = lock(&self.slots);
        let slot = slots.entry(display_id.to_owned()).or_insert_with(Slot::new);
        slot.drop_old_rates(now);
        if slot.recent.len() >= MAX_CONTROLS_PER_SECOND {
            return Err(PushError::TooMany);
        }
        slot.recent.push_back(now);
        if slot.generation != generation {
            slot.generation = generation;
            slot.state = WallRemoteState::default();
        }
        let event = match event {
            PushEvent::Navigate => WallEvent::Navigate,
            PushEvent::Control(control) => {
                slot.state = slot.state.after(&control);
                WallEvent::Control { control }
            }
        };
        slot.seq += 1;
        slot.hold_until = Some(now + Duration::from_millis(REMOTE_HOLD_MS));
        let frame = self.frame(slot, generation, event);
        slot.drop_old(now);
        slot.kept.push_back((now, frame.clone()));
        while slot.kept.len() > MAX_CONTROLS_KEPT {
            slot.kept.pop_front();
        }
        // No receiver is not an error: the frame stays kept.
        let _ = slot.tx.send(frame.clone());
        Ok(frame)
    }

    /// The remote view parts for `generation`: incarnation, `seq`, the state
    /// (the default when the slot holds another generation), the hold left,
    /// `complete`, and the kept frames above `after`.
    #[must_use]
    pub fn view(&self, display_id: &str, generation: u64, after: u64, now: Instant) -> RemoteParts {
        let mut slots = lock(&self.slots);
        let Some(slot) = slots.get_mut(display_id) else {
            return RemoteParts {
                incarnation: self.incarnation.clone(),
                seq: 0,
                state: WallRemoteState::default(),
                hold_left_ms: 0,
                complete: after == 0,
                controls: Vec::new(),
            };
        };
        slot.drop_old(now);
        let complete = match after.cmp(&slot.seq) {
            std::cmp::Ordering::Equal => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Less => slot
                .kept
                .front()
                .is_some_and(|(_, f)| f.seq <= after.saturating_add(1)),
        };
        let hold_left_ms = slot.hold_until.map_or(0, |until| {
            u64::try_from(until.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)
        });
        RemoteParts {
            incarnation: self.incarnation.clone(),
            seq: slot.seq,
            state: slot.state_of(generation),
            hold_left_ms,
            complete,
            controls: slot
                .kept
                .iter()
                .filter(|(_, f)| f.seq > after)
                .map(|(_, f)| f.clone())
                .collect(),
        }
    }

    /// True for the first accepted control or navigation of a display after
    /// ten minutes without one (the start of a remote session). Each call
    /// records the time.
    pub fn audit_due(&self, display_id: &str, now: Instant) -> bool {
        let mut slots = lock(&self.slots);
        let slot = slots.entry(display_id.to_owned()).or_insert_with(Slot::new);
        let due = slot
            .last_accept
            .is_none_or(|at| now.saturating_duration_since(at) >= AUDIT_QUIET);
        slot.last_accept = Some(now);
        due
    }

    /// Revoke: drops the slot; the receivers end.
    pub fn forget(&self, display_id: &str) {
        lock(&self.slots).remove(display_id);
    }
}

#[cfg(test)]
#[path = "wall_control_tests.rs"]
mod tests;
