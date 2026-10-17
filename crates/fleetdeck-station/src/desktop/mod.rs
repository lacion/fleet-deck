//! Desktop control on the station (R13 §6, feature `desktop.v1`, DL-201).
//!
//! Linux only: another system reports no desktop and refuses every action
//! with `unsupported`. The adapter runs fixed programs with typed argv (see
//! [`run`]); it never runs a shell and never takes a command from the caller.
//!
//! - [`detect`]: the Hyprland instance, its Wayland socket and Omarchy.
//! - [`env`]: the environment of desktop children.
//! - [`args`]: the grammar of every argument.
//! - [`info`]: `DesktopInfo`.
//! - [`events`]: the window records from the Hyprland event socket.
//! - [`authority`]: the dispatch authority of `desktop.act`.
//! - [`deadline`]: the total deadlines.
//! - [`quiet`]: the local UTC offset and the quiet hours.
//!
//! The state of the adapter is process-global ([`service`]); a test replaces
//! it with [`install`].

mod actions;
pub mod args;
pub mod authority;
pub mod deadline;
pub mod detect;
pub mod env;
pub mod events;
pub mod info;
pub mod profile;
pub mod quiet;
pub mod run;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction, DesktopAllowed, DesktopInfo};

use detect::Found;
use events::Events;

/// The error codes of the desktop methods.
pub mod code {
    pub const UNSUPPORTED: &str = "unsupported";
    pub const FORBIDDEN: &str = "forbidden";
    pub const INVALID_PARAMS: &str = "invalid_params";
    pub const NOT_FOUND: &str = "not_found";
    pub const CONFLICT: &str = "conflict";
    pub const FAILED: &str = "failed";
}

/// A refused or failed desktop call: a wire error code and a message that
/// holds no URL, no text of a notification and no argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail {
    pub code: &'static str,
    pub message: String,
}

impl Fail {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `allow_desktop` is off.
    #[must_use]
    pub fn forbidden() -> Self {
        Self::new(
            code::FORBIDDEN,
            "Desktop control is off on this station (allow_desktop = false).",
        )
    }

    #[must_use]
    pub fn no_desktop() -> Self {
        Self::new(code::UNSUPPORTED, "This station has no desktop.")
    }
}

/// The live `allow_desktop` of the station policy. It is called right before
/// every program run of an action.
pub type Gate<'a> = &'a (dyn Fn() -> bool + Send + Sync);

/// How long `desktop.info` is served from the cache.
const CACHE_FOR: Duration = Duration::from_secs(10);

struct Cached {
    at: Instant,
    signature: String,
    info: DesktopInfo,
}

/// What one call found: the desktop and the environment of its children.
pub(crate) struct Ctx {
    pub found: Found,
    pub env: ChildEnv,
}

/// The desktop adapter of the station.
pub struct Desktop {
    base: ChildEnv,
    events: Events,
    walls: actions::WallLocks,
    cache: Mutex<Option<Cached>>,
    limits: Mutex<deadline::Limits>,
}

impl Desktop {
    /// An adapter for the station environment `base`.
    #[must_use]
    pub fn with_env(base: ChildEnv) -> Self {
        Self {
            base,
            events: Events::new(),
            walls: actions::WallLocks::default(),
            cache: Mutex::new(None),
            limits: Mutex::new(deadline::Limits::default()),
        }
    }

    /// An adapter for the environment of this process.
    #[must_use]
    pub fn from_process() -> Self {
        Self::with_env(ChildEnv::from_process())
    }

    /// Looks for the desktop and starts the event task.
    async fn context(&self) -> Result<Ctx, Fail> {
        let found = detect::detect(&self.base)
            .await
            .ok_or_else(Fail::no_desktop)?;
        let env = env::desktop_env(&self.base, &found)?;
        self.events.ensure(&found.runtime_dir, &found.signature);
        Ok(Ctx { found, env })
    }

