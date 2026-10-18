//! The recorder supervisor with a fake recorder script: the exact argv, the
//! save and discard stops, the grace and the escalation, the lease, the
//! suspend, the total deadline, the bounded stderr and the owned group.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::super::clock::{SystemClocks, boot_now};
use super::*;

/// The fake recorder. Under `Launch::Direct` it has no FD 3 (the station
/// crate cannot start the `shim-exec` helper), so it makes FD 3 from stdin,
/// which is the retained file in both launches.
const FAKE: &str = r#"#!/bin/sh
[ -e /proc/self/fd/3 ] || exec 3>&0
echo started >&3
if [ -n "$FAKE_IGNORE_INT" ]; then
  trap '' INT
else
  trap 'echo finished >&3; exit 0' INT
fi
if [ -n "$FAKE_STDERR_BYTES" ]; then
  head -c "$FAKE_STDERR_BYTES" /dev/zero | tr '\0' 'e' >&2
fi
if [ -n "$FAKE_XFSZ" ]; then
  kill -XFSZ $$
fi
echo "$$" > "$FAKE_PID_FILE"
while :; do sleep 0.05; done
"#;

struct Rig {
    dir: tempfile::TempDir,
    program: PathBuf,
    output: File,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("wf-recorder");
        std::fs::write(&program, FAKE).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            dir,
            program,
            output: tempfile::tempfile().unwrap(),
        }
    }

    fn env(&self, extra: &[(&str, &str)]) -> ChildEnv {
        let mut env = ChildEnv::empty();
        env.set("PATH", "/usr/bin:/bin").unwrap();
        env.set("FAKE_PID_FILE", self.dir.path().join("pid"))
            .unwrap();
        for (name, value) in extra {
            env.set(name, value).unwrap();
        }
        env
    }

    fn start(&self, extra: &[(&str, &str)], d: Deadlines, clocks: Arc<dyn Clocks>) -> Recorder {
        spawn_with(
            &Launch::Direct,
            &self.env(extra),
            &self.program,
            "DP-1",
            60,
            &self.output,
            d,
            clocks,
        )
        .unwrap()
    }

    fn contents(&mut self) -> String {
        let mut text = String::new();
        self.output.seek(SeekFrom::Start(0)).unwrap();
        self.output.read_to_string(&mut text).unwrap();
        text
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Deadlines that fire nothing: a long duration, total and lease.
fn quiet() -> Deadlines {
    Deadlines {
        duration: Duration::from_secs(30),
        total: Duration::from_secs(60),
        lease_until: boot_now().plus(Duration::from_secs(600)),
        grace: ms(3000),
    }
}

fn real() -> Arc<dyn Clocks> {
    Arc::new(SystemClocks)
}

/// The boot clock plus a settable jump; the monotonic clock is real.
struct Jumpy(AtomicU64);

impl Clocks for Jumpy {
    fn boot(&self) -> BootInstant {
        boot_now().plus(Duration::from_millis(self.0.load(Ordering::SeqCst)))
    }

    fn mono(&self) -> Instant {
        Instant::now()
    }
}

#[test]
fn the_argv_has_the_exact_tokens() {
    for seconds in [1_u32, 60, 120] {
        assert_eq!(
            argv("DP-1", seconds).unwrap(),
            [
                "-o",
                "DP-1",
                "-r",
                "30",
                "-D",
                "-c",
                "libx264",
                "-x",
                "yuv420p",
                "-b",
                "0",
                "-p",
                "preset=veryfast",
                "-p",
                "crf=23",
                "-F",
                &format!("fps=30,trim=duration={seconds}"),
                "-m",
                "mp4",
                "-y",
                "-f",
                "/proc/self/fd/3",
            ]
        );
    }
}

#[test]
fn seconds_outside_one_to_120_are_invalid_params() {
    for seconds in [0_u32, 121, u32::MAX] {
        let e = argv("DP-1", seconds).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
    }
}

#[tokio::test]
async fn the_duration_timer_saves_with_sigint_and_no_escalation() {
    let mut rig = Rig::new();
    let d = Deadlines {
        duration: ms(400),
        ..quiet()
    };
    let started = Instant::now();
    let exit = rig.start(&[], d, real()).exited().await;
    assert_eq!(exit.cause, Some(StopCause::Duration));
    assert!(exit.status.unwrap().success());
    assert!(!exit.escalated && !exit.xfsz);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(rig.contents(), "started\nfinished\n");
}

#[tokio::test]
async fn the_owner_stop_saves_with_sigint_and_no_escalation() {
    let mut rig = Rig::new();
    let recorder = rig.start(&[], quiet(), real());
    std::thread::sleep(ms(300));
    recorder.owner_stop();
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::OwnerStop));
    assert!(exit.status.unwrap().success());
    assert!(!exit.escalated);
    assert_eq!(rig.contents(), "started\nfinished\n");
}

#[tokio::test]
async fn a_recorder_that_ignores_sigint_is_killed_after_the_grace() {
    let rig = Rig::new();
    let d = Deadlines {
        duration: ms(300),
        grace: ms(500),
        ..quiet()
    };
    let started = Instant::now();
    let exit = rig
        .start(&[("FAKE_IGNORE_INT", "1")], d, real())
        .exited()
        .await;
    let took = started.elapsed();
    assert_eq!(exit.cause, Some(StopCause::Duration));
    assert!(exit.escalated);
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&exit.status.unwrap()),
        Some(9)
    );
    assert!(took >= ms(800), "the grace was cut short: {took:?}");
    assert!(took < Duration::from_secs(4));
}

