//! The socket side of the runner: one authenticated client at a time, the
//! ring first and then the live output, and the input to the PTY.
//!
//! The PTY drain (`Ctx::push_output`, called by the reader thread) never
//! waits for a client: it uses `try_send` on a bounded queue, and a full
//! queue drops that client.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fleetdeck_core::secret::ct_eq;
use fleetdeck_proto::desktop_tools::size_ok;
use portable_pty::{MasterPty, PtySize};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::timeout;

use super::frame::{Frame, read_frame, write_frame};
use super::ring::Ring;

/// The frames that one client can have in its queue.
pub(super) const QUEUE_FRAMES: usize = 256;
/// The longest wait for the first frame of a connection.
const HELLO_WAIT: Duration = Duration::from_secs(5);
/// The longest wait for the last frames to reach the client.
const FLUSH_WAIT: Duration = Duration::from_secs(2);
/// The most connections that wait for their handshake at the same time.
const HANDSHAKES: usize = 8;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The client that gets the live output.
struct Client {
    ticket: u64,
    tx: mpsc::Sender<Frame>,
    /// Dropped with the client: the connection closes.
    cancel: oneshot::Sender<()>,
    /// Resolves when the connection task ends.
    done: oneshot::Receiver<()>,
}

struct Shared {
    ring: Ring,
    client: Option<Client>,
    next_ticket: u64,
    /// The exit code, once the child ended.
    exit: Option<i32>,
}

/// What a connection task needs after the handshake.
struct Attach {
    ticket: u64,
    snapshot: Vec<u8>,
    rx: mpsc::Receiver<Frame>,
    cancelled: oneshot::Receiver<()>,
    done_tx: oneshot::Sender<()>,
}

/// The state that the reader thread and the connections share.
pub(super) struct Ctx {
    shared: Mutex<Shared>,
    /// True while the child runs. Checked right before each PTY write.
    running: AtomicBool,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    uid: u32,
    station_token: String,
    runner_token: String,
    handshakes: Arc<Semaphore>,
}

