//! Tests of the relay of a maintenance stream to the runner socket.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::files::{self, RunStateKind};
use super::frame::{Frame, read_frame, write_frame};
use super::relay::{RelayPort, serve_port};
use super::runner_client::connect;
use super::service::MaintenanceService;
use super::service_support::{FakeHost, FakePlans, Launch};
use super::test_support::{
    ID, POST_OK, RUNNER_TOKEN, STATION_TOKEN, contains, private_dir, spec, start,
};

const WAIT: Duration = Duration::from_secs(30);

/// The board side: what the relay wrote and how it ended.
#[derive(Default)]
pub(super) struct FakePort {
    output: Mutex<Vec<u8>>,
    pub(super) closed: AtomicBool,
    closes: AtomicUsize,
    resets: Mutex<Vec<String>>,
    consumed: AtomicUsize,
}

#[async_trait]
impl RelayPort for FakePort {
    async fn write(&self, bytes: Vec<u8>) -> bool {
        self.output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(bytes);
        true
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.closes.fetch_add(1, Ordering::SeqCst);
    }

    async fn reset(&self, reason: &str) {
        self.resets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(reason.to_owned());
    }

    async fn consumed(&self, n: usize) {
        self.consumed.fetch_add(n, Ordering::SeqCst);
    }
}

