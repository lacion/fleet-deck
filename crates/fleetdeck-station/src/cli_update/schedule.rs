//! The schedule (design §3.9): the external-change check on every tick, and
//! the night window when `provider_updates.enabled`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::config::{ProviderUpdatesConfig, UpdateZone};
use fleetdeck_core::new_id;
use fleetdeck_proto::cli_update::{UpdateOutcome, UpdateTrigger, reason};
use fleetdeck_proto::event::ProviderKind;
use tokio::sync::watch;

use super::run::{Engine, Flight, channel_text, end_of, new_attempt};

/// The first tick after the start.
pub const FIRST_TICK: Duration = Duration::from_secs(2 * 60);
/// The time between two ticks.
pub const EVERY: Duration = Duration::from_secs(15 * 60);
/// The limit of `date +%z`.
const DATE_LIMIT: Duration = Duration::from_secs(10);

const MINUTE_MS: i64 = 60_000;
const DAY_MIN: i64 = 24 * 60;

/// The clock of one tick.
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub utc_ms: i64,
    /// Minutes east of UTC; `None` when `local` could not be read (the
    /// window part of the tick is skipped).
    pub offset_min: Option<i32>,
}

/// An open window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The day number (local) of the window start.
    pub id: i64,
    pub start_utc_ms: i64,
    pub elapsed_min: i64,
}

/// The window that is open at `utc_ms`, in the fixed `offset_min`. A window
/// may cross midnight.
#[must_use]
pub fn window_at(cfg: &ProviderUpdatesConfig, offset_min: i32, utc_ms: i64) -> Option<Window> {
    let local = utc_ms + i64::from(offset_min) * MINUTE_MS;
    let day = local.div_euclid(DAY_MIN * MINUTE_MS);
    let minute = local.rem_euclid(DAY_MIN * MINUTE_MS) / MINUTE_MS;
    let start = i64::from(cfg.window_start_min);
    let len = i64::from(cfg.window_hours) * 60;
    let (start_day, elapsed) = if minute >= start && minute < start + len {
        (day, minute - start)
    } else if start + len > DAY_MIN && minute + DAY_MIN < start + len {
        (day - 1, minute + DAY_MIN - start)
    } else {
        return None;
    };
    Some(Window {
        id: start_day,
        start_utc_ms: (start_day * DAY_MIN + start) * MINUTE_MS - i64::from(offset_min) * MINUTE_MS,
        elapsed_min: elapsed,
    })
}

/// The minutes after the window start before this station tries:
/// `byte0(SHA-256(name)) % min(60, window_minutes / 2)`.
#[must_use]
pub fn jitter_min(name: &str, cfg: &ProviderUpdatesConfig) -> i64 {
    let span = (i64::from(cfg.window_hours) * 60 / 2).clamp(1, 60);
    let digest = ring::digest::digest(&ring::digest::SHA256, name.as_bytes());
    i64::from(digest.as_ref()[0]) % span
}

/// The output of `date +%z` (`+0200`) as minutes east of UTC.
#[must_use]
pub fn parse_offset(text: &str) -> Option<i32> {
    let t = text.trim();
    let sign = match t.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = &t[1..];
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    (hours <= 14 && minutes < 60).then_some(sign * (hours * 60 + minutes))
}

