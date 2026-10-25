//! The shared rig of the recording tests: the fake desktop with a fake
//! `wf-recorder` script (it writes a fixture MP4 to the retained descriptor),
//! the real store and artifact tree, a fake deck link, fake clocks and short
//! timings.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fleetdeck_proto::desktop::DesktopActResult;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::recording::{
    CaptureLeaseAnswer, CaptureLeaseRequest, LeaseDecision, LeasePhase, StopReason,
};

use super::{DeckLink, RecordParams, RecordRequest, RoomCheck, Timing};
use crate::artifacts::capture_import::tests::{FP, Rig as StoreRig, rig as store_rig};
use crate::artifacts::capture_import::{DurableFs, SyncFs};
use crate::desktop::authority::Admitted;
use crate::desktop::capture::{Allowed, Launch};
use crate::desktop::fake::{Opts, Rig as FakeRig};
use crate::desktop::{Desktop, Fail};
use crate::media::clock::{BootInstant, Clocks, boot_now};
use crate::media::mp4_fixture::Mp4Fixture;
use crate::store::Store;
use crate::store::capture_ops::{CaptureState, RecordingRow};

/// The points where a test holds a step of the operation still: the launch
/// guard (it runs on the thread of the recorder) and the phase write. The
/// registry of the desktop carries them in a test build only.
#[derive(Default)]
pub(crate) struct Hooks {
    launch: Mutex<Option<Arc<Block>>>,
    phase: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    before_insert: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    after_miss: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    after_write: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}

/// Waits for one permit of `gate`, when there is a gate.
async fn pass(gate: Option<Arc<tokio::sync::Semaphore>>) {
    if let Some(gate) = gate {
        gate.acquire().await.unwrap().forget();
    }
}

impl Hooks {
    /// The next launch guard waits in the returned block until it is opened.
    pub(crate) fn hold_launch(&self) -> Arc<Block> {
        let block = Block::new();
        *self.launch.lock().unwrap() = Some(Arc::clone(&block));
        block
    }

    /// The phase write waits until a permit is added to the returned gate.
    pub(crate) fn hold_phase_write(&self) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *self.phase.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// The operation waits for a permit just before it registers itself.
    pub(crate) fn hold_before_insert(&self) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *self.before_insert.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// A stop that left a pending stop (no live recording) waits for a
    /// permit before it writes the store latch.
    pub(crate) fn hold_after_miss(&self) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *self.after_miss.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// A stop whose store write committed waits for a permit before any
    /// later step.
    pub(crate) fn hold_after_write(&self) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *self.after_write.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    pub(super) async fn wait_after_write(&self) {
        let gate = self.after_write.lock().unwrap().clone();
        pass(gate).await;
    }

    pub(super) async fn wait_before_insert(&self) {
        let gate = self.before_insert.lock().unwrap().clone();
        pass(gate).await;
    }

    pub(super) async fn wait_after_miss(&self) {
        let gate = self.after_miss.lock().unwrap().clone();
        pass(gate).await;
    }

    pub(super) fn launch(&self) -> Option<Arc<Block>> {
        self.launch.lock().unwrap().clone()
    }

    pub(super) fn phase(&self) -> Option<Arc<tokio::sync::Semaphore>> {
        self.phase.lock().unwrap().clone()
    }
}

/// A thread that waits in [`Block::wait`] until a test opens the block.
pub(crate) struct Block {
    /// `(reached, open)`.
    state: Mutex<(bool, bool)>,
    changed: std::sync::Condvar,
}

impl Block {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((false, false)),
            changed: std::sync::Condvar::new(),
        })
    }

    /// The waiting thread: says that it is here, then waits for `open`.
    pub(super) fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
    }

    /// Lets the waiting thread go on.
    pub(crate) fn open(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }

    /// Waits until a thread is in [`Block::wait`].
    pub(crate) async fn reached(&self) {
        for _ in 0..1000 {
            if self.state.lock().unwrap().0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no thread reached the block");
    }
}

pub(crate) const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
pub(crate) const ID2: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

