//! Window records from the Hyprland event socket (R13 §5C item 5, §6).
//!
//! A task reads `$XDG_RUNTIME_DIR/hypr/<signature>/.socket2.sock`. A launch
//! registers a unique class; the first `openwindow` event with that class is
//! the launch's window and creates a record. `closewindow` deletes the record
//! (and the profile of a preview). The pid is never used. A read error, an
//! EOF, a new signature or a reconnect clears all records, so the station
//! never owns a window that it might confuse.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::desktop::WallWindow;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{oneshot, watch};

use super::{Fail, code, profile};

/// The longest event line that the reader keeps, in bytes.
pub const MAX_LINE: usize = 64 * 1024;
/// The pause before a reconnect.
const RECONNECT_PAUSE: Duration = Duration::from_secs(2);

/// What a window is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wall,
    Preview,
}

/// A window that the station opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub signature: String,
    /// Empty in a pending launch.
    pub address: String,
    pub class: String,
    pub kind: Kind,
    pub display_id: Option<String>,
    pub monitor: Option<String>,
    pub opener: Option<SessionId>,
    pub launch_id: String,
}

/// Who opened a coordinator preview (R13 P6 §4.5). A renewal of a grant
/// keeps the grant id; a new grant does not match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewOwner {
    pub deck_fp: String,
    pub grant_id: String,
    pub caller_station: String,
    pub caller_session: String,
}

/// The most preview records that the station keeps; the 257th registration
/// removes the oldest.
pub const MAX_PREVIEWS: usize = 256;

/// One event of the socket that the station uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Open { address: String, class: String },
    Close { address: String },
}

/// Parses one `EVENT>>DATA` line. `openwindow` and `openwindowv2` carry
/// `ADDRESS,WORKSPACE,CLASS,TITLE`; the title may hold commas. The address
/// has no `0x` prefix on the socket: it gets one here, in lower case.
#[must_use]
pub fn parse_event(line: &str) -> Option<Event> {
    let (name, data) = line.split_once(">>")?;
    match name {
        "openwindow" | "openwindowv2" => {
            let mut parts = data.splitn(4, ',');
            let address = address_of(parts.next()?)?;
            let _workspace = parts.next()?;
            let class = parts.next()?;
            parts.next()?;
            Some(Event::Open {
                address,
                class: class.to_owned(),
            })
        }
        "closewindow" => Some(Event::Close {
            address: address_of(data.trim())?,
        }),
        _ => None,
    }
}

fn address_of(raw: &str) -> Option<String> {
    let hex = raw.strip_prefix("0x").unwrap_or(raw).to_ascii_lowercase();
    let ok = (1..=16).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| format!("0x{hex}"))
}

/// What the line reader returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// A line, now in the buffer (without the newline).
    Text,
    /// A line longer than [`MAX_LINE`]: skipped, the buffer is empty.
    TooLong,
}

/// Reads one line of at most [`MAX_LINE`] bytes into `buf`; a longer line is
/// skipped to its end. `None` at EOF (a partial last line is dropped).
///
/// # Errors
/// The read error.
pub async fn next_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<Line>> {
    buf.clear();
    let mut too_long = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(None);
        }
        let newline = chunk.iter().position(|b| *b == b'\n');
        let take = newline.unwrap_or(chunk.len());
        if !too_long {
            if buf.len() + take > MAX_LINE {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        reader.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(if too_long { Line::TooLong } else { Line::Text }));
        }
    }
}

struct Pending {
    record: Record,
    tx: oneshot::Sender<String>,
}

/// A coordinator preview: its owner and the window record at registration.
struct PreviewEntry {
    /// The order of registration (the oldest has the smallest).
    seq: u64,
    owner: PreviewOwner,
    record: Record,
}

#[derive(Default)]
struct State {
    signature: Option<String>,
    runtime_dir: Option<PathBuf>,
    records: HashMap<String, Record>,
    /// The coordinator previews by window id. They live and end with the
    /// window records: every place that clears the records clears them.
    previews: HashMap<String, PreviewEntry>,
    preview_seq: u64,
    /// Launches that wait for their window, by class.
    pending: HashMap<String, Pending>,
    task: Option<tokio::task::JoinHandle<()>>,
}

