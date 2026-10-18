//! The registry of the board sockets and the shell-attach sockets (R13 P7
//! §7.6, "Sockets (one admission step)").
//!
//! One mutex guards one map. The guard adds an entry (state `Pending`) for
//! an upgrade before any await of the handler, and the handler makes it
//! `Live` at `on_upgrade` under the same mutex ([`Registry::go_live`]). A
//! sign-out sets `revoked` on every entry of the deleted sessions under that
//! mutex ([`Registry::revoke`]), so an entry is either revoked before it goes
//! live (it never relays) or live when it is revoked (its relay ends and
//! signals `done`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

/// The kind of browser session of a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// An owner login session (`login_sessions`).
    Owner,
    /// A member session (`member_sessions`).
    Member,
}

/// The kind of a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketKind {
    /// The board WebSocket (`GET /api/ws`).
    Board,
    /// A shell-attach socket (`.../shells/{shell}/attach`).
    Shell,
}

/// The state of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    /// Registered by the guard; the handler has not started its relay.
    Pending,
    /// The relay runs.
    Live,
}

/// A flag that an async task can wait for: an [`AtomicBool`] and a
/// [`Notify`].
#[derive(Debug, Default)]
pub struct Signal {
    set: AtomicBool,
    notify: Notify,
}

impl Signal {
    /// Sets the flag and wakes every waiter. Setting it again changes
    /// nothing.
    pub fn fire(&self) {
        self.set.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    #[must_use]
    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    /// Returns when the flag is set (at once when it is set already).
    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Enable before the check, so a `fire` between the check and the
            // await is not lost.
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

/// The handle of one registered socket: the guard puts it in a request
/// extension, and the handler gives it to [`Registry::go_live`] and
/// [`Registry::remove`], and watches [`Ticket::revoked`]. The entry leaves
/// the registry when the last clone of the ticket drops, so a request that
/// ends before its upgrade leaves nothing behind.
#[derive(Debug, Clone)]
pub struct Ticket(Arc<TicketShared>);

#[derive(Debug)]
struct TicketShared {
    id: u64,
    session_hash: String,
    kind: SessionKind,
    revoked: Arc<Signal>,
    done: Arc<Signal>,
    registry: Arc<Registry>,
}

impl Drop for TicketShared {
    fn drop(&mut self) {
        self.registry.remove_id(self.id);
        self.done.fire();
    }
}

impl Ticket {
    /// The SHA-256 of the cookie value of the session of this socket.
    #[must_use]
    pub fn session_hash(&self) -> &str {
        &self.0.session_hash
    }

    #[must_use]
    pub fn kind(&self) -> SessionKind {
        self.0.kind
    }

    /// True once a sign-out revoked this socket. A relay checks it before it
    /// forwards each frame.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.0.revoked.is_set()
    }

    /// Returns when this socket is revoked.
    pub async fn revoked(&self) {
        self.0.revoked.wait().await;
    }
}

/// What [`Registry::revoke`] returns for each revoked entry: [`wait_all`]
/// waits until its relay ended.
#[derive(Debug, Clone)]
pub struct Waiter {
    done: Arc<Signal>,
}

/// One registered socket.
#[derive(Debug)]
struct Entry {
    session_hash: String,
    kind: SessionKind,
    socket: SocketKind,
    addr: String,
    state: SocketState,
    revoked: Arc<Signal>,
    done: Arc<Signal>,
}

/// What a test or a log may read of an entry (never a hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    pub kind: SessionKind,
    pub socket: SocketKind,
    pub addr: String,
    pub state: SocketState,
    pub revoked: bool,
}

#[derive(Debug, Default)]
struct Map {
    next_id: u64,
    entries: HashMap<u64, Entry>,
}