async fn local_offset() -> Option<i32> {
    let mut command = tokio::process::Command::new("date");
    command.arg("+%z").kill_on_drop(true);
    let out = tokio::time::timeout(DATE_LIMIT, command.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| parse_offset(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

#[derive(Default)]
struct State {
    /// The window of the last scheduled attempt of a provider.
    ran: HashMap<ProviderKind, i64>,
    /// The window in which a provider stayed busy at every try.
    busy: HashMap<ProviderKind, i64>,
    /// The last version that the external check handled.
    seen: HashMap<ProviderKind, String>,
}

/// The schedule of one station.
pub struct Schedule {
    engine: Arc<Engine>,
    name: String,
    state: Mutex<State>,
}

impl Schedule {
    #[must_use]
    pub fn new(engine: Arc<Engine>, name: String) -> Self {
        Self {
            engine,
            name,
            state: Mutex::new(State::default()),
        }
    }

    /// The task: the first tick after [`FIRST_TICK`], then every [`EVERY`].
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut wait = FIRST_TICK;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = shutdown.changed() => return,
            }
            let now = self.now().await;
            self.tick(now).await;
            wait = EVERY;
        }
    }

    async fn now(&self) -> Now {
        let offset_min = match self.engine.config.current().timezone {
            UpdateZone::Fixed(m) => Some(m),
            UpdateZone::Local => local_offset().await,
        };
        Now {
            utc_ms: self.engine.store.now_ms(),
            offset_min,
        }
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> Option<T> {
        self.state.lock().ok().map(|mut s| f(&mut s))
    }

    /// One tick.
    pub async fn tick(&self, now: Now) {
        self.external().await;
        let cfg = self.engine.config.current();
        let window = if self.engine.enabled {
            now.offset_min.and_then(|o| window_at(&cfg, o, now.utc_ms))
        } else {
            None
        };
        self.close_busy(window.map(|w| w.id)).await;
        if let Some(w) = window {
            self.in_window(&cfg, w).await;
        }
    }

    /// A provider that was busy in a window that ended gets one
    /// `SkippedBusy` row.
    async fn close_busy(&self, current: Option<i64>) {
        let closed: Vec<ProviderKind> = self
            .with_state(|s| {
                let done: Vec<_> = s
                    .busy
                    .iter()
                    .filter(|(_, w)| Some(**w) != current)
                    .map(|(k, _)| *k)
                    .collect();
                for k in &done {
                    s.busy.remove(k);
                }
                done
            })
            .unwrap_or_default();
        let cfg = self.engine.config.current();
        for kind in closed {
            let Ok(install) = self.engine.install_of(kind) else {
                continue;
            };
            let id = new_id::<String>(self.engine.ids.as_ref());
            let a = new_attempt(
                &id,
                kind,
                UpdateTrigger::Schedule,
                install.method,
                &channel_text(kind, &cfg),
                None,
                None,
            );
            let end = end_of(
                UpdateOutcome::SkippedBusy,
                Some(reason::SESSIONS_RUNNING),
                None,
            );
            if let Err(e) = self.engine.store.cli_update_insert(a, Some(end)).await {
                tracing::warn!(error = %e, "the busy window row is not written");
            }
        }
    }

    async fn in_window(&self, cfg: &ProviderUpdatesConfig, w: Window) {
        if w.elapsed_min < jitter_min(&self.name, cfg) {
            return;
        }
        for kind in [ProviderKind::Claude, ProviderKind::Codex] {
            if self.engine.primary(kind).is_none()
                || self.engine.quarantine_reason(kind).await.is_some()
                || self.engine.has_journal(kind).await
                || self.done_in(kind, w).await
            {
                continue;
            }
            let answer = self
                .engine
                .run_attempt(kind, UpdateTrigger::Schedule, false, false)
                .await;
            let refused = answer.refused.as_deref();
            self.with_state(|s| match refused {
                Some(reason::SESSIONS_RUNNING) => {
                    s.busy.insert(kind, w.id);
                }
                Some(reason::ANOTHER_UPDATE | reason::RECOVERY_PENDING) => {}
                _ => {
                    s.ran.insert(kind, w.id);
                    s.busy.remove(&kind);
                }
            });
        }
    }

    /// A scheduled attempt of `kind` already ran in the window `w`.
    async fn done_in(&self, kind: ProviderKind, w: Window) -> bool {
        if self.with_state(|s| s.ran.get(&kind) == Some(&w.id)) == Some(true) {
            return true;
        }
        match self.engine.store.cli_update_last(kind).await {
            Ok(Some(last)) => {
                last.trigger == UpdateTrigger::Schedule && last.started_at >= w.start_utc_ms
            }
            _ => false,
        }
    }

    /// The external-change check of every tick (§3.9), also when updates
    /// are off. It takes the single flight, so no attempt runs meanwhile.
    async fn external(&self) {
        for kind in [ProviderKind::Claude, ProviderKind::Codex] {
            if self.engine.primary(kind).is_none() || self.engine.has_journal(kind).await {
                continue;
            }
            let Some(flight) = self.engine.take_flight() else {
                return;
            };
            self.external_one(&flight, kind).await;
        }
    }

    async fn external_one(&self, flight: &Flight, kind: ProviderKind) {
        let Ok(install) = self.engine.install_of(kind) else {
            return;
        };
        let Ok(probe) = self.engine.probe_launcher(&flight.id, kind, &install).await else {
            return;
        };
        let (Some(found), Some(cached)) = (probe.version, self.engine.host.installed(kind).await)
        else {
            return;
        };
        let fresh = self
            .with_state(|s| {
                let same = found == cached || s.seen.get(&kind) == Some(&found);
                s.seen.insert(kind, found.clone());
                !same
            })
            .unwrap_or(false);
        if !fresh {
            return;
        }
        if self.engine.quarantine_reason(kind).await.is_some() {
            let _ = self
                .engine
                .verify_inner(&flight.id, kind, UpdateTrigger::External)
                .await;
            return;
        }
        self.engine.host.refresh().await;
        let cfg = self.engine.config.current();
        let a = new_attempt(
            &flight.id,
            kind,
            UpdateTrigger::External,
            install.method,
            &channel_text(kind, &cfg),
            Some(&cached),
            Some(&found),
        );
        let end = end_of(
            UpdateOutcome::Updated,
            Some(reason::OUTSIDE_FLEET_DECK),
            Some(&found),
        );
        if let Err(e) = self.engine.store.cli_update_insert(a, Some(end)).await {
            tracing::warn!(error = %e, "the external change row is not written");
        }
    }
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
