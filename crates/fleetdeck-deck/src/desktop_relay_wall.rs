//! The wall lock and the wall actions of the desktop relay (R13 P3b). Moved
//! from `desktop_relay.rs` for the module size rule (R13 P6 F0).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use fleetdeck_proto::StationId;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};

use super::{
    RelayCtx, RelayError, bind_error, is_loopback_origin, monitor_ok, run_act, store_error,
    valid_display_id, wall_url,
};
use crate::link::desktop_authority::resolve;

type LockMap = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;

/// The locks of the wall actions: one for each display, held from the first
/// store step to the commit or the clear.
static WALL_LOCKS: OnceLock<LockMap> = OnceLock::new();

pub(super) fn wall_locks() -> &'static LockMap {
    WALL_LOCKS.get_or_init(Mutex::default)
}

/// A held wall lock. The map entry goes away with the last holder.
pub(super) struct WallGuard {
    display_id: String,
    _held: tokio::sync::OwnedMutexGuard<()>,
}

impl WallGuard {
    /// Takes the lock of `display_id` without waiting.
    pub(super) fn take(display_id: &str) -> Result<Self, RelayError> {
        let mut map = wall_locks().lock().unwrap_or_else(PoisonError::into_inner);
        let lock = Arc::clone(map.entry(display_id.to_owned()).or_default());
        match lock.try_lock_owned() {
            Ok(held) => Ok(Self {
                display_id: display_id.to_owned(),
                _held: held,
            }),
            Err(_) => Err(RelayError::Conflict),
        }
    }
}

impl Drop for WallGuard {
    fn drop(&mut self) {
        let mut map = wall_locks().lock().unwrap_or_else(PoisonError::into_inner);
        // The map and this guard hold the only two references.
        if map
            .get(&self.display_id)
            .is_some_and(|l| Arc::strong_count(l) == 2)
        {
            map.remove(&self.display_id);
        }
    }
}

/// One wall action of the owner, with its display lock.
pub(super) struct Wall<'a> {
    ctx: &'a RelayCtx,
    station: &'a StationId,
    display_id: &'a str,
    origin: Option<&'a str>,
    _guard: WallGuard,
}

impl<'a> Wall<'a> {
    pub(super) fn start(
        ctx: &'a RelayCtx,
        station: &'a StationId,
        display_id: &'a str,
        origin: Option<&'a str>,
    ) -> Result<Self, RelayError> {
        if !valid_display_id(display_id) {
            return Err(RelayError::Invalid);
        }
        let guard = WallGuard::take(display_id)?;
        Ok(Self {
            ctx,
            station,
            display_id,
            origin,
            _guard: guard,
        })
    }

    /// The board address for the pair page: a wall needs one that is not
    /// loopback.
    fn origin(&self) -> Result<&'a str, RelayError> {
        match self.origin {
            Some(o) if !is_loopback_origin(o) => Ok(o),
            _ => Err(RelayError::Invalid),
        }
    }

    /// `wall_open`: begin, send, commit.
    pub(super) async fn open(
        self,
        monitor: &str,
        workspace: Option<u8>,
    ) -> Result<DesktopActResult, RelayError> {
        monitor_ok(monitor)?;
        let origin = self.origin()?;
        self.launch(monitor, origin, |url| DesktopAction::WallOpen {
            display_id: self.display_id.to_owned(),
            monitor: monitor.to_owned(),
            workspace,
            url,
        })
        .await
    }

    /// `wall_reload`: a new launch on the bound monitor.
    pub(super) async fn reload(self) -> Result<DesktopActResult, RelayError> {
        let origin = self.origin()?;
        let display = self
            .ctx
            .store
            .display_get(self.display_id.to_owned())
            .await
            .map_err(|e| store_error(&e))?
            .ok_or(RelayError::NotFound)?;
        let binding = display
            .binding
            .filter(|b| &b.station_id == self.station)
            .ok_or(RelayError::Conflict)?;
        self.launch(&binding.monitor, origin, |url| DesktopAction::WallReload {
            display_id: self.display_id.to_owned(),
            url,
        })
        .await
    }

    async fn launch(
        &self,
        monitor: &str,
        origin: &str,
        action: impl FnOnce(String) -> DesktopAction,
    ) -> Result<DesktopActResult, RelayError> {
        // A missing link or feature fails before the store moves.
        resolve(&self.ctx.link, &self.ctx.store, self.station)?;
        let ticket = self
            .ctx
            .store
            .display_begin_open(
                self.display_id.to_owned(),
                self.station.clone(),
                monitor.to_owned(),
            )
            .await
            .map_err(bind_error)?;
        let url = wall_url(origin, self.display_id, ticket.op_revision);
        // An error or a timeout leaves the binding as it is: the outcome on
        // the station can be unknown, and the deck never retries.
        let result = run_act(self.ctx, self.station, &action(url), None).await?;
        self.ctx
            .store
            .display_commit_binding(
                self.display_id.to_owned(),
                ticket.op_revision,
                self.station.clone(),
                monitor.to_owned(),
            )
            .await
            .map_err(bind_error)?;
        Ok(result)
    }

    /// `wall_close`: after a successful close the binding goes only when it
    /// is still the one seen at the start. A missing display is no error.
    pub(super) async fn close(self) -> Result<DesktopActResult, RelayError> {
        let action = DesktopAction::WallClose {
            display_id: self.display_id.to_owned(),
        };
        // The binding on this station before the send (none: no clear).
        let store = &self.ctx.store;
        let seen = (store.display_get(self.display_id.to_owned()).await)
            .map_err(|e| store_error(&e))?
            .and_then(|d| Some((d.binding?, d.binding_revision)))
            .filter(|(b, _)| &b.station_id == self.station);
        let result = run_act(self.ctx, self.station, &action, None).await?;
        if let Some((_, revision)) = seen {
            // `false`: the binding changed meanwhile and stays.
            let id = self.display_id.to_owned();
            let at = self.station.clone();
            (store.display_clear_binding_at(id, at, revision).await)
                .map_err(|e| store_error(&e))?;
        }
        Ok(result)
    }
}