impl FakePort {
    pub(super) fn text(&self) -> Vec<u8> {
        self.output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) async fn until_output(&self, needle: &str) {
        timeout(WAIT, async {
            while !contains(&self.text(), needle.as_bytes()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    pub(super) async fn until_closed(&self) {
        timeout(WAIT, async {
            while !self.closed.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}

fn service(dir: &Path) -> Arc<MaintenanceService> {
    Arc::new(MaintenanceService::new(
        dir.to_owned(),
        "/opt/fleetdeck".into(),
        FakeHost::new(Launch::Fail),
        FakePlans::new(),
    ))
}

/// Waits until the runner wrote the state `running`.
async fn running(dir: &Path) {
    timeout(WAIT, async {
        while !matches!(
            files::read_state(dir, ID),
            Ok(Some(s)) if s.state == RunStateKind::Running
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

pub(super) struct Relay {
    pub(super) port: Arc<FakePort>,
    pub(super) board: mpsc::UnboundedSender<Vec<u8>>,
    pub(super) task: JoinHandle<()>,
}

/// Starts the relay of the run `id`.
pub(super) fn relay_of(svc: &Arc<MaintenanceService>, id: &str) -> Relay {
    let port = Arc::new(FakePort::default());
    let (board, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn({
        let (svc, port, id) = (Arc::clone(svc), Arc::clone(&port), id.to_owned());
        async move { serve_port(&svc, port.as_ref(), &id, rx).await }
    });
    Relay { port, board, task }
}

fn relay(svc: &Arc<MaintenanceService>) -> Relay {
    relay_of(svc, ID)
}

#[tokio::test]
async fn maintenance_relay_output_reaches_the_board_and_board_bytes_reach_the_pty() {
    let run = start("echo hello-board; read -r x; echo got-$x", POST_OK);
    running(run.path()).await;
    let svc = service(run.path());
    let relay = relay(&svc);
    relay.port.until_output("hello-board").await;
    relay.board.send(b"abc\n".to_vec()).unwrap();
    relay.port.until_output("got-abc").await;
    // The exit ends the stream.
    relay.port.until_closed().await;
    drop(relay.board);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
    assert!(relay.port.resets.lock().unwrap().is_empty());
    // The bytes were given back to the board.
    assert_eq!(relay.port.consumed.load(Ordering::SeqCst), 4);
    let (dir, code) = run.finished().await;
    assert_eq!(code, 0);
    drop(dir);
}

#[tokio::test]
async fn maintenance_relay_the_ring_comes_first_for_a_late_board() {
    let run = start("echo early-line; read -r x; echo got-$x", POST_OK);
    running(run.path()).await;
    // Wait until the line is in the ring: a first relay sees it.
    let svc = service(run.path());
    let first = relay(&svc);
    first.port.until_output("early-line").await;
    drop(first.board);
    timeout(WAIT, first.task).await.unwrap().unwrap();
    let second = relay(&svc);
    second.port.until_output("early-line").await;
    second.board.send(b"y\n".to_vec()).unwrap();
    second.port.until_output("got-y").await;
    second.port.until_closed().await;
    run.finished().await;
}

/// A fake runner that sends `output`, then `Exit`, and keeps the Input
/// frames that arrive on the connection.
fn fake_runner(dir: &Path, output: &'static str) -> JoinHandle<Vec<Frame>> {
    let listener = UnixListener::bind(files::socket_path(dir, ID)).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let hello = read_frame(&mut stream).await.unwrap();
        assert_eq!(hello, Frame::Hello(STATION_TOKEN.into()));
        write_frame(&mut stream, &Frame::HelloOk(RUNNER_TOKEN.into()))
            .await
            .unwrap();
        write_frame(&mut stream, &Frame::SnapshotEnd).await.unwrap();
        write_frame(&mut stream, &Frame::Output(output.into()))
            .await
            .unwrap();
        write_frame(&mut stream, &Frame::Exit(3)).await.unwrap();
        let mut frames = Vec::new();
        timeout(WAIT, async {
            while let Ok(frame) = read_frame(&mut stream).await {
                frames.push(frame);
            }
        })
        .await
        .unwrap();
        frames
    })
}

#[tokio::test]
async fn maintenance_relay_after_exit_board_bytes_are_dropped_and_never_sent() {
    let dir = private_dir();
    files::write_spec(dir.path(), &spec(ID)).unwrap();
    files::write_state(
        dir.path(),
        ID,
        &files::RunState {
            v: 1,
            state: RunStateKind::Running,
            exit_code: None,
            post_check: None,
            at: 1,
        },
    )
    .unwrap();
    let runner = fake_runner(dir.path(), "last words");
    let svc = service(dir.path());
    let relay = relay(&svc);
    relay.port.until_output("last words").await;
    relay.port.until_closed().await;
    // The runner ended: the board bytes that come now go nowhere.
    relay.board.send(b"late-password\n".to_vec()).unwrap();
    let before = relay.port.consumed.load(Ordering::SeqCst);
    timeout(WAIT, async {
        while relay.port.consumed.load(Ordering::SeqCst) == before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(relay.port.consumed.load(Ordering::SeqCst), 14);
    drop(relay.board);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
    let frames = timeout(WAIT, runner).await.unwrap().unwrap();
    assert!(frames.is_empty(), "{frames:?}");
}

#[tokio::test]
async fn maintenance_relay_an_exited_run_sends_its_output_file_and_ends() {
    let dir = private_dir();
    files::write_spec(dir.path(), &spec(ID)).unwrap();
    files::write_output(dir.path(), ID, b"stored output").unwrap();
    files::write_state(
        dir.path(),
        ID,
        &files::RunState {
            v: 1,
            state: RunStateKind::Exited,
            exit_code: Some(0),
            post_check: None,
            at: 1,
        },
    )
    .unwrap();
    let svc = service(dir.path());
    let relay = relay(&svc);
    relay.port.until_closed().await;
    assert_eq!(relay.port.text(), b"stored output");
    // The input of the board is dropped.
    relay.board.send(b"input\n".to_vec()).unwrap();
    drop(relay.board);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
    assert_eq!(relay.port.consumed.load(Ordering::SeqCst), 6);
    assert_eq!(relay.port.text(), b"stored output");
}

#[tokio::test]
async fn maintenance_relay_a_lost_run_without_an_output_file_ends_at_once() {
    let dir = private_dir();
    files::write_spec(dir.path(), &spec(ID)).unwrap();
    files::write_state(
        dir.path(),
        ID,
        &files::RunState {
            v: 1,
            state: RunStateKind::Lost,
            exit_code: None,
            post_check: None,
            at: 1,
        },
    )
    .unwrap();
    let svc = service(dir.path());
    let relay = relay(&svc);
    relay.port.until_closed().await;
    assert!(relay.port.text().is_empty());
    drop(relay.board);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
}

#[tokio::test]
async fn maintenance_relay_an_unknown_id_resets_the_stream() {
    let dir = private_dir();
    let svc = service(dir.path());
    let relay = relay(&svc);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
    assert_eq!(*relay.port.resets.lock().unwrap(), ["unknown_maintenance"]);
    assert!(!relay.port.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn maintenance_relay_the_end_of_the_board_stream_leaves_the_runner_running() {
    let run = start("read -r x; echo got-$x", POST_OK);
    running(run.path()).await;
    let svc = service(run.path());
    let relay = relay(&svc);
    // The relay is connected when it is registered.
    timeout(WAIT, async {
        while svc.relay_sender(ID).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(relay.board);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
    assert!(svc.relay_sender(ID).is_none());
    // The state is still `running` and the child is alive: a new client
    // finishes it.
    let state = files::read_state(run.path(), ID).unwrap().unwrap();
    assert_eq!(state.state, RunStateKind::Running);
    let mut client = connect(run.path(), &spec(ID)).await.unwrap();
    client.input(b"z\n").await.unwrap();
    let mut output = Vec::new();
    let code = timeout(WAIT, async {
        loop {
            match client.next().await.unwrap() {
                Frame::Output(bytes) => output.extend(bytes),
                Frame::Exit(code) => return code,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(code, 0);
    assert!(contains(&output, b"got-z"));
    run.finished().await;
}

#[tokio::test]
async fn maintenance_relay_the_end_of_the_board_stream_closes_the_station_half_once() {
    let run = start("read -r x; echo got-$x", POST_OK);
    running(run.path()).await;
    let svc = service(run.path());
    let relay = relay(&svc);
    timeout(WAIT, async {
        while svc.relay_sender(ID).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(relay.board);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
    // The relay closed its half exactly once, and the runner still runs.
    assert_eq!(relay.port.closes.load(Ordering::SeqCst), 1);
    let state = files::read_state(run.path(), ID).unwrap().unwrap();
    assert_eq!(state.state, RunStateKind::Running);
    let mut client = connect(run.path(), &spec(ID)).await.unwrap();
    client.input(b"q\n").await.unwrap();
    timeout(WAIT, async {
        while !matches!(client.next().await.unwrap(), Frame::Exit(_)) {}
    })
    .await
    .unwrap();
    run.finished().await;
}

#[tokio::test]
async fn maintenance_relay_a_newer_relay_replaces_the_older_one() {
    let run = start("read -r x; echo got-$x", POST_OK);
    running(run.path()).await;
    let svc = service(run.path());
    let older = relay(&svc);
    timeout(WAIT, async {
        while svc.relay_sender(ID).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let newer = relay(&svc);
    // The older relay ends its stream, and drops the bytes of its board until
    // that stream ends; the newer one serves.
    older.port.until_closed().await;
    drop(older.board);
    timeout(WAIT, older.task).await.unwrap().unwrap();
    newer.board.send(b"n\n".to_vec()).unwrap();
    newer.port.until_output("got-n").await;
    newer.port.until_closed().await;
    run.finished().await;
}

#[tokio::test]
async fn maintenance_relay_a_connection_that_the_runner_cut_off_ends_the_stream() {
    // A fake runner closes the connection after the handshake: the relay
    // ends the stream and leaves the state alone.
    let dir = private_dir();
    files::write_spec(dir.path(), &spec(ID)).unwrap();
    files::write_state(
        dir.path(),
        ID,
        &files::RunState {
            v: 1,
            state: RunStateKind::Running,
            exit_code: None,
            post_check: None,
            at: 1,
        },
    )
    .unwrap();
    let listener = UnixListener::bind(files::socket_path(dir.path(), ID)).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_frame(&mut stream).await.unwrap();
        write_frame(&mut stream, &Frame::HelloOk(RUNNER_TOKEN.into()))
            .await
            .unwrap();
        // The runner cuts the connection off.
    });
    let svc = service(dir.path());
    let relay = relay(&svc);
    relay.port.until_closed().await;
    drop(relay.board);
    timeout(WAIT, relay.task).await.unwrap().unwrap();
    server.await.unwrap();
    let state = files::read_state(dir.path(), ID).unwrap().unwrap();
    assert_eq!(state.state, RunStateKind::Running);
}