impl Ctx {
    pub(super) fn new(
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        uid: u32,
        station_token: String,
        runner_token: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            shared: Mutex::new(Shared {
                ring: Ring::default(),
                client: None,
                next_ticket: 0,
                exit: None,
            }),
            running: AtomicBool::new(true),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(master),
            uid,
            station_token,
            runner_token,
            handshakes: Arc::new(Semaphore::new(HANDSHAKES)),
        })
    }

    /// Appends PTY output to the ring and to the queue of the client. A
    /// full queue drops the client; the caller never waits.
    pub(super) fn push_output(&self, bytes: &[u8]) {
        let mut shared = lock(&self.shared);
        shared.ring.push(bytes);
        let full = shared
            .client
            .as_ref()
            .is_some_and(|c| c.tx.try_send(Frame::Output(bytes.to_vec())).is_err());
        if full {
            // The drop of the client closes its connection.
            shared.client = None;
        }
    }

    /// The child ended: no more input and no more resize.
    pub(super) fn stop_input(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// The ring as it is now.
    pub(super) fn ring_snapshot(&self) -> Vec<u8> {
        lock(&self.shared).ring.snapshot()
    }

    /// Records the exit code, sends `Exit` to the current client and waits
    /// a short time until the connection has written it.
    pub(super) async fn finish(&self, code: i32) {
        let client = {
            let mut shared = lock(&self.shared);
            shared.exit = Some(code);
            shared.client.take()
        };
        if let Some(client) = client {
            let Client {
                tx, cancel, done, ..
            } = client;
            // A full queue means a slow client: it gets no `Exit`.
            let _ = tx.try_send(Frame::Exit(code));
            drop(tx);
            // The wait ends when the connection task ends or after the limit.
            let _ = timeout(FLUSH_WAIT, done).await;
            drop(cancel);
        }
    }

    fn attach(&self) -> Attach {
        let (tx, rx) = mpsc::channel(QUEUE_FRAMES);
        let (cancel, cancelled) = oneshot::channel();
        let (done_tx, done) = oneshot::channel();
        let mut shared = lock(&self.shared);
        shared.next_ticket += 1;
        let ticket = shared.next_ticket;
        let snapshot = shared.ring.snapshot();
        if let Some(code) = shared.exit {
            // The child ended already: the client reads the ring and `Exit`.
            let _ = tx.try_send(Frame::Exit(code));
        } else {
            // The old client, if any, is dropped here and its connection closes.
            shared.client = Some(Client {
                ticket,
                tx,
                cancel,
                done,
            });
        }
        Attach {
            ticket,
            snapshot,
            rx,
            cancelled,
            done_tx,
        }
    }

    fn detach(&self, ticket: u64) {
        let mut shared = lock(&self.shared);
        if shared.client.as_ref().is_some_and(|c| c.ticket == ticket) {
            shared.client = None;
        }
    }

    fn write_input(&self, bytes: &[u8]) {
        let mut writer = lock(&self.writer);
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        if let Some(w) = writer.as_mut()
            && w.write_all(bytes).and_then(|()| w.flush()).is_err()
        {
            eprintln!("maintenance: the input did not reach the terminal");
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        if !size_ok(cols, rows) || !self.running.load(Ordering::SeqCst) {
            return;
        }
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        if lock(&self.master).resize(size).is_err() {
            eprintln!("maintenance: the terminal size did not change");
        }
    }
}

/// Accepts connections until the task is aborted.
pub(super) async fn accept_loop(listener: UnixListener, ctx: Arc<Ctx>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                // Too many open handshakes: the connection is closed at once.
                if let Ok(permit) = Arc::clone(&ctx.handshakes).try_acquire_owned() {
                    tokio::spawn(connection(stream, Arc::clone(&ctx), permit));
                }
            }
            Err(_) => {
                eprintln!("maintenance: accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(mut stream: UnixStream, ctx: Arc<Ctx>, permit: OwnedSemaphorePermit) {
    let Some(attach) = handshake(&mut stream, &ctx).await else {
        return;
    };
    drop(permit);
    let (read, mut write) = stream.into_split();
    let Attach {
        ticket,
        snapshot,
        mut rx,
        cancelled,
        done_tx,
    } = attach;
    let sending = async {
        if !snapshot.is_empty() {
            write_frame(&mut write, &Frame::Output(snapshot)).await?;
        }
        write_frame(&mut write, &Frame::SnapshotEnd).await?;
        while let Some(frame) = rx.recv().await {
            write_frame(&mut write, &frame).await?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::select! {
        _ = sending => {}
        () = receiving(read, &ctx) => {}
        _ = cancelled => {}
    }
    ctx.detach(ticket);
    drop(done_tx);
}

/// The checks of a new connection and the answer. `None`: close it.
async fn handshake(stream: &mut UnixStream, ctx: &Arc<Ctx>) -> Option<Attach> {
    if stream.peer_cred().ok()?.uid() != ctx.uid {
        return None;
    }
    let first = timeout(HELLO_WAIT, read_frame(stream)).await.ok()?.ok()?;
    let Frame::Hello(token) = first else {
        return None;
    };
    if !ct_eq(&token, ctx.station_token.as_bytes()) {
        return None;
    }
    let answer = Frame::HelloOk(ctx.runner_token.as_bytes().to_vec());
    timeout(HELLO_WAIT, write_frame(stream, &answer))
        .await
        .ok()?
        .ok()?;
    Some(ctx.attach())
}

/// Reads the frames of the client. Any frame that is not `Input` or
/// `Resize`, and any read error, ends the connection.
async fn receiving(mut read: OwnedReadHalf, ctx: &Arc<Ctx>) {
    loop {
        match read_frame(&mut read).await {
            Ok(Frame::Input(bytes)) => {
                let ctx = Arc::clone(ctx);
                // A write to a full PTY can block: not on the runtime thread.
                if tokio::task::spawn_blocking(move || ctx.write_input(&bytes))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(Frame::Resize { cols, rows }) => ctx.resize(cols, rows),
            _ => return,
        }
    }
}