/// The fake recorder. Under `Launch::Direct` it has no FD 3, so it makes FD 3
/// from stdin, which is the retained file in both launches. The flag files of
/// the fake desktop folder change what it does.
fn script(dir: &Path) -> String {
    let d = dir.display();
    format!(
        "#!/bin/sh\n\
         [ -e /proc/self/fd/3 ] || exec 3>&0\n\
         {{ for a in \"$@\"; do echo \"$a\"; done; echo --; }} >> {d}/recorder.log\n\
         if [ -f {d}/ignore_int ]; then trap '' INT; else trap 'exit 0' INT; fi\n\
         [ -f {d}/recorder_fail ] && exit 3\n\
         /bin/cat {d}/fixture.mp4 >&3\n\
         echo $$ > {d}/recorder.pid\n\
         [ -f {d}/exit_early ] && exit 0\n\
         while :; do /bin/sleep 0.05; done\n"
    )
}

/// What the fake deck answers to a lease request.
#[derive(Clone)]
pub(crate) enum Reply {
    Continue,
    Save,
    Discard(StopReason),
    /// An answer for another sequence.
    WrongSequence,
    /// A wire error with this code.
    Wire(&'static str),
    /// The request itself fails.
    Transport,
    /// No answer, ever.
    Hang,
    /// The answer after a delay.
    Delay(u64, Box<Reply>),
    /// A malformed answer.
    Garbage,
}

/// A fake deck link.
pub(crate) struct Peer {
    pub requests: Mutex<Vec<CaptureLeaseRequest>>,
    default: Mutex<Reply>,
    by_phase: Mutex<Vec<(LeasePhase, Reply)>>,
    open: AtomicBool,
    current: AtomicBool,
    closed: tokio::sync::watch::Sender<bool>,
    live: AtomicUsize,
    /// The most requests that were open at the same time.
    pub(crate) peak: AtomicUsize,
}

impl Peer {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            default: Mutex::new(Reply::Continue),
            by_phase: Mutex::new(Vec::new()),
            open: AtomicBool::new(true),
            current: AtomicBool::new(true),
            closed: tokio::sync::watch::channel(false).0,
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    /// The answer to the requests of one phase.
    pub(crate) fn set_phase(&self, phase: LeasePhase, reply: Reply) {
        let mut by = self.by_phase.lock().unwrap();
        by.retain(|(p, _)| *p != phase);
        by.push((phase, reply));
    }

    /// The link ends.
    pub(crate) fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
        self.closed.send_replace(true);
    }

    /// The link is no longer the current one of its peer.
    pub(crate) fn supersede(&self) {
        self.current.store(false, Ordering::SeqCst);
    }

    /// The phases of the requests so far.
    pub(crate) fn phases(&self) -> Vec<LeasePhase> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.phase)
            .collect()
    }

    pub(crate) fn count(&self, phase: LeasePhase) -> usize {
        self.phases().iter().filter(|p| **p == phase).count()
    }

    fn reply_for(&self, phase: LeasePhase) -> Reply {
        self.by_phase
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| *p == phase)
            .map_or_else(|| self.default.lock().unwrap().clone(), |(_, r)| r.clone())
    }
}

/// A small step type: the answer, a hang, or a delay.
enum Step {
    Done(Result<Outcome, String>),
    Hang,
    Delay(u64, Reply),
}

fn answer(sequence: u64, decision: LeaseDecision, reason: Option<StopReason>) -> Outcome {
    Outcome::Ok(
        serde_json::to_value(CaptureLeaseAnswer {
            sequence,
            decision,
            reason,
        })
        .unwrap(),
    )
}

fn respond(sequence: u64, reply: &Reply) -> Step {
    match reply {
        Reply::Continue => Step::Done(Ok(answer(sequence, LeaseDecision::Continue, None))),
        Reply::Save => Step::Done(Ok(answer(
            sequence,
            LeaseDecision::Save,
            Some(StopReason::OwnerStop),
        ))),
        Reply::Discard(r) => Step::Done(Ok(answer(sequence, LeaseDecision::Discard, Some(*r)))),
        Reply::WrongSequence => {
            Step::Done(Ok(answer(sequence + 1000, LeaseDecision::Continue, None)))
        }
        Reply::Wire(code) => Step::Done(Ok(Outcome::Err(WireError::new(*code, "refused")))),
        Reply::Transport => Step::Done(Err("the link is down".to_owned())),
        Reply::Hang => Step::Hang,
        Reply::Delay(ms, then) => Step::Delay(*ms, (**then).clone()),
        Reply::Garbage => Step::Done(Ok(Outcome::Ok(serde_json::json!({"nonsense": true})))),
    }
}

/// Counts a request out when its future ends or is dropped.
struct Open<'a>(&'a AtomicUsize);