struct Inner {
    state: Mutex<State>,
    connected: watch::Sender<bool>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic cannot leave the maps half changed: recover the guard.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The records, the pending launches and the event task of one desktop.
#[derive(Clone)]
pub struct Events {
    inner: Arc<Inner>,
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

impl Events {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                connected: watch::channel(false).0,
            }),
        }
    }

    /// Starts the event task for `signature`, or keeps the running one. A new
    /// signature or a new runtime folder clears all records.
    pub fn ensure(&self, runtime_dir: &Path, signature: &str) {
        let mut st = self.inner.lock();
        let same = st.signature.as_deref() == Some(signature)
            && st.runtime_dir.as_deref() == Some(runtime_dir)
            && st.task.as_ref().is_some_and(|t| !t.is_finished());
        if same {
            return;
        }
        if let Some(task) = st.task.take() {
            task.abort();
        }
        st.records.clear();
        st.previews.clear();
        st.signature = Some(signature.to_owned());
        st.runtime_dir = Some(runtime_dir.to_owned());
        self.inner.connected.send_replace(false);
        let inner = Arc::clone(&self.inner);
        let socket = runtime_dir
            .join("hypr")
            .join(signature)
            .join(".socket2.sock");
        st.task = Some(tokio::spawn(run_task(inner, socket)));
    }

    /// Waits until the event socket is connected.
    pub async fn wait_connected(&self, limit: Duration) -> bool {
        let mut rx = self.inner.connected.subscribe();
        tokio::time::timeout(limit, rx.wait_for(|up| *up))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Registers a launch: its window is the first `openwindow` of
    /// `template.class`. A wall for a display that has a record or a pending
    /// launch is a `conflict`.
    ///
    /// # Errors
    /// `conflict`, or `failed` when the events are not running.
    pub fn register(&self, mut template: Record) -> Result<oneshot::Receiver<String>, Fail> {
        let mut st = self.inner.lock();
        let Some(signature) = st.signature.clone() else {
            return Err(Fail::new(
                code::FAILED,
                "the desktop events are not running",
            ));
        };
        if template.kind == Kind::Wall {
            let taken = |r: &Record| r.kind == Kind::Wall && r.display_id == template.display_id;
            if st.records.values().any(taken) || st.pending.values().any(|p| taken(&p.record)) {
                return Err(Fail::new(
                    code::CONFLICT,
                    "This display already has a wall window; close it first.",
                ));
            }
        }
        template.signature = signature;
        let (tx, rx) = oneshot::channel();
        st.pending.insert(
            template.class.clone(),
            Pending {
                record: template,
                tx,
            },
        );
        Ok(rx)
    }

    /// Forgets a pending launch.
    pub fn cancel(&self, class: &str) {
        self.inner.lock().pending.remove(class);
    }

    /// The record of `address`.
    #[must_use]
    pub fn record(&self, address: &str) -> Option<Record> {
        self.inner.lock().records.get(address).cloned()
    }

    /// Registers the preview `window_id` for the window at `address`: a
    /// copy of the record of that window and its owner. At most
    /// [`MAX_PREVIEWS`] previews are kept; the oldest goes first.
    ///
    /// # Errors
    /// `not_found` when no window record exists for `address`.
    pub fn register_preview(
        &self,
        window_id: &str,
        owner: PreviewOwner,
        address: &str,
    ) -> Result<(), Fail> {
        let mut st = self.inner.lock();
        let Some(record) = st.records.get(address).cloned() else {
            return Err(Fail::new(code::NOT_FOUND, "The window is gone."));
        };
        if st.previews.len() >= MAX_PREVIEWS && !st.previews.contains_key(window_id) {
            let oldest = st
                .previews
                .iter()
                .min_by_key(|(_, p)| p.seq)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                st.previews.remove(&id);
            }
        }
        st.preview_seq += 1;
        let seq = st.preview_seq;
        st.previews
            .insert(window_id.to_owned(), PreviewEntry { seq, owner, record });
        Ok(())
    }

    /// The owner and the window record of the preview `window_id`.
    #[must_use]
    pub fn preview(&self, window_id: &str) -> Option<(PreviewOwner, Record)> {
        self.inner
            .lock()
            .previews
            .get(window_id)
            .map(|p| (p.owner.clone(), p.record.clone()))
    }

    /// The wall record of `display_id`.
    #[must_use]
    pub fn wall(&self, display_id: &str) -> Option<Record> {
        self.inner
            .lock()
            .records
            .values()
            .find(|r| r.kind == Kind::Wall && r.display_id.as_deref() == Some(display_id))
            .cloned()
    }

    /// The wall windows, sorted by display id.
    #[must_use]
    pub fn walls(&self) -> Vec<WallWindow> {
        let mut walls: Vec<WallWindow> = self
            .inner
            .lock()
            .records
            .values()
            .filter(|r| r.kind == Kind::Wall)
            .map(|r| WallWindow {
                display_id: r.display_id.clone().unwrap_or_default(),
                monitor: r.monitor.clone(),
                address: r.address.clone(),
            })
            .collect();
        walls.sort_by(|a, b| a.display_id.cmp(&b.display_id));
        walls
    }

    /// Deletes the record of `address` (and the profile of a preview).
    pub fn forget(&self, address: &str) {
        self.inner.forget(address);
    }

    /// Waits until the record of `address` is gone; `false` at the limit.
    pub async fn wait_gone(&self, address: &str, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if self.record(address).is_none() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl Inner {
    fn forget(&self, address: &str) {
        let (record, runtime_dir) = {
            let mut st = self.lock();
            st.previews.retain(|_, p| p.record.address != address);
            (st.records.remove(address), st.runtime_dir.clone())
        };
        if let (Some(record), Some(runtime_dir)) = (record, runtime_dir)
            && record.kind == Kind::Preview
        {
            remove_preview_profile(&runtime_dir, &record.launch_id);
        }
    }

    fn handle(&self, event: Event) {
        match event {
            Event::Open { address, class } => {
                let mut st = self.lock();
                if let Some(p) = st.pending.remove(&class) {
                    let mut record = p.record;
                    record.address.clone_from(&address);
                    st.records.insert(address.clone(), record);
                    if p.tx.send(address.clone()).is_err() {
                        // The launch gave up: do not own the window.
                        st.records.remove(&address);
                    }
                }
            }
            Event::Close { address } => self.forget(&address),
        }
    }

    fn clear(&self) {
        let mut st = self.lock();
        st.records.clear();
        st.previews.clear();
    }
}

/// The folder that holds the profile of a preview.
#[must_use]
pub fn preview_parent(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("fleetdeck")
}

/// The profile folder name of a launch id.
#[must_use]
pub fn preview_name(launch_id: &str) -> String {
    format!("browser-{launch_id}")
}

/// Deletes the profile of a preview in the background (it can be large).
pub fn remove_preview_profile(runtime_dir: &Path, launch_id: &str) {
    let parent = preview_parent(runtime_dir);
    let name = preview_name(launch_id);
    let work = move || match profile::remove(&parent, &name) {
        Ok(()) => {}
        Err(profile::ProfileError::NotPlain) => {
            tracing::warn!("a preview profile is not a plain folder; it was not removed");
        }
        Err(profile::ProfileError::Io(e)) => {
            tracing::warn!(error = %e, "a preview profile was not removed");
        }
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        drop(handle.spawn_blocking(work));
    } else {
        work();
    }
}

async fn run_task(inner: Arc<Inner>, socket: PathBuf) {
    // The folder of the instance: while it exists, the desktop exists.
    let instance_dir = socket.parent().map(Path::to_owned);
    loop {
        inner.clear();
        if instance_dir.as_ref().is_none_or(|d| !d.is_dir()) {
            inner.connected.send_replace(false);
            return;
        }
        match UnixStream::connect(&socket).await {
            Ok(stream) => {
                inner.connected.send_replace(true);
                read_events(&inner, stream).await;
                inner.connected.send_replace(false);
            }
            Err(e) => tracing::debug!(error = %e, "the Hyprland event socket does not connect"),
        }
        // A gap in the stream: the station owns no window it might confuse.
        inner.clear();
        tokio::time::sleep(RECONNECT_PAUSE).await;
    }
}

async fn read_events(inner: &Inner, stream: UnixStream) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        match next_line(&mut reader, &mut buf).await {
            Ok(Some(Line::Text)) => {
                if let Some(event) = std::str::from_utf8(&buf).ok().and_then(parse_event) {
                    inner.handle(event);
                }
            }
            Ok(Some(Line::TooLong)) => tracing::debug!("a long event line was skipped"),
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(error = %e, "the Hyprland event socket failed");
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