#[tokio::test]
async fn a_discard_kills_at_once_without_the_grace() {
    let rig = Rig::new();
    let recorder = rig.start(&[], quiet(), real());
    std::thread::sleep(ms(300));
    let started = Instant::now();
    recorder.discard();
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::Discard));
    assert!(!exit.escalated);
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&exit.status.unwrap()),
        Some(9)
    );
    assert!(started.elapsed() < ms(1500));
}

#[tokio::test]
async fn a_discard_after_an_owner_stop_wins_and_escalates_at_once() {
    let rig = Rig::new();
    let recorder = rig.start(&[("FAKE_IGNORE_INT", "1")], quiet(), real());
    std::thread::sleep(ms(300));
    let started = Instant::now();
    recorder.owner_stop();
    std::thread::sleep(ms(150));
    recorder.discard();
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::Discard));
    assert!(exit.escalated);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_save_after_a_discard_does_not_replace_it() {
    let rig = Rig::new();
    let recorder = rig.start(&[], quiet(), real());
    std::thread::sleep(ms(300));
    recorder.discard();
    recorder.owner_stop();
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::Discard));
    assert!(!exit.escalated);
}

#[tokio::test]
async fn a_lease_that_is_not_renewed_expires() {
    let rig = Rig::new();
    let d = Deadlines {
        lease_until: boot_now().plus(ms(500)),
        ..quiet()
    };
    let started = Instant::now();
    let exit = rig.start(&[], d, real()).exited().await;
    assert_eq!(exit.cause, Some(StopCause::LeaseExpired));
    assert!(started.elapsed() >= ms(450));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(!exit.escalated);
}

#[tokio::test]
async fn a_renewed_lease_keeps_the_recorder_alive() {
    let rig = Rig::new();
    let d = Deadlines {
        duration: ms(1200),
        lease_until: boot_now().plus(ms(500)),
        ..quiet()
    };
    let recorder = rig.start(&[], d, real());
    std::thread::sleep(ms(200));
    recorder.renew(boot_now().plus(Duration::from_secs(30)));
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::Duration));
    assert!(exit.status.unwrap().success());
}

#[tokio::test]
async fn a_boot_clock_jump_is_a_suspend() {
    let rig = Rig::new();
    let clocks = Arc::new(Jumpy(AtomicU64::new(0)));
    let recorder = rig.start(&[], quiet(), Arc::clone(&clocks) as Arc<dyn Clocks>);
    std::thread::sleep(ms(200));
    clocks.0.store(5000, Ordering::SeqCst);
    let started = Instant::now();
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::Suspended));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!exit.escalated);
}

#[tokio::test]
async fn the_total_deadline_discards() {
    let rig = Rig::new();
    let d = Deadlines {
        total: ms(500),
        ..quiet()
    };
    let exit = rig.start(&[], d, real()).exited().await;
    assert_eq!(exit.cause, Some(StopCause::Deadline));
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&exit.status.unwrap()),
        Some(9)
    );
}

#[tokio::test]
async fn stderr_over_16_kib_is_cut_and_fully_drained() {
    let rig = Rig::new();
    let d = Deadlines {
        duration: ms(500),
        ..quiet()
    };
    let exit = rig
        .start(&[("FAKE_STDERR_BYTES", "200000")], d, real())
        .exited()
        .await;
    assert_eq!(exit.stderr_tail.len(), STDERR_KEEP);
    assert!(exit.stderr_tail.iter().all(|b| *b == b'e'));
    assert!(exit.status.unwrap().success());
    assert!(!format!("{exit:?}").contains("eeee"));
}

#[tokio::test]
async fn a_child_that_ends_with_sigxfsz_is_marked() {
    let rig = Rig::new();
    let exit = rig
        .start(&[("FAKE_XFSZ", "1")], quiet(), real())
        .exited()
        .await;
    assert!(exit.xfsz);
    assert_eq!(exit.cause, None);
}

#[tokio::test]
async fn an_unrelated_process_of_any_name_survives_every_stop() {
    let rig = Rig::new();
    let mut bystander = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let recorder = rig.start(&[], quiet(), real());
    std::thread::sleep(ms(300));
    recorder.discard();
    let exit = recorder.exited().await;
    assert_eq!(exit.cause, Some(StopCause::Discard));
    assert!(bystander.try_wait().unwrap().is_none());
    bystander.kill().unwrap();
    drop(bystander.wait());
}

#[tokio::test]
async fn a_member_of_the_group_is_gone_when_the_exit_is_reported() {
    let rig = Rig::new();
    let recorder = rig.start(&[], quiet(), real());
    std::thread::sleep(ms(300));
    let pid: i32 = std::fs::read_to_string(rig.dir.path().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    recorder.discard();
    drop(recorder.exited().await);
    let group = rustix::process::Pid::from_raw(pid).unwrap();
    assert_eq!(
        rustix::process::kill_process_group(group, Signal::KILL),
        Err(rustix::io::Errno::SRCH)
    );
}

#[test]
fn a_program_that_cannot_start_is_a_failure() {
    let rig = Rig::new();
    let result = spawn_with(
        &Launch::Direct,
        &rig.env(&[]),
        &rig.dir.path().join("missing"),
        "DP-1",
        60,
        &rig.output,
        quiet(),
        real(),
    );
    assert_eq!(result.err().map(|e| e.code), Some("recording_failed"));
}
