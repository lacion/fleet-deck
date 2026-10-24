//! The actions of `desktop.act` (R13 §6.3, §5C items 4 and 5). Each action
//! runs fixed programs with typed argv. `allow_desktop` is read live right
//! before every program run (the runner's gate).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::coordinator_desktop_text::{code as p6_code, text as p6_text};
use fleetdeck_proto::coordinator_desktop_wire::AuthorityPhase;
use fleetdeck_proto::desktop::{DesktopAction, DesktopKind, Urgency};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::OwnedMutexGuard;

use super::authority::Start;
use super::coordinator::{new_window_id, preview_host_ok, preview_owner, refusal};
use super::detect::Found;
use super::events::{Events, Kind, Record, preview_name, preview_parent, remove_preview_profile};
use super::info::{self, RawMonitor};
use super::run::{CALL_TIMEOUT, Runner, STARTUP_TIMEOUT, on_path};
use super::{Fail, Gate, args, code, profile};

/// How long a close waits for the `closewindow` event.
pub(super) const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// One lock per display: a wall action holds it from its first check to the
/// end of the profile removal or the replacement launch (R13 §5C item 5).
/// The entries are never removed.
pub(super) type WallLocks = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;

#[path = "actions_name.rs"]
mod name_of;
pub(super) use name_of::name;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