/// The registry. One per deck store.
#[derive(Debug, Default)]
pub struct Registry {
    map: Mutex<Map>,
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Map> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a `Pending` entry. `addr` is the canonical peer address of the
    /// connection. Call it in the guard, before any await of the handler.
    pub fn register(
        self: &Arc<Self>,
        session_hash: &str,
        kind: SessionKind,
        socket: SocketKind,
        addr: &str,
    ) -> Ticket {
        let mut map = self.lock();
        map.next_id += 1;
        let id = map.next_id;
        let revoked = Arc::new(Signal::default());
        let done = Arc::new(Signal::default());
        map.entries.insert(
            id,
            Entry {
                session_hash: session_hash.to_owned(),
                kind,
                socket,
                addr: addr.to_owned(),
                state: SocketState::Pending,
                revoked: Arc::clone(&revoked),
                done: Arc::clone(&done),
            },
        );
        Ticket(Arc::new(TicketShared {
            id,
            session_hash: session_hash.to_owned(),
            kind,
            revoked,
            done,
            registry: Arc::clone(self),
        }))
    }

    /// Makes the entry `Live` and returns true, under the mutex of the
    /// registry; false when the entry is revoked or `session_exists` says
    /// that the session row is gone. The caller closes the socket at once
    /// on false. `session_exists` must be the answer of a lookup that began
    /// after [`Self::register`].
    pub fn go_live(&self, ticket: &Ticket, session_exists: impl FnOnce() -> bool) -> bool {
        let mut map = self.lock();
        if ticket.is_revoked() {
            return false;
        }
        let Some(entry) = map.entries.get_mut(&ticket.0.id) else {
            return false;
        };
        if !session_exists() {
            return false;
        }
        entry.state = SocketState::Live;
        true
    }

    /// The sessions of every entry (pending or live) whose address is in
    /// `addrs`, each session once, under the mutex. The attempted
    /// transaction of an input operation deletes these sessions with the
    /// sessions of the same addresses ([`crate::store::DeckStore::desktop_op_attempted_input`]).
    #[must_use]
    pub fn sessions_at(&self, addrs: &[String]) -> Vec<(SessionKind, String)> {
        let map = self.lock();
        let mut out: Vec<(SessionKind, String)> = Vec::new();
        for entry in map.entries.values() {
            if !addrs.contains(&entry.addr) {
                continue;
            }
            let session = (entry.kind, entry.session_hash.clone());
            if !out.contains(&session) {
                out.push(session);
            }
        }
        out
    }

    /// Sets `revoked` on every entry (pending or live) of a session in
    /// `sessions` or with an address in `addrs`, under the mutex. Returns
    /// their waiters and the session of every revoked entry (each session
    /// once). A pending entry has no relay: it signals `done` at once, and
    /// [`Self::go_live`] refuses it from now on.
    #[must_use]
    pub fn revoke(
        &self,
        sessions: &[String],
        addrs: &[String],
    ) -> (Vec<Waiter>, Vec<(SessionKind, String)>) {
        let map = self.lock();
        let mut waiters = Vec::new();
        let mut hit_sessions: Vec<(SessionKind, String)> = Vec::new();
        for entry in map.entries.values() {
            let hit = sessions.contains(&entry.session_hash) || addrs.contains(&entry.addr);
            if !hit {
                continue;
            }
            entry.revoked.fire();
            if entry.state == SocketState::Pending {
                entry.done.fire();
            }
            waiters.push(Waiter {
                done: Arc::clone(&entry.done),
            });
            let session = (entry.kind, entry.session_hash.clone());
            if !hit_sessions.contains(&session) {
                hit_sessions.push(session);
            }
        }
        (waiters, hit_sessions)
    }

    /// Removes the entry at the end of the socket and signals `done`. Call
    /// it after the relay can no longer forward a frame.
    pub fn remove(&self, ticket: &Ticket) {
        self.remove_id(ticket.0.id);
        ticket.0.done.fire();
    }

    fn remove_id(&self, id: u64) {
        self.lock().entries.remove(&id);
    }

    /// The entries now (for tests and diagnostics).
    #[must_use]
    pub fn entries(&self) -> Vec<EntryInfo> {
        self.lock()
            .entries
            .values()
            .map(|e| EntryInfo {
                kind: e.kind,
                socket: e.socket,
                addr: e.addr.clone(),
                state: e.state,
                revoked: e.revoked.is_set(),
            })
            .collect()
    }
}

/// True when every relay of `waiters` signalled `done` within `limit`.
pub async fn wait_all(waiters: Vec<Waiter>, limit: Duration) -> bool {
    let all = async {
        for w in &waiters {
            w.done.wait().await;
        }
    };
    tokio::time::timeout(limit, all).await.is_ok()
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