    async fn fresh_info(
        &self,
        ctx: &Ctx,
        gate: Gate<'_>,
        refresh: bool,
    ) -> Result<DesktopInfo, Fail> {
        let cached = if refresh {
            None
        } else {
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .filter(|c| c.signature == ctx.found.signature && c.at.elapsed() < CACHE_FOR)
                .map(|c| c.info.clone())
        };
        let mut info = match cached {
            Some(info) => info,
            None => {
                let info = info::collect(&ctx.env, &ctx.found).await?;
                *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(Cached {
                    at: Instant::now(),
                    signature: ctx.found.signature.clone(),
                    info: info.clone(),
                });
                info
            }
        };
        // The live parts are never cached.
        info.allowed = DesktopAllowed {
            desktop: gate(),
            capture: false,
            input: false,
        };
        info.walls = self.events.walls();
        Ok(info)
    }

    fn limits(&self) -> deadline::Limits {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the total deadlines (tests make them short).
    #[cfg(test)]
    pub(crate) fn set_limits(&self, limits: deadline::Limits) {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner) = limits;
    }

    async fn info_within(&self, gate: Gate<'_>, refresh: bool) -> Result<DesktopInfo, Fail> {
        let call = async {
            let ctx = self.context().await?;
            self.fresh_info(&ctx, gate, refresh).await
        };
        match tokio::time::timeout(self.limits().short, call).await {
            Ok(result) => result,
            Err(_) => Err(Fail::new(code::FAILED, deadline::INFO_TIMEOUT)),
        }
    }

    /// `desktop.info`: the cached info, at most 10 s old.
    ///
    /// # Errors
    /// `unsupported` without a desktop; `failed` when the monitors cannot be
    /// read or the 30 s deadline passes.
    pub async fn info(&self, gate: Gate<'_>) -> Result<DesktopInfo, Fail> {
        self.info_within(gate, false).await
    }

    /// `desktop.info` with `fresh`: the station reads everything again and
    /// ignores the cache of 10 s (R13 §6D item 4).
    ///
    /// # Errors
    /// As [`Desktop::info`].
    pub async fn info_fresh(&self, gate: Gate<'_>) -> Result<DesktopInfo, Fail> {
        self.info_within(gate, true).await
    }

    /// `desktop.act`: one action that the deck authorized, then the fresh
    /// info. The caller checks `allow_desktop` at intake; every program run
    /// checks it again, and the start check of `admitted` runs before the
    /// first program. The whole call has a total deadline (90 s for a wall
    /// open, a wall reload and a browser launch, else 30 s).
    ///
    /// # Errors
    /// `unsupported`, `forbidden`, `invalid_params`, `not_found`, `conflict`
    /// or `failed`.
    pub async fn act(
        &self,
        gate: Gate<'_>,
        action: DesktopAction,
        admitted: authority::Admitted,
    ) -> Result<DesktopActResult, Fail> {
        let name = actions::name(&action);
        let limit = self.limits().of(&action);
        let result = match tokio::time::timeout(limit, self.act_inner(gate, action, admitted)).await
        {
            Ok(result) => result,
            Err(_) => Err(Fail::new(code::FAILED, deadline::ACT_TIMEOUT)),
        };
        match &result {
            Ok(_) => tracing::info!(action = name, result = "ok", "desktop action"),
            Err(e) => tracing::info!(action = name, result = e.code, "desktop action"),
        }
        result
    }

    async fn act_inner(
        &self,
        gate: Gate<'_>,
        action: DesktopAction,
        admitted: authority::Admitted,
    ) -> Result<DesktopActResult, Fail> {
        let ctx = self.context().await?;
        if !gate() {
            return Err(Fail::forbidden());
        }
        let start = authority::Start::new(admitted, &action);
        let cx = actions::Cx {
            base: &self.base,
            found: &ctx.found,
            env: &ctx.env,
            events: &self.events,
            walls: &self.walls,
            gate,
            start: &start,
        };
        let address = cx.perform(action).await?;
        let info = self.fresh_info(&ctx, gate, true).await?;
        Ok(DesktopActResult { address, info })
    }
}

static SERVICE: Mutex<Option<Arc<Desktop>>> = Mutex::new(None);

/// The adapter of this station: built from the process environment at the
/// first call.
#[must_use]
pub fn service() -> Arc<Desktop> {
    let mut slot = SERVICE.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(slot.get_or_insert_with(|| Arc::new(Desktop::from_process())))
}

/// Replaces the adapter of this process. Tests use it to point the station
/// at a fake desktop.
pub fn install(desktop: Arc<Desktop>) {
    *SERVICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(desktop);
}

#[cfg(test)]
mod authority_tests;
#[cfg(test)]
mod detect_tests;
#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod window_tests;