/// 16 random hex characters.
fn launch_id() -> Result<String, Fail> {
    let mut bytes = [0_u8; 8];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Fail::new(code::FAILED, "the system random source failed"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// What one launch needs.
struct Launch {
    kind: Kind,
    class: String,
    launch_id: String,
    display_id: Option<String>,
    monitor: Option<String>,
    opener: Option<SessionId>,
    /// The `chromium` arguments.
    args: Vec<String>,
}

pub(super) struct Cx<'a> {
    pub base: &'a ChildEnv,
    pub found: &'a Found,
    pub env: &'a ChildEnv,
    pub events: &'a Events,
    pub walls: &'a WallLocks,
    pub gate: Gate<'a>,
    /// The start check of the authority of this action.
    pub start: &'a Start,
    /// The fingerprint of the deck link that carries an owner
    /// `preview_close`; `None` for every other call.
    pub deck_fp: Option<&'a str>,
}

impl Cx<'_> {
    fn runner(&self) -> Runner<'_> {
        Runner {
            env: self.env,
            gate: Some(self.gate),
            start: Some(self.start),
        }
    }

    async fn hyprctl(&self, args: &[&str]) -> Result<(), Fail> {
        self.runner()
            .run_ok("hyprctl", &argv(args), CALL_TIMEOUT)
            .await
            .map(|_| ())
    }

    /// `hyprctl dispatch <dispatcher> <arg>…`.
    async fn dispatch(&self, parts: &[&str]) -> Result<(), Fail> {
        let mut all = vec!["dispatch"];
        all.extend_from_slice(parts);
        self.hyprctl(&all).await
    }

    /// The live monitors, with `wanted` checked against them.
    async fn monitors(&self, wanted: Option<&str>) -> Result<Vec<RawMonitor>, Fail> {
        let list = info::monitors(&self.runner()).await?;
        if let Some(name) = wanted {
            let names: Vec<String> = list.iter().map(|m| m.info.name.clone()).collect();
            args::monitor(name, &names)?;
        }
        Ok(list)
    }

    /// Runs one action; the answer is the window address, when there is one,
    /// and the preview handle of a coordinator preview (R13 P6 §4.4).
    pub(super) async fn perform(
        &self,
        action: DesktopAction,
    ) -> Result<(Option<String>, Option<String>), Fail> {
        match action {
            DesktopAction::BrowserOpen {
                url,
                monitor,
                session,
            } if self.start.coordinator().is_some() => {
                self.preview_open(&url, monitor, session).await
            }
            DesktopAction::PreviewClose { window_id } if self.start.coordinator().is_some() => {
                self.preview_close(&window_id).await.map(|()| (None, None))
            }
            other => self
                .perform_owner(other)
                .await
                .map(|address| (address, None)),
        }
    }

    /// The actions of the owner and the wake (and the refusals of the
    /// rest); the answer is the window address, when there is one.
    async fn perform_owner(&self, action: DesktopAction) -> Result<Option<String>, Fail> {
        match action {
            DesktopAction::ScreenPower { monitor, on } => {
                if let Some(m) = &monitor {
                    self.monitors(Some(m)).await?;
                }
                let mut parts = vec!["dpms", if on { "on" } else { "off" }];
                parts.extend(monitor.as_deref());
                self.dispatch(&parts).await?;
                Ok(None)
            }
            DesktopAction::ScreenLock => self.lock().await.map(|()| None),
            DesktopAction::ScreenDnd { on } => {
                let flag = if on { "-a" } else { "-r" };
                let args = argv(&["mode", flag, "do-not-disturb"]);
                self.runner().run_ok("makoctl", &args, CALL_TIMEOUT).await?;
                Ok(None)
            }
            DesktopAction::ScreenIdleInhibit { on } => self.idle(on).await.map(|()| None),
            DesktopAction::WorkspaceShow { monitor, workspace } => {
                let n = args::workspace(workspace)?.to_string();
                self.monitors(Some(&monitor)).await?;
                self.dispatch(&["focusmonitor", &monitor]).await?;
                self.dispatch(&["workspace", &n]).await?;
                Ok(None)
            }
            DesktopAction::Notify {
                title,
                body,
                urgency,
            } => self.notify(&title, &body, urgency).await.map(|()| None),
            DesktopAction::BrowserOpen {
                url,
                monitor,
                session,
            } => self.browser(&url, monitor, session).await.map(Some),
            DesktopAction::WallOpen {
                display_id,
                monitor,
                workspace,
                url,
            } => self
                .wall_open(&display_id, &monitor, workspace, &url)
                .await
                .map(Some),
            DesktopAction::WallClose { display_id } => {
                self.wall_close(&display_id).await.map(|_| None)
            }
            DesktopAction::WallReload { display_id, url } => {
                self.wall_reload(&display_id, &url).await.map(Some)
            }
            DesktopAction::WindowClose { address } => {
                let address = args::address(&address)?;
                let record = self.events.record(address).ok_or_else(|| {
                    Fail::new(code::FORBIDDEN, "Fleet Deck did not open this window.")
                })?;
                self.close_window(&record).await?;
                self.events.wait_gone(address, CLOSE_WAIT).await;
                Ok(None)
            }
            // R13 P5a task A: a stub until the capture task (D2); no program runs.
            DesktopAction::ScreenCapture { .. } => Err(Fail::new(
                code::UNSUPPORTED,
                "This station cannot take screenshots yet.",
            )),
            // R13 P5b task F: a stub until the recording task (S1); no program runs.
            DesktopAction::ScreenRecord { .. } => Err(Fail::new(
                code::UNSUPPORTED,
                fleetdeck_proto::capture::text::RECORDING_UNSUPPORTED,
            )),
            // R13 P6 follow-up: the owner closes a preview that an agent
            // opened (`desktop/preview_close.rs`).
            DesktopAction::PreviewClose { window_id } => {
                self.owner_preview_close(&window_id).await.map(|()| None)
            }
            // R13 P7 and P7b task F: no owner path sends input; the click and
            // the broader variants refuse as the P7 ones do.
            DesktopAction::InputKeyPress { .. }
            | DesktopAction::InputTypeText { .. }
            | DesktopAction::InputPointerMove { .. }
            | DesktopAction::InputPointerClick { .. }
            | DesktopAction::InputAnyKeyPress { .. }
            | DesktopAction::InputAnyTypeText { .. }
            | DesktopAction::InputAnyPointerMove { .. }
            | DesktopAction::InputAnyPointerClick { .. } => Err(Fail::input_unsupported()),
        }
    }

    async fn lock(&self) -> Result<(), Fail> {
        let runner = self.runner();
        let omarchy_lock =
            self.found.kind == DesktopKind::Omarchy && on_path(self.env, "omarchy-lock-screen");
        if omarchy_lock {
            runner
                .run_ok("omarchy-lock-screen", &[], CALL_TIMEOUT)
                .await?;
        } else {
            runner
                .run_ok("loginctl", &argv(&["lock-session"]), CALL_TIMEOUT)
                .await?;
        }
        Ok(())
    }

    async fn idle(&self, on: bool) -> Result<(), Fail> {
        if self.found.kind != DesktopKind::Omarchy {
            return Err(Fail::new(
                code::UNSUPPORTED,
                "Idle inhibit needs Omarchy on this desktop.",
            ));
        }
        let runner = self.runner();
        let now = info::idle_inhibited(&runner).await.ok_or_else(|| {
            Fail::new(
                code::FAILED,
                "The idle state of this desktop cannot be read.",
            )
        })?;
        if now != on {
            runner
                .run_ok("omarchy-toggle-idle", &[], CALL_TIMEOUT)
                .await?;
        }
        Ok(())
    }

    async fn notify(&self, title: &str, body: &str, urgency: Urgency) -> Result<(), Fail> {
        send_notify(&self.runner(), title, body, urgency).await
    }

    /// The `chromium` command, through `uwsm-app --` when that is present.
    fn wrapped(&self, chromium_args: Vec<String>) -> (&'static str, Vec<String>) {
        if on_path(self.env, "uwsm-app") {
            let mut all = vec!["--".to_owned(), "chromium".to_owned()];
            all.extend(chromium_args);
            ("uwsm-app", all)
        } else {
            ("chromium", chromium_args)
        }
    }

    /// Starts Chromium and waits up to 15 s for the window of its class.
    async fn launch(&self, spec: Launch) -> Result<String, Fail> {
        if !self.events.wait_connected(STARTUP_TIMEOUT).await {
            return Err(Fail::new(
                code::FAILED,
                "The window events of this desktop are not available.",
            ));
        }
        // A coordinator preview checks the authority again right before the
        // spawn (§6.3): no await follows this check before the program starts.
        if spec.kind == Kind::Preview && self.start.coordinator().is_some() {
            self.start
                .check_phase(AuthorityPhase::Execute, self.env)
                .await?;
        }
        let rx = self.events.register(Record {
            signature: String::new(),
            address: String::new(),
            class: spec.class.clone(),
            kind: spec.kind,
            display_id: spec.display_id.clone(),
            monitor: spec.monitor.clone(),
            opener: spec.opener.clone(),
            launch_id: spec.launch_id.clone(),
        })?;
        let (program, program_args) = self.wrapped(spec.args);
        let mut child = match self.runner().spawn(program, &program_args).await {
            Ok(child) => child,
            Err(e) => {
                self.events.cancel(&spec.class);
                self.drop_preview_profile(&spec.kind, &spec.launch_id);
                return Err(e);
            }
        };
        let mut rx = rx;
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        let mut exited = false;
        let found = loop {
            tokio::select! {
                got = &mut rx => break got.ok(),
                status = child.wait(), if !exited => {
                    exited = true;
                    // A launcher may exit 0 after it started the browser.
                    let bad = match status {
                        Ok(s) if s.success() => None,
                        Ok(s) => Some(s.code().unwrap_or(-1)),
                        Err(_) => Some(-1),
                    };
                    if let Some(exit) = bad {
                        self.events.cancel(&spec.class);
                        self.drop_preview_profile(&spec.kind, &spec.launch_id);
                        return Err(Fail::new(
                            code::FAILED,
                            format!("`{program}` stopped with exit code {exit}"),
                        ));
                    }
                }
                () = tokio::time::sleep_until(deadline) => break None,
            }
        };
        if let Some(address) = found {
            if !exited {
                // Reap the process when it ends.
                tokio::spawn(async move {
                    if let Err(e) = child.wait().await {
                        tracing::debug!(error = %e, "a desktop launch cannot be waited for");
                    }
                });
            }
            return Ok(address);
        }
        self.events.cancel(&spec.class);
        if !exited && let Err(e) = child.kill().await {
            tracing::debug!(error = %e, "a launch that gave no window cannot be stopped");
        }
        self.drop_preview_profile(&spec.kind, &spec.launch_id);
        Err(Fail::new(
            code::FAILED,
            format!(
                "The window did not appear in {} s.",
                STARTUP_TIMEOUT.as_secs()
            ),
        ))
    }

    fn drop_preview_profile(&self, kind: &Kind, launch_id: &str) {
        if *kind == Kind::Preview {
            remove_preview_profile(&self.found.runtime_dir, launch_id);
        }
    }

    /// Closes the window that a launch opened when a later step failed.
    async fn abandon(&self, address: &str) {
        if let Err(e) = self
            .dispatch(&["closewindow", &format!("address:{address}")])
            .await
        {
            tracing::debug!(code = e.code, "a window of a failed action stays open");
        }
        self.events.forget(address);
    }

    /// The workspace of `monitor` now.
    fn active_workspace(list: &[RawMonitor], monitor: &str) -> Result<String, Fail> {
        list.iter()
            .find(|m| m.info.name == monitor)
            .and_then(|m| m.active_workspace)
            .map(|id| id.to_string())
            .ok_or_else(|| {
                Fail::new(
                    code::FAILED,
                    "The workspace of this monitor cannot be read.",
                )
            })
    }

    async fn browser(
        &self,
        url: &str,
        monitor: Option<String>,
        session: Option<SessionId>,
    ) -> Result<String, Fail> {
        let url = args::url(url)?;
        let target = match &monitor {
            Some(m) => Some(Self::active_workspace(&self.monitors(Some(m)).await?, m)?),
            None => None,
        };
        let id = launch_id()?;
        let profile = preview_parent(&self.found.runtime_dir).join(preview_name(&id));
        // A file change is a side effect: the start check comes first.
        self.start.begin(self.env).await?;
        make_parent(&preview_parent(&self.found.runtime_dir)).await?;
        let class = format!("fleetdeck-preview-{id}");
        let address = self
            .launch(Launch {
                kind: Kind::Preview,
                class: class.clone(),
                launch_id: id,
                display_id: None,
                monitor: monitor.clone(),
                opener: session,
                args: vec![
                    format!("--app={url}"),
                    format!("--user-data-dir={}", profile.display()),
                    format!("--class={class}"),
                    "--no-first-run".to_owned(),
                ],
            })
            .await?;
        if let Some(workspace) = target {
            // A refusal leaves the opened window: it is visible already (§6.4).
            self.recheck_write().await?;
            let moved = self
                .dispatch(&[
                    "movetoworkspacesilent",
                    &format!("{workspace},address:{address}"),
                ])
                .await;
            if let Err(e) = moved {
                self.abandon(&address).await;
                return Err(e);
            }
        }
        Ok(address)
    }

    /// `preview_open` of a coordinator (R13 P6 §4.4): the host must be one
    /// of the allowed hosts; the launch is the fresh-profile launch of the
    /// owner; the answer is a station-made handle and never the address.
    async fn preview_open(
        &self,
        url: &str,
        monitor: Option<String>,
        session: Option<SessionId>,
    ) -> Result<(Option<String>, Option<String>), Fail> {
        let (Some(ctx), Some(extras)) = (self.start.coordinator(), self.start.link_extras()) else {
            return Err(refusal());
        };
        let valid = args::url(url)?;
        if !preview_host_ok(valid, extras.tailnet_host.as_deref()) {
            return Err(Fail::new(code::INVALID_PARAMS, "`url` is not valid"));
        }
        let address = self.browser(url, monitor, session).await?;
        let window_id = new_window_id()?;
        self.events.register_preview(
            &window_id,
            preview_owner(&extras.deck_fp, ctx),
            &ctx.caller_label,
            &address,
        )?;
        Ok((None, Some(window_id)))
    }

    /// `preview_close` of a coordinator (R13 P6 §4.5): the handle must name
    /// a preview of the same deck, grant, caller station and caller session.
    async fn preview_close(&self, window_id: &str) -> Result<(), Fail> {
        let (Some(ctx), Some(extras)) = (self.start.coordinator(), self.start.link_extras()) else {
            return Err(refusal());
        };
        let owner = preview_owner(&extras.deck_fp, ctx);
        let not_found = || {
            Fail::new(
                p6_code::DESKTOP_WINDOW_NOT_FOUND,
                p6_text(p6_code::DESKTOP_WINDOW_NOT_FOUND).unwrap_or_default(),
            )
        };
        let (found_owner, record) = self.events.preview(window_id).ok_or_else(not_found)?;
        if found_owner != owner {
            return Err(not_found());
        }
        // The remote check, then the record is read again: it must be the
        // same one. The identity check of `close_window` follows.
        self.start.begin(self.env).await?;
        match self.events.preview(window_id) {
            Some((o, r)) if o == owner && r == record => {}
            _ => return Err(not_found()),
        }
        self.close_window(&record).await?;
        self.events.wait_gone(&record.address, CLOSE_WAIT).await;
        Ok(())
    }

    /// `$XDG_DATA_HOME` or `~/.local/share`, then `fleetdeck`.
    fn data_dir(&self) -> Result<PathBuf, Fail> {
        let absolute = |name: &str| {
            self.base
                .get(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let data = absolute("XDG_DATA_HOME")
            .or_else(|| absolute("HOME").map(|h| h.join(".local/share")))
            .ok_or_else(|| Fail::new(code::FAILED, "This station has no data folder."))?;
        Ok(data.join("fleetdeck"))
    }

    /// The lock of `display_id`: one wall action at a time per display.
    async fn wall_guard(&self, display_id: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut map = self.walls.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(map.entry(display_id.to_owned()).or_default())
        };
        lock.lock_owned().await
    }

    /// The checks of a wall open that do not depend on the old window: the
    /// arguments, the live `allow_desktop` and the monitor.
    async fn wall_open_check(
        &self,
        display_id: &str,
        monitor: &str,
        workspace: Option<u8>,
        url: &str,
    ) -> Result<(), Fail> {
        args::display_id(display_id)?;
        args::url(url)?;
        workspace.map(args::workspace).transpose()?;
        if !(self.gate)() {
            return Err(Fail::forbidden());
        }
        self.monitors(Some(monitor)).await?;
        Ok(())
    }

    async fn wall_open(
        &self,
        display_id: &str,
        monitor: &str,
        workspace: Option<u8>,
        url: &str,
    ) -> Result<String, Fail> {
        let display_id = args::display_id(display_id)?;
        let _guard = self.wall_guard(display_id).await;
        self.wall_open_locked(display_id, monitor, workspace, url)
            .await
    }

    /// A wall open; the caller holds the lock of `display_id`.
    async fn wall_open_locked(
        &self,
        display_id: &str,
        monitor: &str,
        workspace: Option<u8>,
        url: &str,
    ) -> Result<String, Fail> {
        self.wall_open_check(display_id, monitor, workspace, url)
            .await?;
        if self.events.wall(display_id).is_some() {
            return Err(Fail::new(
                code::CONFLICT,
                "This display already has a wall window; close it first.",
            ));
        }
        let list = self.monitors(Some(monitor)).await?;
        let target = match workspace {
            Some(n) => n.to_string(),
            None => Self::active_workspace(&list, monitor)?,
        };
        let parent = self.data_dir()?;
        let profile = parent.join(format!("wall-{display_id}"));
        if tokio::fs::symlink_metadata(&profile)
            .await
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(not_plain());
        }
        self.start.begin(self.env).await?;
        make_parent(&parent).await?;
        let id = launch_id()?;
        let class = format!("fleetdeck-wall-{id}");
        let address = self
            .launch(Launch {
                kind: Kind::Wall,
                class: class.clone(),
                launch_id: id,
                display_id: Some(display_id.to_owned()),
                monitor: Some(monitor.to_owned()),
                opener: None,
                args: vec![
                    format!("--app={url}"),
                    format!("--user-data-dir={}", profile.display()),
                    format!("--class={class}"),
                    "--no-first-run".to_owned(),
                    "--kiosk".to_owned(),
                ],
            })
            .await?;
        let place = async {
            self.dispatch(&[
                "movetoworkspacesilent",
                &format!("{target},address:{address}"),
            ])
            .await?;
            self.dispatch(&["focuswindow", &format!("address:{address}")])
                .await?;
            self.dispatch(&["fullscreen", "0"]).await
        };
        if let Err(e) = place.await {
            self.abandon(&address).await;
            return Err(e);
        }
        Ok(address)
    }

    /// Closes the wall of `display_id` and deletes its profile. The answer is
    /// the monitor that the wall was on.
    async fn wall_close(&self, display_id: &str) -> Result<Option<String>, Fail> {
        let display_id = args::display_id(display_id)?;
        let _guard = self.wall_guard(display_id).await;
        self.wall_close_locked(display_id).await
    }

    /// A wall close; the caller holds the lock of `display_id`. The profile
    /// is deleted only after the window is confirmed gone.
    async fn wall_close_locked(&self, display_id: &str) -> Result<Option<String>, Fail> {
        let record = self
            .events
            .wall(display_id)
            .ok_or_else(|| Fail::new(code::NOT_FOUND, "This display has no wall window."))?;
        self.close_window(&record).await?;
        if !self.events.wait_gone(&record.address, CLOSE_WAIT).await {
            // The record stays: a second close can try again.
            return Err(Fail::new(
                code::FAILED,
                "The wall window did not close; its profile stays. Try again.",
            ));
        }
        // A lost event stream clears the records too: only a fresh window
        // list confirms that the window is gone.
        let not_closed = || {
            Fail::new(
                code::FAILED,
                "The wall window did not close; its profile stays. Try again.",
            )
        };
        let listed = info::clients(&self.runner())
            .await
            .map_err(|_| not_closed())?;
        let wall_class = record.class.as_str();
        if listed
            .iter()
            .any(|(address, class)| *address == record.address || class == wall_class)
        {
            return Err(not_closed());
        }
        let parent = self.data_dir()?;
        let name = format!("wall-{display_id}");
        let removed = tokio::task::spawn_blocking(move || profile::remove(&parent, &name))
            .await
            .map_err(|_| Fail::new(code::FAILED, "The wall profile removal stopped."))?;
        match removed {
            Ok(()) => Ok(record.monitor),
            Err(profile::ProfileError::NotPlain) => Err(not_plain()),
            Err(profile::ProfileError::Io(e)) => Err(Fail::new(
                code::FAILED,
                format!("The wall profile was not removed: {}", e.kind()),
            )),
        }
    }

    /// `wall.reload`: the checks of the new window first, then the close,
    /// then the open on the same monitor, all under one lock.
    async fn wall_reload(&self, display_id: &str, url: &str) -> Result<String, Fail> {
        let display_id = args::display_id(display_id)?;
        let _guard = self.wall_guard(display_id).await;
        let record = self
            .events
            .wall(display_id)
            .ok_or_else(|| Fail::new(code::NOT_FOUND, "This display has no wall window."))?;
        let monitor = record
            .monitor
            .ok_or_else(|| Fail::new(code::NOT_FOUND, "The wall has no monitor to reopen on."))?;
        self.wall_open_check(display_id, &monitor, None, url)
            .await?;
        self.wall_close_locked(display_id).await?;
        self.wall_open_locked(display_id, &monitor, None, url).await
    }

    /// Closes the window of `record` after a fresh `clients` check of the
    /// instance, the address and the class.
    pub(super) async fn close_window(&self, record: &Record) -> Result<(), Fail> {
        let clients = info::clients(&self.runner()).await?;
        let alive = record.signature == self.found.signature
            && clients
                .iter()
                .any(|(a, c)| *a == record.address && *c == record.class);
        if !alive {
            self.events.forget(&record.address);
            return Err(Fail::new(code::NOT_FOUND, "The window is gone."));
        }
        // A coordinator close checks the authority again at the write itself
        // (§6.3): no other await between this check and the dispatch.
        if self.start.coordinator().is_some() {
            self.recheck_write().await?;
            // The await can outlive the window: a close, an event gap or a
            // new compositor clears its record, and a new window that reuses
            // the address has another record. Only the same record closes.
            if self.events.record(&record.address).as_ref() != Some(record)
                || record.signature != self.found.signature
            {
                return Err(Fail::new(code::NOT_FOUND, "The window is gone."));
            }
        }
        self.dispatch(&["closewindow", &format!("address:{}", record.address)])
            .await
    }

    /// The check right before a compositor write of a coordinator action
    /// (§6.3). An owner or wall action has no coordinator context and runs
    /// no check here.
    async fn recheck_write(&self) -> Result<(), Fail> {
        if self.start.coordinator().is_some() {
            self.start
                .check_phase(AuthorityPhase::Execute, self.env)
                .await?;
        }
        Ok(())
    }
}