impl Drop for Open<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl DeckLink for Peer {
    fn open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    fn current(&self) -> bool {
        self.current.load(Ordering::SeqCst)
    }

    async fn closed(&self) {
        let mut rx = self.closed.subscribe();
        // `wait_for` returns at once when the value is already true.
        let _ = rx.wait_for(|closed| *closed).await;
    }

    async fn lease(&self, req: &CaptureLeaseRequest) -> Result<Outcome, String> {
        self.requests.lock().unwrap().push(req.clone());
        let open = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(open, Ordering::SeqCst);
        let _guard = Open(&self.live);
        let mut reply = self.reply_for(req.phase);
        loop {
            match respond(req.sequence, &reply) {
                Step::Done(result) => return result,
                Step::Hang => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    return Err("hung".to_owned());
                }
                Step::Delay(ms, then) => {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    reply = then;
                }
            }
        }
    }
}

/// The boot clock plus a settable jump; the monotonic clock is real.
pub(crate) struct Jumpy(AtomicU64);

impl Jumpy {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(0)))
    }

    /// The boot clock jumps ahead by `ms` (a suspend).
    pub(crate) fn jump(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clocks for Jumpy {
    fn boot(&self) -> BootInstant {
        boot_now().plus(Duration::from_millis(self.0.load(Ordering::SeqCst)))
    }

    fn mono(&self) -> Instant {
        Instant::now()
    }
}

/// The timings of the tests: short, with a lease that outlives one missed
/// renewal.
pub(crate) fn timing() -> Timing {
    Timing {
        tick: Duration::from_millis(150),
        request: Duration::from_millis(500),
        validity: Duration::from_millis(1500),
        grace: Duration::from_millis(600),
        probe: Duration::from_secs(1),
        slack: Duration::from_secs(30),
    }
}

pub(crate) fn open() -> Allowed {
    Allowed {
        desktop: true,
        capture: true,
        monitors: Vec::new(),
        quota_bytes: crate::desktop::capture::CAPTURE_QUOTA,
        free_reserve_bytes: crate::desktop::capture::FREE_RESERVE,
    }
}

/// The room check of a disk with room to spare until `low` is set.
pub(crate) fn room_check(low: Arc<AtomicBool>) -> RoomCheck {
    Arc::new(move |_, _| {
        if low.load(Ordering::SeqCst) {
            Err(Fail::new(
                fleetdeck_proto::capture::code::DISK_FULL,
                fleetdeck_proto::capture::text::DISK_FULL,
            ))
        } else {
            Ok(())
        }
    })
}

pub(crate) struct T {
    pub fake: FakeRig,
    pub s: StoreRig,
    pub allowed: Arc<Mutex<Allowed>>,
    pub peer: Arc<Peer>,
    pub clocks: Arc<Jumpy>,
    pub timing: Mutex<Timing>,
    /// The disk is low from now on (the free-space check fails).
    pub low: Arc<AtomicBool>,
}

/// The fixture that matches the mode of `DP-1` of the fake desktop.
pub(crate) fn fixture() -> Vec<u8> {
    Mp4Fixture::default().size(3440, 1440).samples(30).build()
}

impl T {
    pub(crate) async fn new() -> Self {
        Self::with(FakeRig::new(Opts::default()), Some(fixture())).await
    }

    /// A rig with `bytes` as the file that the fake recorder writes; `None`
    /// has no recorder program at all.
    pub(crate) async fn with(fake: FakeRig, bytes: Option<Vec<u8>>) -> Self {
        if let Some(bytes) = bytes {
            let dir = fake.dir.path();
            std::fs::write(dir.join("fixture.mp4"), bytes).unwrap();
            let path = dir.join("bin/wf-recorder");
            std::fs::write(&path, script(dir)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            fake,
            s: store_rig().await,
            allowed: Arc::new(Mutex::new(open())),
            peer: Peer::new(),
            clocks: Jumpy::new(),
            timing: Mutex::new(timing()),
            low: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        self.fake.dir.path()
    }

    pub(crate) fn desktop(&self) -> &Arc<Desktop> {
        &self.fake.desktop
    }

    pub(crate) fn store(&self) -> &Store {
        &self.s.store
    }

    pub(crate) fn set_timing(&self, f: impl FnOnce(&mut Timing)) {
        f(&mut self.timing.lock().unwrap());
    }

    pub(crate) fn request_with(
        &self,
        id: &str,
        monitor: &str,
        seconds: u32,
        fs: Arc<dyn DurableFs>,
    ) -> RecordRequest {
        let allowed = Arc::clone(&self.allowed);
        RecordRequest {
            service: self.s.service.clone(),
            allowed: Arc::new(move || allowed.lock().unwrap().clone()),
            admitted: Admitted::open(),
            deck_fp: FP.to_owned(),
            capture_id: id.to_owned(),
            action_hash: format!("hash-{monitor}-{seconds}"),
            monitor: monitor.to_owned(),
            seconds,
            station_id: "srv".to_owned(),
            station_name: "srv".to_owned(),
            link: Arc::clone(&self.peer) as Arc<dyn DeckLink>,
            link_epoch: "epoch-1".to_owned(),
            coordinator: None,
            params: RecordParams {
                launch: Launch::Direct,
                fs,
                admission: Duration::from_secs(5),
                clocks: Arc::clone(&self.clocks) as Arc<dyn Clocks>,
                timing: *self.timing.lock().unwrap(),
                room: room_check(Arc::clone(&self.low)),
            },
        }
    }

    pub(crate) fn request(&self, id: &str, monitor: &str, seconds: u32) -> RecordRequest {
        self.request_with(id, monitor, seconds, Arc::new(SyncFs))
    }

    pub(crate) async fn run(
        &self,
        id: &str,
        monitor: &str,
        seconds: u32,
    ) -> Result<DesktopActResult, Fail> {
        self.desktop()
            .screen_record(self.request(id, monitor, seconds))
            .await
    }

    /// Starts the recording in a task.
    pub(crate) fn start(
        &self,
        id: &str,
        monitor: &str,
        seconds: u32,
    ) -> tokio::task::JoinHandle<Result<DesktopActResult, Fail>> {
        let desktop = Arc::clone(self.desktop());
        let req = self.request(id, monitor, seconds);
        tokio::spawn(async move { desktop.screen_record(req).await })
    }

    pub(crate) async fn row(&self, id: &str) -> Option<RecordingRow> {
        self.store().recording_get(FP, id).await.unwrap()
    }

    /// Waits until the fake recorder runs and the row says `recording`.
    pub(crate) async fn until_recording(&self, id: &str) {
        for _ in 0..500 {
            let running = self.dir().join("recorder.pid").exists();
            if running
                && self.row(id).await.is_some_and(|r| {
                    r.phase
                        .is_some_and(|p| p == fleetdeck_proto::recording::RecordingPhase::Recording)
                })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the recording did not start");
    }

    /// How many times the fake recorder started.
    pub(crate) fn recorder_runs(&self) -> usize {
        std::fs::read_to_string(self.dir().join("recorder.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "--")
            .count()
    }

    pub(crate) fn flag(&self, name: &str) {
        std::fs::write(self.dir().join(name), "").unwrap();
    }

    pub(crate) async fn artifact_count(&self) -> i64 {
        self.store()
            .db()
            .call(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))?))
            .await
            .unwrap()
    }

    /// The staging folders and the artifact folders that exist.
    pub(crate) fn leftovers(&self) -> (Vec<String>, Vec<String>) {
        let names = |p: &Path| {
            std::fs::read_dir(p)
                .map(|d| {
                    d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        (names(&self.s.captures), names(&self.s.root))
    }

    pub(crate) fn staging_file(&self, id: &str) -> PathBuf {
        self.s.captures.join(id).join("capture.mp4")
    }

    pub(crate) fn set_monitors(&self, json: &str) {
        std::fs::write(self.dir().join("monitors.json"), json).unwrap();
    }

    /// A failed recording left no artifact and no folder, kept no
    /// reservation, and says why.
    pub(crate) async fn assert_discarded(&self, id: &str, code: &str, reason: StopReason) {
        let row = self.row(id).await.unwrap();
        assert_eq!(row.op.state, CaptureState::Failed);
        assert_eq!(row.op.code.as_deref(), Some(code));
        assert_eq!(row.stop_reason, Some(reason));
        assert_eq!(row.op.reserved_bytes, 0);
        assert_eq!(self.artifact_count().await, 0);
        assert_eq!(self.leftovers(), (Vec::new(), Vec::new()));
    }
}

pub(crate) fn err(r: Result<DesktopActResult, Fail>) -> (&'static str, String) {
    let e = r.unwrap_err();
    (e.code, e.message)
}
