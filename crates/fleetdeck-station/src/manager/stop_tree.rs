//! Real process trees for the tests of the stop of a lost run (BG-4).
//!
//! A tree is a `sh -c <script>` that the test starts with `setsid`: it is
//! the leader of its own session and group, as a provider is. Every
//! process that a test signals is one that the test started itself.

#![cfg_attr(
    not(target_os = "linux"),
    allow(unused_imports, dead_code, reason = "most tests here are Linux-only")
)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fleetdeck_providers::host::shim::identity::real_boot_id_reader;
use fleetdeck_providers::host::shim::stop_inventory::{
    Inventory, Member, StatNow, inventory, stat_now,
};
use fleetdeck_providers::host::shim::wire::ProviderIdentity;

use super::stop_signal;
use super::stop_state::{PlanTarget, StopPlan};

/// The longest wait of a test for a tree to show its members.
const WAIT: Duration = Duration::from_secs(15);

pub struct Tree {
    leader: Child,
    /// The leader was reaped: its PID may be anybody's now.
    reaped: bool,
    /// Every member that the test saw, as `(pid, start_time)`.
    known: std::sync::Mutex<Vec<(u32, u64)>>,
}

impl Tree {
    /// Starts `script` as the leader of a new session.
    pub fn spawn(script: &str) -> Self {
        let leader = Command::new("setsid")
            .arg("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the tree with setsid");
        Self {
            leader,
            reaped: false,
            known: std::sync::Mutex::default(),
        }
    }

    /// Three members: the leader and two `sleep`.
    pub fn simple() -> Self {
        let tree = Self::spawn("sleep 600 & sleep 600 & wait");
        tree.wait_members(3);
        tree
    }

    pub fn pid(&self) -> u32 {
        self.leader.id()
    }

    /// The identity that the shim records for the leader.
    pub fn identity(&self) -> ProviderIdentity {
        let StatNow::Alive(stat) = stat_now(self.pid()) else {
            panic!("the leader is not alive")
        };
        ProviderIdentity {
            pid: self.pid(),
            start_time: stat.start_time,
            boot_id: real_boot_id_reader()().expect("the boot id"),
        }
    }

    /// The inventory of the tree, retried while the machine churns.
    pub fn inventory(&self) -> Inventory {
        let until = Instant::now() + WAIT;
        loop {
            match inventory(self.pid()) {
                Ok(inv) => return inv,
                Err(e) => assert!(Instant::now() < until, "{e}"),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn members(&self) -> Vec<Member> {
        let domain = self.inventory().domain;
        let mut known = self.known.lock().unwrap();
        for m in &domain {
            if !known.contains(&(m.pid, m.start_time)) {
                known.push((m.pid, m.start_time));
            }
        }
        domain
    }

    /// Waits until the domain has `n` live members.
    pub fn wait_members(&self, n: usize) {
        let until = Instant::now() + WAIT;
        while self.members().len() != n {
            assert!(
                Instant::now() < until,
                "no {n} members: {:?}",
                self.members()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// True when the process of this PID lives (not a zombie).
    pub fn alive(pid: u32) -> bool {
        matches!(stat_now(pid), StatNow::Alive(s) if !s.is_zombie())
    }

    /// The members of the tree as the targets of a stop plan.
    pub fn plan(&self, session: &str, shim: &str) -> StopPlan {
        let members = self.members();
        let targets = members
            .iter()
            .map(|m| PlanTarget {
                pid: m.pid,
                start_time: m.start_time,
                fd: stop_signal::open(m.pid).expect("open a pidfd"),
            })
            .collect();
        StopPlan::new(
            "plan-1".to_owned(),
            (session.to_owned().into(), shim.to_owned(), self.pid()),
            targets,
            members.iter().map(|m| (m.pid, m.start_time)).collect(),
            (Instant::now(), 0),
        )
    }

    /// Ends every member and reaps the leader. The test started them all.
    pub fn end(&mut self) {
        if !self.reaped {
            // The leader number is still the tree's.
            self.members();
            let _ = self.leader.kill();
            let _ = self.leader.wait();
            self.reaped = true;
        }
        for (pid, start_time) in self.known.lock().unwrap().iter() {
            kill_process_of(*pid, *start_time);
        }
    }

    /// Kills the leader and reaps it; the other members live on. After
    /// this the tree never kills by the leader's number again: the test
    /// ends the orphans itself with [`kill_process_of`].
    pub fn kill_leader(&mut self) {
        self.members();
        self.leader.kill().expect("kill the leader");
        self.leader.wait().expect("reap the leader");
        self.reaped = true;
    }

    /// Writes into a FIFO that a member of the tree reads: it goes on.
    pub fn release(fifo: &Path) {
        std::fs::write(fifo, "go\n").expect("release the tree");
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.end();
    }
}

/// Kills the process `pid` when it still has `start_time`: a process that
/// a test started, never a PID that something else reuses.
pub fn kill_process_of(pid: u32, start_time: u64) {
    use rustix::process::{Pid, Signal, kill_process};
    if matches!(stat_now(pid), StatNow::Alive(s) if s.start_time == start_time)
        && let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw)
    {
        let _ = kill_process(pid, Signal::KILL);
    }
}

/// A FIFO in `dir`.
pub fn fifo(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("go");
    let status = Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("run mkfifo");
    assert!(status.success());
    path
}