/// `notify-send` with the fixed argv: the app name, the urgency, `--` and the
/// checked title and body. The runner decides what runs before the program.
pub(super) async fn send_notify(
    runner: &Runner<'_>,
    title: &str,
    body: &str,
    urgency: Urgency,
) -> Result<(), Fail> {
    let title = args::title(title)?;
    let body = args::body(body);
    let urgency = match urgency {
        Urgency::Low => "--urgency=low",
        Urgency::Normal => "--urgency=normal",
        Urgency::Critical => "--urgency=critical",
    };
    let args = vec![
        "--app-name=Fleet Deck".to_owned(),
        urgency.to_owned(),
        "--".to_owned(),
        title,
        body,
    ];
    runner.run_ok("notify-send", &args, CALL_TIMEOUT).await?;
    Ok(())
}

fn not_plain() -> Fail {
    Fail::new(
        code::FAILED,
        "The wall profile is not a plain folder; it was not removed.",
    )
}

/// Creates the folder that holds the profiles (0700).
async fn make_parent(dir: &std::path::Path) -> Result<(), Fail> {
    tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .await
        .map_err(|e| {
            Fail::new(
                code::FAILED,
                format!("The profile folder cannot be made: {}", e.kind()),
            )
        })
}

#[cfg(test)]
#[path = "actions_preview_close_tests.rs"]
mod preview_close_tests;
