# Resume after Stop without manual steps (B13)

- **Status:** Draft, revision 2 (2026-10-23, after the adversary design review round 1; see the last section). Risk: **high** (it changes the termination proof of DL-182 and the process-termination path of the shim). It needs an adversary DESIGN REVIEW before the build, and a PRE-PR REVIEW before the merge.
- **Owner report (B13, 2026-10-22):** the owner stops a session and then resumes it. The resume fails. The owner must open "Check the previous run" and click "Mark as ended". After that, the resume works.
- **Wanted:** Stop and then Resume work without manual steps. The station does the check and the reconciliation itself when it is safe. The owner sees the manual dialog only when the station cannot decide.
- **Base:** `v2` at 00055138 (revision 1 used 497b2fbc; the code that this design reads is the same, except the station schema V43 of b0cdbf5f). All `file:line` references are on 00055138.
- **Evidence folder:** `~/fleetdeck-briefs/backlog/resume-after-stop/` (recordings, scripts, logs).
- **Decision-log numbers:** DL-430 to DL-434 (§11).

## 1. The problem and the verified facts

### 1.1 The cause, in one paragraph

The cause is a bug in the shim, not a live process. At a provider exit, the shim counts the members of the provider's process group and session (the census). Before and between the census passes, the shim must reap its own ended children: the orphans of the provider come back to the shim, because the shim is the child subreaper. The reaper calls `rustix::process::waitpid(None, NOHANG)` (`crates/fleetdeck-providers/src/host/shim/server_proc.rs:194-196`). In rustix 1.1.5 `waitpid(None, …)` is `waitpid(0, …)`: it waits only for children **in the caller's own process group** (rustix `src/process/wait.rs:407-433`; backend `Pid::as_raw(None) == 0`). The shim has its own group (it calls `setsid()`, `server.rs:128-131`), and the provider's orphans are in the provider's group. So the reaper never reaps them. Their zombies stay in the provider's session. A zombie of the group or session makes every census pass "not clean" (`inspect.rs:186-190`), so after the 1 s exit budget (`inspect.rs:135`) the count is `None`, and the station records `exited` / `exit_unverified` (`manager/shims.rs:62-70`). That is not a proof (`store/resumes.rs:41-57`), so the resume is refused (`manager/resume.rs:118-123`). When the station acknowledges the exit, the shim ends; its zombie children go to the init process, which reaps them. Some seconds later the owner's check (`session.execution_check`) finds nothing and "Mark as ended" passes.

### 1.2 Verified facts (code)

1. The exit path of the shim (`server.rs:389-436`): `child.wait()` reaps the provider (`server.rs:382`), then on a blocking thread `reap_orphans()` and `group_members(leader)` (`server.rs:407-410`), then a numeric group kill (`server.rs:419`), then `reap_orphans()` again (`server.rs:421`), then the exit report with `descendants_remaining` and `inspection: 1` (`server.rs:423-433`).
2. `group_members` passes `reap_orphans` as the reaper between passes (`server_proc.rs:189-191`, `inspect.rs:222-224`).
3. `reap_orphans` is `while let Ok(Some(_)) = waitpid(None, NOHANG) {}` (`server_proc.rs:194-196`). The periodic reaper that runs while the provider lives uses another function, `reap_orphans_except`, which reads the zombie children from `/proc` and waits for each one by its pid (`server_proc.rs:225-244`). That one is correct and tested (`server_proc_tests.rs:50-148`). No test covers `reap_orphans` with an orphan in another process group.
4. The census (`inspect.rs:169-199`): a pass is clean only when no listed pid vanished and no zombie is the leader or a member of the leader's group or session. A pass that shows a live member is final at once. The exit budget is 1 s, the check budget 3 s (`inspect.rs:131-139`).
5. The station maps the report (`manager/shims.rs:62-70`): `Some(0)` with `inspection >= 1` → `exited`; `Some(n > 0)` → `exited_descendants_remaining`; `None` → `exit_unverified`.
6. Proof (`store/resumes.rs:41-57`): a row proves its end when it is `never_started`, or when it has a `proof` (`ended_by_reboot`, `reconciled_by_owner`), or it is a clean exit (`exited`, reason `exited`, `inspection_version >= 1`).
7. The resume decision refuses with `invalid_state` "not_resumable: the previous execution may still run (shim <id> is exited (exit_unverified))" (`manager/resume.rs:118-123`, `store/resumes.rs:73-80`).
8. "Mark as ended" (`session.reconcile_execution`) runs `check_row` before the decision (`manager/manager_execution.rs:21-43`, placed at `manager/mod.rs:628`) and refuses while the check is incomplete or a process lives (`manager/shims.rs:592-662`). It is owner only (board or local origin, `manager/shims.rs:599-604`).
9. The exit path also has an unsafe fallback: `signal_group` sends the signal to the single pid when the group is gone (`server_proc.rs:175-181`). At `server.rs:419` the provider is already reaped, so that pid number is free and can belong to an unrelated process. The group kill itself is unsafe there too: when the old group is empty, a new process can get the number `L` and make a new group `L` (`setsid`, `setpgid`).
10. BG-4 ("Stop previous run processes", `docs/v2/design/board-gaps.md` §5) stops processes of a lost run through pidfds, after an elevation, and says "Do not Mark as ended automatically after a stop" (`board-gaps.md:783`).
11. The control frames `Kill` and `Interrupt` call `signal_group(self.pid(), …)` with no check of the provider's state (`server_conn.rs:223-234`), and so do `stop()` (`server.rs:729`, `:736`) and the early error path of `run` (`server.rs:366`, before any reap). After the reap, the shim still accepts frames while `wait_stdout_end` waits for the owner's `ack`s, which has no time limit while the buffer is full (`server.rs:441-467`). A late frame can therefore signal a reused number `L`.
12. Tokio 1.53 reaps a child only when its `Child` future is polled, or when the `Child` was dropped (`tokio/src/process/unix/orphan.rs`, `pidfd_reaper.rs`). Nothing else in the shim waits for the provider.
13. An older station binary cannot open a newer `station.db`: `migrations.to_latest` fails with `DatabaseTooFarAhead`, which is `DbError::TooNew` (`fleetdeck-core/src/db.rs:220-228`); the station does not start. Before a migration the station writes `<data>/backups/station-v<from>-to-v<to>-<unix ms>.db` (`fleetdeck-core/src/db_backup.rs:38`, `operations.md` §13.6).
14. Auto-continue does not go through `SessionManager::submit`. At the station start, `auto_continue::evaluate` (`manager/mod.rs:322`) calls `store.decide` directly (`auto_continue.rs:229-232`) and refuses a session with an unresolved execution as `execution_unknown` (`auto_continue.rs:106-107`; test `descendants_remaining_and_an_unverified_exit_are_not_a_proof`, `auto_continue_shim_tests.rs:276`).

### 1.3 Verified facts (live, on Lighting, 2026-10-23)

| Evidence | Result |
| --- | --- |
| Station database, all shim rows (read-only) | 15 rows. Exits: 7 × `exited/exit_unverified/inspection 1` (5 of them later `reconciled_by_owner`), 1 × `exited/exited`. **No `exited_descendants_remaining` row.** Lost rows: 4 × `execution_unknown` with `ended_by_reboot`. |
| The owner's own case (session `01M5KVK917…`, 2026-10-23 08:18) | `session.stop` 08:18:52, `session.ended` 08:18:53, `session.resume` refused 08:18:54 "…shim 01M5KVK92711… is exited (exit_unverified)", `session.reconcile_execution` accepted 08:19:01, `session.resume` accepted 08:19:02. The same sequence on 2026-10-22 16:18 for session `01M5G7VDC6…`. |
| Reproduction, Claude (`recording-claude-stop1.md`) | A new Claude session, one turn, fleet `stop`. For 1.02 s after the leader exit, every census pass sees 0 vanished pids, 0 live members, and 4 zombies of the provider's session whose parent is the shim. Then the zombies are gone. Row: `exited/exit_unverified`. `resume_session` refused with the same text. No process of the session lives after the stop. |
| Reproduction, Codex (`recording-codex-stop1.md`) | The same: 2 zombies of the session whose parent is the shim, for 1.02 s. Row `exited/exit_unverified`; resume refused. Codex starts its MCP servers in **their own process groups** inside the provider's session, so only the session id ties them to the run. |
| `scripts/waitpid_zero.py` | A subreaper in its own group with an orphan zombie in another group: `waitpid(0, WNOHANG)` gives `ECHILD` and the zombie stays; `waitpid(-1, WNOHANG)` reaps it. |
| `scripts/census_churn.py` (20 s, normal load) | 98.9 % of passes are clean (256 processes, a pass takes 2.5 ms). Machine-wide churn does not explain the failures. |

### 1.4 Corrections to the backlog text of B13

1. "Read the `shims` row (`state`, `reason`, `descendants_remaining`, …)": the row has no `descendants_remaining` column. The count lives only in the exit report and the exit file; the row stores the reason (`store/shims.rs:96-118`).
2. "Possible row reasons are `exited_descendants_remaining` …, `exit_unverified` …, or `lost`": on Lighting every failed case is `exit_unverified` with `inspection_version = 1`. It is not an older shim and not a live descendant.
3. "So when the button works, the station had already found nothing alive. The only reason for the owner's click is the accepted risk of an escaped descendant": not correct. The click works because the shim has ended by then, and the init process reaped the zombies that the shim could not reap. The exit census did not find "something alive"; it found zombies that it could not remove.
4. The design statement "a graceful stop gives `exited` with a verified zero count in the normal case" (`shim-resume.md:70`) is false on HEAD for every provider that has a child that ends at about the same time as the provider (MCP servers, hooks). The one clean exit on Lighting is an exception.

## 2. Answers to the questions of B13

**Q1. Which row reason does a normal Stop leave?** `exited` / `exit_unverified` / `inspection_version 1`, for Claude and for Codex (§1.3). The cause is the reaper bug of §1.1, not a live process. The fix of the reaper (§3.1) alone makes a normal Stop give `exited` with `Some(0)`. In addition, the shim stops the leftover members of the run before the exit report (§3.2), so that a slow MCP server or a background command does not leave `exited_descendants_remaining`.

**Q2. After an owner Stop, may the station reconcile by itself?** Yes, for one exact case, and the cause of the end does not matter (a Stop, a provider crash and a hibernation give the same evidence). The station may record its own proof for a row that is `exited` with the effective reason `exit_unverified`, when a fresh complete check on the same boot finds no live provider identity and no group or session member (§3.3). The shim saw the provider end through `waitpid`; only the members were not proven, and the check proves them. This is the standard of a clean exit (DL-182 §1a), checked later. Rows that are `lost` or `exited_descendants_remaining` stay owner-only: there the station saw no exit, or saw live processes.

**Q3. Can Resume run the check first?** Yes. The station runs the settle check of §3.3 inside `submit`, before the decision of `session.resume` and of an input that resumes a hibernated session. Auto-continue does not go through `submit` (§1.2 fact 14), so it runs the same settle itself at the station start, before its decision (§3.3). The error shows only when the check finds a live process, is incomplete, or the row is not eligible.

**Q4. Can Resume offer "Stop them and resume"?** Yes, on the board, as one owner action: the BG-4 stop (with its plan and its elevation), then a fresh check, then "Mark as ended" (the owner's consent, with the warning in the confirmation text), then the resume (§3.4). It changes one rule of BG-4 for this combined action only (DL-433).

## 3. Design

### 3.1 The reaper fix (shim)

- `reap_orphans` in `server_proc.rs:194-196` becomes:

```rust
/// Reaps every ended child of the shim (`waitpid(-1)`). Only for the exit
/// path, after tokio has reaped the provider: the shim has no other child
/// whose status another owner needs (DL-430).
pub(super) fn reap_orphans() {
    while let Ok(Some(_)) = rustix::process::wait(rustix::process::WaitOptions::NOHANG) {}
}
```

- `rustix::process::wait(NOHANG)` is `waitpid(-1, WNOHANG)`: any child, of any process group. The callers are only the exit path (`server.rs:408`, `:421`) and the census reaper (`server_proc.rs:190`), which all run after `child.wait()` returned. The periodic reaper (`reap_orphans_except`) does not change.
- After T2, `reap_orphans()` must never run while the leader is unreaped: it would reap the leader and end the pin of §3.2. In T2 the sweep and the pinned census use `reap_orphans_except(Some(leader))`; `reap_orphans()` runs only after the leader reap (§3.2, step E5 and step U3).
- This task changes no wire, no row and no station code.

### 3.2 The exit sweep with a pinned leader (shim)

Goal: before the exit report, the shim ends every live member of the provider's group and session, and counts again. The report then gives `Some(0)` in the normal case also when an MCP server or a background command is slow to end.

**Safety rule: a numeric signal (`kill(-L)` or `kill(L)`) is sent only while the leader is unreaped.** While the provider's process or zombie exists, Linux keeps its pid number allocated; no new process can get that number, so no new process group and no new session can have the number `L` (a new group or session id is the pid of its creator). Every process with `pgrp == L` or `session == L` is therefore a member of this run. After the shim reaps the leader, no code path of the shim sends a numeric signal to `L` or `-L`: not the exit path, not a control frame, not `stop()` (§3.2.2). This is the BG-4 witness case (a) "the leader is alive" (`board-gaps.md` §5.3), with the zombie as the anchor.

#### 3.2.1 The wait without a reap

- Right after the spawn (`server.rs` `serve`, after `child.id()`), the shim starts one dedicated thread (`std::thread::Builder`, name `fd-shim-leader-wait`) that runs `wait_leader(L) -> LeaderWait` (new file `server_leader.rs`) and sends the result over a `tokio::sync::oneshot` channel:

```rust
pub(super) enum LeaderWait {
    /// The leader terminated; it stays a zombie (WNOWAIT): `L` is pinned.
    Ended,
    /// The wait failed; the leader may or may not be reaped. Fail closed.
    Failed(rustix::io::Errno),
}

/// Linux: waits until the leader terminates, without reaping it.
pub(super) fn wait_leader(pid: u32) -> LeaderWait {
    // Pid::from_raw(pid) (None → Failed(Errno::INVAL));
    // loop {
    //   match rustix::process::waitid(WaitId::Pid(pid),
    //                                 WaitIdOptions::EXITED | WaitIdOptions::NOWAIT) {
    //     Ok(Some(_)) => return LeaderWait::Ended,
    //     Ok(None) | Err(Errno::INTR) => continue,
    //     Err(e) => return LeaderWait::Failed(e),
    //   }
    // }
}
// Not Linux: `wait_leader` returns Failed(Errno::NOSYS) at once (the shim host
// refuses outside Linux, DL-182; the code only has to compile).
```

- `waitid(P_PID, …, WEXITED | WNOWAIT)` is in Linux since 2.6.9. The pin therefore does not depend on pidfd support: no `waitid(P_PIDFD)` (Linux 5.4), no `AsyncFd`, no readiness error to handle. A closed oneshot channel (the thread panicked or did not start) counts as `Failed(Errno::IO)`; a thread that cannot start is logged at `warn`.
- `run` awaits the oneshot instead of `child.wait()`. Nothing polls `child.wait()` before step E4 or U2, so tokio does not reap the leader before then (§1.2 fact 12).
- The pinned census and the sweep run only after `LeaderWait::Ended`: that result is the confirmed termination. `Failed` never enables them.
- A pidfd of the leader: right after the spawn, `pidfd_open(L, PidfdFlags::empty())`. `Ok` → `leader_fd = Some(fd)`; any error (`ENOSYS` on Linux < 5.3, `EMFILE`, other) → `None`, logged at `debug`. `L` is unreaped at that time, so the pidfd names the provider. The shim uses it only in the state `Unpinned` (§3.2.2).

#### 3.2.2 The leader lifecycle and every signal of the provider

New type `Leader` in `server_leader.rs`. It is the only code that signals the provider after the `Shim` exists:

```rust
pub(super) enum Lifecycle {
    /// Not terminated, not reaped.
    Running,
    /// Terminated, a zombie (LeaderWait::Ended): the exit sweep runs.
    Ended,
    /// LeaderWait::Failed: the leader may still run until tokio reaps it.
    Unpinned,
    /// Reaped (or about to be reaped: the reap starts only after this state).
    Reaped,
}

pub(super) struct Leader {
    pid: u32,
    fd: Option<std::os::fd::OwnedFd>,
    state: std::sync::Mutex<Lifecycle>,
    /// A Kill during `Ended`: the sweep skips its graces (§3.2.3 step E2.3).
    escalate: tokio::sync::watch::Sender<bool>,
}

impl Leader {
    /// Sends `sig` by the rule table below. `on_running` runs under the
    /// lifecycle lock before the send, only in `Running`.
    pub(super) fn signal(&self, sig: Signal, on_running: impl FnOnce()) -> Sent;
    pub(super) fn set(&self, to: Lifecycle);
    pub(super) fn escalated(&self) -> tokio::sync::watch::Receiver<bool>;
}

pub(super) enum Sent { Group, Escalated, LeaderFd, Nothing }
```

| State | SIGTERM, SIGINT | SIGKILL |
| --- | --- | --- |
| `Running` | `signal_group(L, sig)` (today's helper, with its single-pid fallback: safe, `L` is unreaped) → `Group` | the same |
| `Ended` | nothing → `Nothing` | sets `escalate`, sends nothing → `Escalated`. The sweep reads it at its next poll (≤ 50 ms) and goes to the SIGKILL rounds; its group kill is `kill(-L)` with `L` still pinned |
| `Unpinned` | `pidfd_send_signal(leader_fd, sig)` to the leader process only (an error, also `ESRCH`, is logged at `debug`) → `LeaderFd`; without `leader_fd`: nothing, `warn` "the provider cannot be signalled safely" → `Nothing` | the same |
| `Reaped` | nothing, `debug` log → `Nothing` | nothing → `Nothing` |

- `signal` holds the lifecycle mutex while it sends (`kill` and `pidfd_send_signal` do not block). `set` takes the same mutex. The shim calls `set(Reaped)` before it starts the reap. So a numeric signal can never run at the same time as the reap or after it.
- Lock order: the lifecycle mutex before the `State` mutex. The `on_running` closure of `Kill` takes the `State` mutex inside the lifecycle mutex; no code calls `Leader` while it holds the `State` mutex.
- The callers (all change to `Leader`):
  - `server_conn.rs` `ToShim::Kill`: `self.leader.signal(KILL, || self.lock().cause = Some("killed"))`. A Kill after the provider ended does not change the cause of that exit.
  - `server_conn.rs` `ToShim::Interrupt`: `self.leader.signal(INT, || {})`.
  - `server.rs` `stop()`: `self.leader.signal(TERM, || {})` and `self.leader.signal(KILL, || {})` (`server.rs:729`, `:736`). The cause `stop_forced` is set as today, before the call, under the `State` mutex only.
  - `server.rs` `run`, the missing-pipe error path (`server.rs:366`): `self.leader.signal(KILL, || {})` (state `Running`).
  - `serve`, the identity error (`server.rs:192`): the `Shim` does not exist yet and the child is unreaped; the direct `signal_group` call stays.
- After this change `signal_group` has two callers: `Leader` (state `Running`) and `serve` (unreaped child). The numeric group kill of the exit sweep is `kill_group_only(L, sig)` (no single-pid fallback), called only in the state `Ended`.

#### 3.2.3 The exit sequence

**When `wait_leader` gives `Ended` (the normal path, Linux):**

- E1. `leader.set(Ended)`; then `exited` is set (as today, `server.rs:383`), so `stop()` ends its waits.
- E2. On a blocking thread, `exit_sweep(leader, killed, escalate)` (new file `server_exit.rs`); `killed` is `cause == Some("killed")` at this time:
   1. Reap the zombie children except the leader: `reap_orphans_except(Some(leader))`. In the whole sweep this is the only reaper (§3.1).
   2. Census with the pinned leader (§3.2.4). Result `Some(0)` → go to E2.6.
   3. `Some(n > 0)`: wait for the members to end by themselves. Every 50 ms (`SWEEP_POLL`): reap (except the leader) and count again, for at most `MEMBER_GRACE` = 2 s. The grace is 0 when `killed` is true, and it ends at once when `escalate` becomes true.
   4. Members remain: send SIGTERM. The group: `kill_group_only(L, TERM)` (safe: `L` is pinned). Each live session member whose `pgrp != L`: `pidfd_open(pid)`, then read `/proc/<pid>/stat` again, then `pidfd_send_signal(TERM)` only when the read shows `session == L` and the same start time as the census read. Wait up to `MEMBER_TERM_GRACE` = 2 s with the same 50 ms count loop; `escalate` ends this wait at once.
   5. Members remain: SIGKILL in the same way, to every member of a fresh census (new members are members of this run too), at most `KILL_ROUNDS` = 3 rounds 100 ms apart.
   6. The final census (budget 1 s) gives `descendants_remaining`.
   - Errors of the member path: `pidfd_open` gives `ESRCH` → the member is gone (fine). Any other error (`ENOSYS`, `EMFILE`, `EPERM`) or a recheck that does not match → that member is not signalled; it stays in the count. `kill_group_only` gives `ESRCH` → the group is empty (fine); another error → `warn`. A failed signal never stops the sweep; the final census decides the report.
- E3. `kill_group_only(L, KILL)` once more (today's `server.rs:419`, without the single-pid fallback). `L` is still pinned.
- E4. `leader.set(Reaped)`; then `child.wait().await` reaps the leader. Its status gives the termination text as today (`code:<n>`, `signal:<n>`, `unknown`).
- E5. `reap_orphans()` (the `waitpid(-1)` loop of §3.1).
- E6. The rest is as today: `wait_stdout_end`, the exit report, the publication. A control frame in this time sends nothing (state `Reaped`).

**When `wait_leader` gives `Failed` (the unpinned path):** this path is not expected on Linux (the only real cause is `ECHILD` after a reap by other code). It fails closed and sends no numeric signal.

- U1. `warn` with the errno; `leader.set(Unpinned)`.
- U2. `child.wait().await` (tokio waits and reaps; an error gives the termination `unknown`). Then `leader.set(Reaped)`, then `exited` is set.
- U3. `reap_orphans()`, then the census without a pin as today: `exit_census(L, false, &mut reap_orphans)`. Its result goes into the report unchanged: `Some(0)`, `Some(n)` or `None`. (A new process that reuses `L` makes a new group or session `L`, which the census counts: a false "member", never a false `Some(0)`.)
- U4. No sweep, no group kill, no single-pid kill. A live member stays and is counted (`exited_descendants_remaining` → the owner path).
- U5. As E6.

The longest exit path is about 1 + 2 + 2 + 0.3 + 1 ≈ 6.3 s (a member that ignores SIGTERM). A run with no live member at the exit is as fast as today, or faster (no 1 s of unclean passes).

#### 3.2.4 The census with a pinned leader

- New parameter of the census: `pinned: bool`. With `pinned = true`, the zombie of the leader itself does not make a pass unclean. Reason: the leader was a zombie before the first listing (`LeaderWait::Ended` confirmed it), so it cannot fork during any pass. Every other rule of a clean pass stays (`inspect.rs:157-168`): a vanished pid or a zombie member makes the pass unclean, and a live member is final at once.
- New file `inspect_exit.rs` (a child module of `inspect.rs` through `#[path]`, so that `inspect.rs`, now 710 lines, stays under 800): `pub fn exit_census(leader: u32, pinned: bool, reap: &mut dyn FnMut()) -> Option<u32>` (the `group_members_reaping` of today becomes `exit_census(leader, false, reap)`), and `pub fn exit_members(leader: u32, pinned: bool, reap: &mut dyn FnMut()) -> Option<Vec<ProcStat>>` with `(pid, pgrp, session, start_time)` of each live member (the leader excluded) for the sweep.

#### 3.2.5 What does not change

- `INSPECTION` stays 1 and the wire does not change (`wire.rs`). The meaning of `Some(0)` is the same: a clean pass with zero live members.
- The stop policy (`StopPolicy`, the 30 s grace and the 5 s TERM grace of the leader) does not change. The sweep starts only after the leader ended.
- Escaped descendants (left both the group and the session) are not touched. A member that escapes between the check and the signal can still get the signal (the same accepted limit as BG-4 §5.4).

### 3.3 The station settle check (station)

**When it runs.** In two places.

1. In `SessionManager::submit` (`manager/mod.rs:594`), after `stop_prepare` (`:624`, which holds the per-session lock for `session.resume`). The call `let check = self.reconcile_check(&command).await;` (`:628`) becomes `let check = self.pre_decision(&command).await;`. The new method `pre_decision` (in `manager_execution.rs`) runs the settle, then returns `self.reconcile_check(&command).await` as today:
   - `SessionCommand::Resume { session_id }` (board Resume, "Resume all", fleet `resume_session`): settle that session. `stop_prepare` already holds the per-session lock; `pre_decision` does not take it again.
   - A command of `restart_gate::may_resume` (`Prompt`, `FollowUp`, `Steer`, `ShellCommand`, `restart_gate.rs:179-187`) for a session whose summary state is `exited` or `lost` (a hibernated session): `pre_decision` takes `self.target.stops().locks.hold(session)` for the settle only and drops it before it returns.
   - No other command. `session.import` keeps the owner path (out of scope, §8).
2. At the station start, for auto-continue (§1.2 fact 14). `manager/mod.rs:322-325` (the call of `auto_continue::evaluate` and its log; the comment above stays) becomes one call: `auto_continue::evaluate_at_start(&store, &policy, restore_from, shims.as_ref()).await?;`. In `auto_continue.rs`:
   - `pub async fn evaluate_at_start(store, policy, after, shims: Option<&target::ShimSettings>) -> Result<usize>`: takes the boot reader of the shim host (`Arc::clone(&settings.host.config().boot_id_reader)`), calls `evaluate_settling`, and logs `"stored auto-continue intents"` as today.
   - `pub async fn evaluate_settling(store, policy, after, boot: Option<&BootIdReader>) -> Result<usize>`: the loop of `evaluate`. For each session that passes the existing pre-filter of the loop (`auto_continue` on, summary state `lost`, `lost_work` is `Some`), and only when `boot` is `Some` (a shim host), it calls `settle_unverified_exits(store, boot, &session_id)` **before** `store.decide`. The decision `decide_lost` and its rules (`ineligible`, `admit_resume`) do not change; they read `proves_end`, which now accepts the station proof.
   - `evaluate(store, policy, after)` stays with the same signature and calls `evaluate_settling(…, None)`, so the existing tests and callers do not change.
   - No per-session lock is needed: the station start admits no command before this call (`mod.rs` comment "before any command is admitted").
   - The continuation dispatch recheck (`auto_continue::recheck`, `shim_recheck`) needs no change: it reads the rows through `execution_exclusion`, which uses `proves_end`.

**The shared settle function.** `pub(super) async fn settle_unverified_exits(store: &Store, boot: &BootIdReader, session: &SessionId)` is a free function in `manager_execution.rs`, so the start (no `SessionManager` yet) and `pre_decision` (with `self.boot_reader()`) use the same code.

**Which rows.** `fn settle_eligible(row: &ShimRow) -> bool` in `store/resumes.rs`:

```rust
row.state == ShimState::Exited
    && row.proof.is_none()
    && row.station_proof.is_none()
    && effective_reason(row) == EXIT_UNVERIFIED   // raw exit_unverified, or a legacy exited without inspection
    && row.provider_identity.is_some()
```

**The check.** For each eligible row of the session (oldest first): `check_row(&boot, &row)` on a blocking thread (`manager/shims.rs:513`, the same check as "Mark as ended", budget 3 s). The station records the proof only when `check.complete && check.same_boot && !check.still_runs()`. Otherwise it does nothing; the decision then refuses as today.

**The proof.** New column `station_proof` on `shims`, value `checked_after_exit`, and `station_proof_at` (Unix ms). The store method writes it and the event in one write transaction:

```rust
/// DL-432: records that the station checked an unverified exit and found
/// nothing of the run. `false` when the row is not eligible any more.
pub async fn shim_settle(&self, session: SessionId, shim_id: String) -> Result<bool>;
// UPDATE shims SET station_proof = 'checked_after_exit', station_proof_at = ?now, updated_at = ?now
//  WHERE shim_id = ?1 AND session_id = ?2 AND state = 'exited' AND proof IS NULL
//    AND station_proof IS NULL
//    AND (reason = 'exit_unverified' OR (reason = 'exited' AND inspection_version < 1))
// then, when one row changed, append Event::ExecutionReconciled { shim_id, by: Some(ReconciledBy::Station) }
```

- `proves_end` (`store/resumes.rs:51-57`): `ShimState::Exited | ShimState::Lost => row.proof.is_some() || row.station_proof.is_some() || clean_exit(row)`.
- `UNRESOLVED` (`store/resumes.rs:486-488`) adds `AND station_proof IS NULL`, so the boot pass, the owner's check and `shims_unresolved` skip a settled row.
- The decision and the dispatch recheck (`unended_execution`, `unended_native`) need no other change: they read `proves_end`.

**Schema V44** (the number that the architect assigned to this track; station V43 is the DL-402 fix of b0cdbf5f). New file `store/schema_v44.rs` with `pub(super) const V44`, `mod schema_v44;` in `store/mod.rs`, `M::up(super::schema_v44::V44)` at the end of the list in `store/migrations.rs`. Forward only, like V23 (`store/migrations.rs:553-572`):

```sql
-- DL-432 (B13): the station's own proof of an unverified exit. NULL or
-- 'checked_after_exit'; next to the row's state, reason and owner proof.
ALTER TABLE shims ADD COLUMN station_proof TEXT
    CHECK (station_proof IS NULL OR station_proof = 'checked_after_exit');
ALTER TABLE shims ADD COLUMN station_proof_at INTEGER;
```

`ShimRow` gets `pub station_proof: Option<String>` and `pub station_proof_at: Option<i64>`; `COLUMNS` and the decode add them.

**The event.** `Event::ExecutionReconciled` (`fleetdeck-proto/src/event.rs:359`) gets an optional field:

```rust
#[serde(rename = "session.execution_reconciled")]
ExecutionReconciled {
    shim_id: String,
    /// Who proved the end: absent for the owner (older stations), `station`
    /// for the station's own check (DL-432).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    by: Option<ReconciledBy>,
},

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum ReconciledBy {
    Owner,
    Station,
    /// A value of a newer peer.
    #[serde(other)]
    Unknown,
}
```

TypeScript (generated): `{ "kind": "session.execution_reconciled", shim_id: string, by?: ReconciledBy }` and `type ReconciledBy = "owner" | "station" | "unknown"`. The owner path keeps `by: None` (the wire of today). The fold ignores the event (`fold.rs:330`). The board does not render it (§8).

**Log.** `tracing::info!(session, shim, "the station proved the end of an unverified exit")`; a check that does not prove: `tracing::info!` with the reason (incomplete, a live process, another boot).

**Errors.** A store error or a check that did not run is logged (`warn`) and the command goes on to its decision, which refuses from the row as today. The settle never rejects a command by itself.

### 3.4 The board

#### 3.4.1 A refused resume opens the check

- New module `web/src/session/resumeRefusal.ts`:

```ts
/** The station refused a resume because a previous run is not proven ended (DL-182, DL-433). */
export const UNPROVEN_RUN = 'not_resumable: the previous execution may still run';
export function isUnprovenRun(text: string): boolean {
  return text.includes(UNPROVEN_RUN);
}
```

- `SessionView.tsx`: the `actions` given to the header, the composer resume bar and the hibernated input path get a wrapped `resume`:

```ts
const resume = async (): Promise<Outcome> => {
  const outcome = await actions.resume();
  if (!outcome.ok && actions.canAdmin && isUnprovenRun(outcome.text)) setChecking('resume');
  return outcome;
};
```

  `checking` becomes `null | 'menu' | 'resume'`. The menu item "Check the previous run" opens `'menu'` (today's dialog, unchanged texts).
- A member or a guest (no `admin` scope) sees only the refusal text, as today.
- This works with an older station too: the prefix is the text of `manager/resume.rs:121` since DL-182.

#### 3.4.2 The dialog in resume mode

`ExecutionDialog` gets a prop `purpose: 'menu' | 'resume'` (default `'menu'`) and `onResumed: () => void`. It also gets the `.fd-dialog-head` of DESIGN-SYSTEM.md §3 (title, description, the X `.fd-iconbtn` "Close"); the text button "Close" at the foot goes away (it breaks the rule "a dialog has no text Close button"). The `aria-label` stays "Previous run of this session" in both modes (tests use it).

| State (resume mode) | What the dialog shows |
| --- | --- |
| Head | Title **"Resume needs the previous run to end"**. Description: "Fleet Deck cannot prove that the previous run of this session stopped. It checked the processes again." |
| Loading | `Loading` of `ui/Loading.tsx` with "Checking the processes…" |
| Load error | `ErrorNotice` with `onRetry` = check again |
| Complete, nothing lives | The lines and lists of today. Note: "Nothing of the previous run runs now. Fleet Deck cannot see a process that left its process group. Mark it ended only if you know that the agent is not running." Button `.fd-danger` **"Mark as ended and resume"** (busy: "Resuming…"). |
| Processes live | The lists. The BG-4 block with the action **"Stop them and resume"** (instead of "Stop these processes…"). |
| Incomplete | Today's incomplete text and the "Check again" button. No mark button. |
| Nothing unresolved (the station settled it meanwhile) | "Nothing to reconcile: no earlier run of this session is unresolved." Button `.fd-primary` **"Resume"**. |

**"Mark as ended and resume":** `reconcileExecution(shimId)`; on `ok`, `resume()`; on `ok`, `onResumed()` (the dialog closes). A refusal of either call shows its text under the button (`role="alert"` through the existing note pattern of the dialog) and keeps the dialog open.

**"Stop them and resume"** (`ExecutionStop` with a new prop `then: 'resume' | null`, default `null`):

1. The plan and the elevation of BG-4 do not change (`STOP_ELEVATION_LABEL` "Stop previous run processes").
2. The confirmation text is `STOP_CONFIRM` plus one sentence: "Then Fleet Deck marks the run as ended and resumes the session. Fleet Deck cannot see a process that left its process group."
3. After the outcome (`awaitStop`): when the outcome is `done` without a code and the fresh check is complete and `stillRuns` is false, the board calls `reconcileExecution(shimId)`, then `resume()`, then `onResumed()`.
4. Any other result shows the BG-4 result text (`stopResultText`) and the new check; nothing is marked and nothing resumes.

#### 3.4.3 Texts in one place

| Key | Text |
| --- | --- |
| Head title (resume) | Resume needs the previous run to end |
| Head description (resume) | Fleet Deck cannot prove that the previous run of this session stopped. It checked the processes again. |
| Nothing lives note | Nothing of the previous run runs now. Fleet Deck cannot see a process that left its process group. Mark it ended only if you know that the agent is not running. |
| Mark button | Mark as ended and resume |
| Busy | Resuming… |
| Stop action | Stop them and resume |
| Stop confirm addition | Then Fleet Deck marks the run as ended and resumes the session. Fleet Deck cannot see a process that left its process group. |
| Settled meanwhile | (existing) Nothing to reconcile: no earlier run of this session is unresolved. + button Resume |

### 3.5 Compatibility

| Pair | Behavior |
| --- | --- |
| New station, shim that an older build launched (still running at the upgrade) | That shim keeps the bug: its exit gives `exit_unverified`. The settle check of §3.3 proves it at the next resume. |
| New shim, older station | The report is the same wire. The older station gets `exited` with `Some(0)` more often. Nothing else changes. |
| An older station binary after V44 | It does not start: `DbError::TooNew` (§1.2 fact 13). There is no in-place binary rollback. The supported recovery is the one of `operations.md` §13.6: (1) forward repair, the normal path: install a fixed newer build; (2) only when the new build has not written data that matters: stop the service, restore `<data>/backups/station-v43-to-v44-<unix ms>.db` as `station.db` (the caution of §13.6: every station write since the upgrade is lost), start the older binary. Never lower `user_version` by hand. The live smoke (§5) checks that the upgrade on Lighting wrote this backup at V43. |
| Older deck | The deck passes `by` through (unknown fields are kept, `event.rs:6-8`). |
| Older board | It ignores `by`. Without the board task, the owner still gets the settle at resume (no dialog). |
| macOS station | The shim host refuses on macOS (DL-182 status line); the settle check is incomplete there (`check_row`, `NO_INSPECTION`): nothing changes. |
| Older Linux kernel | The pin of the leader uses `waitid(P_PID, WNOWAIT)` (Linux 2.6.9), so the sweep runs on every supported kernel. Only the signals to session members in other groups need `pidfd_open` (Linux 5.3); without it those members are not signalled and stay in the count (`exited_descendants_remaining`, owner path). The leader pidfd of the state `Unpinned` is optional too. |

No feature flag is needed: the station decides alone, and the wire change is one optional field.

### 3.6 Edge cases

1. A resume while the shim of the run is still in its exit sweep: the row is `running` until the report commits; the resume refuses "only an ended session can be resumed" as today. The board can try again.
2. A member that ignores SIGTERM and SIGKILL (uninterruptible sleep `D`): the final census counts it; the row is `exited_descendants_remaining`; the owner path (BG-4 stop, Mark as ended).
3. A member owned by another user (a `sudo` child): the signal fails with `EPERM`; it is counted; the owner path.
4. A member that forks faster than the sweep: at most 3 kill rounds, then the count; the owner path.
5. A row of another boot: `check_row` says `same_boot = false`; the settle does nothing; the boot pass at the next station start records `ended_by_reboot` (`manager/shims.rs` `prove_reboots`).
6. Two resumes at the same time: `stop_prepare` serializes them per session; the second settle finds the row settled and does nothing.
7. A BG-4 stop and a resume at the same time: the same per-session lock (`board-gaps.md` §5.3 rule 1).
8. A legacy row (`exited/exited`, inspection 0): eligible (effective reason `exit_unverified`).
9. A Kill stop (`StopMode::Kill`): the shim sends SIGKILL to the leader's group at once (today), and the sweep has no grace.
10. A `Kill` frame during the sweep (state `Ended`): no signal is sent by the frame; the sweep skips its graces and goes to the SIGKILL rounds within 50 ms. The cause of the exit does not change.
11. A `Kill` or `Interrupt` frame after the reap (state `Reaped`), for example while `wait_stdout_end` waits for `ack`s: nothing is sent, the cause does not change, a `debug` line is logged.
12. `wait_leader` fails (state `Unpinned`): no sweep and no numeric signal; the report holds the census without a pin (`Some(0)`, `Some(n)` or `None`). Stop and Kill frames before the reap reach only the leader process through its pidfd, or nothing.

## 4. Risk class and threat notes

**Risk: high.** The design changes the termination proof of DL-182 (a new proof kind that no owner gives) and the process-termination path of the shim (new signals to session members), and it adds a schema migration.

| Threat | Why it holds |
| --- | --- |
| The station proves an end while a provider of the run still runs, and a second provider starts on the same conversation | The settle needs: the row is `exited` (the shim saw the leader end through `waitpid`), a complete check on the same boot, no live process with the stored identity (pid + start time), and no live member of the group or session. Membership of a group or session comes only from a fork of a member; when a clean pass sees no member, no member can appear later. A later process that reuses the number `L` makes a **new** group or session, which the check counts as a member: a false "still runs" (fail closed), never a false proof. |
| An escaped descendant (left the group and the session) writes to the resumed conversation | Out of scope of every proof since DL-182 §1a, and the same on the direct host. The settle accepts it without the owner's click, as the clean exit proof does today. The owner path keeps its warning. The design does not make it worse than a clean exit. |
| A member alive at the exit escapes before the later check | The same as a member that escapes just before the exit census. With the sweep (§3.2), the members that the shim sees are stopped before the report. A row with live members (`exited_descendants_remaining`) is never settled by the station. |
| The sweep signals an unrelated process | The leader is not reaped until the sweep ends, so the number `L` cannot be reused: every process with `pgrp == L` or `session == L` belongs to the run. A session member in another group is signalled only through a pidfd, after a fresh read shows `session == L` and the same start time. The group kill of the sweep has no single-pid fallback. |
| A late control frame (`Kill`, `Interrupt`) or a `stop()` escalation signals a process that reused `L` | Every signal of the provider goes through `Leader` (§3.2.2). A numeric signal is sent only in the state `Running` (the leader is unreaped, so `L` is pinned). The shim sets `Reaped` under the same mutex before it reaps, so no numeric signal can follow the reap. In `Ended` a Kill only escalates the sweep, whose own kill runs while `L` is pinned. |
| The exit waiter fails and the legacy order kills a foreign group | There is no legacy numeric kill any more. When `wait_leader` fails, the unpinned path (§3.2.3 U1-U5) sends no numeric signal at all and reports its census as it is; signals before the reap use only the leader pidfd. |
| The sweep kills something that the owner wanted to keep (a dev server that the agent started) | A process of the agent's session ends with the agent. Today the shim already kills the leader's group at every exit (`server.rs:419`); only session members in other groups survived. A process that must outlive the agent must leave the session (`setsid`); then it is an escaped descendant and is not touched. Owner question 1. |
| A long exit path blocks the station | The longest path is about 6.3 s and runs on a blocking thread of the shim; the station waits for the report as it does for any exit. A takeover during the path attaches as today. |
| An automatic proof for a session that the owner did not resume (auto-continue at the start) | The settle at the start runs only for sessions with auto-continue on, state `lost` and a lost turn, on a shim host, and records only the same proof as a resume. The auto-continue rules (`ineligible`) still decide; a session that they skip keeps a true proof and no intent. |
| A wrong automatic proof cannot be undone | The proof is a separate column; state and reason stay as evidence. An audit row (the event with `by: station`) names the row. |
| Rollback | An older station binary does not start on the V44 database (`DbError::TooNew`). Recovery is forward repair or the restore of the V43 pre-upgrade backup (§3.5, `operations.md` §13.6); the restore loses station writes since the upgrade, so forward repair is the default. |
| Denial of resume by a process that never ends | The owner path stays (BG-4 stop, Mark as ended), now offered in the refusal dialog. |

## 5. Build plan

Workers run with the AGENTS.md rules (every heavy command through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`). At most two workers at the same time. T1 and T4 can run in parallel; T2 and T3 can run in parallel after T1 (their files do not overlap).

### T1. Reap every ended child at the provider exit (backend, ~90 lines)

- **Owned files:** `crates/fleetdeck-providers/src/host/shim/server_proc.rs`, `crates/fleetdeck-providers/src/host/shim/server_proc_tests.rs`, `crates/fleetdeck-cli/tests/shim_resume_process.rs`.
- **Changes:**
  1. `server_proc.rs:193-196`: `reap_orphans` uses `rustix::process::wait(WaitOptions::NOHANG)` in the loop (§3.1), with the doc comment of §3.1.
  2. `server_proc.rs:203`: the comment of `reap_while_running` names the fixed function.
- **Tests:**
  - `server_proc_tests.rs` `reap_orphans_reaps_an_orphan_of_another_process_group`: under nextest (`isolated()`), become the subreaper, start `setsid sh -c 'sleep 0.1 &'` and wait for it, wait until a zombie child exists whose process group differs from the test's group, call `reap_orphans()`, assert `child_pids()` is empty within 6 s.
  - `shim_resume_process.rs` `a_child_that_ends_with_the_provider_leaves_a_proven_exit`: `Rig::new("sh -c 'true &'; sleep 0.3; echo done")`; the exit report has `descendants_remaining == Some(0)`, `inspection == 1`, and `shims::exit_reason` is `exited`. (On 497b2fbc it gives `None`.)
- **Depends on:** none.
- **Acceptance:**
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(reap_orphans)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(shim_resume_process)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T2. The exit sweep with a pinned leader, and the signal gate (backend, ~750 lines)

- **Owned files:** `crates/fleetdeck-providers/src/host/shim/server.rs`, `crates/fleetdeck-providers/src/host/shim/server_conn.rs`, `crates/fleetdeck-providers/src/host/shim/server_leader.rs` (new), `crates/fleetdeck-providers/src/host/shim/server_leader_tests.rs` (new), `crates/fleetdeck-providers/src/host/shim/server_exit.rs` (new), `crates/fleetdeck-providers/src/host/shim/server_exit_tests.rs` (new), `crates/fleetdeck-providers/src/host/shim/server_proc.rs`, `crates/fleetdeck-providers/src/host/shim/inspect.rs`, `crates/fleetdeck-providers/src/host/shim/inspect_exit.rs` (new), `crates/fleetdeck-providers/src/host/shim/inspect_pinned_tests.rs` (new), `crates/fleetdeck-testkit/src/sim_codex.rs`, `crates/fleetdeck-cli/tests/shim_manager_parts/live_exit.rs`, `crates/fleetdeck-cli/tests/shim_exit_sweep.rs` (new).
- **Changes:**
  1. `server.rs`: `#[path = "server_leader.rs"] mod leader;` and `#[path = "server_exit.rs"] mod exit;` next to the existing `conn`, `process`, `secrets` modules. `server.rs` (738 lines) must stay under 800 lines: `on_exit` moves to `server_exit.rs` (an `impl Shim` block, as `server_conn.rs` does).
  2. `server.rs` `serve`: after `child.id()`, open `leader_fd` and start the `fd-shim-leader-wait` thread (§3.2.1); build `Leader` (state `Running`) and keep it in `Shim` as `leader: Leader`. `Shim::pid()` stays (the census and the report use the number).
  3. `server.rs` `run`: await the oneshot of `wait_leader` instead of `child.wait()`; then call `exit::on_exit(self, child, wait, eof_rx)`, which runs §3.2.3 (E1-E6 or U1-U5). `child: &mut Child` is passed to `on_exit` for the reap.
  4. `server.rs` `stop()` and the missing-pipe path of `run`: call `self.leader.signal(…)` (§3.2.2). `serve`'s identity error path keeps `signal_group`.
  5. `server_conn.rs`: `ToShim::Kill` → `self.leader.signal(KILL, || self.lock().cause = Some("killed"))`; `ToShim::Interrupt` → `self.leader.signal(INT, || {})`. No other change.
  6. `server_leader.rs`: `LeaderWait`, `wait_leader` (Linux and not-Linux variants), `Lifecycle`, `Leader`, `Sent`, the rule table of §3.2.2. For the tests, a `#[cfg(test)] pub(super) fn with_sender(pid, fd, send: Box<dyn Fn(Target, Signal) + Send + Sync>) -> Leader` constructor, with `pub(super) enum Target { Group(u32), LeaderFd }`; the production constructor uses the real syscalls through the same `send` slot.
  7. `server_exit.rs`: `on_exit` (moved and changed), `exit_sweep`, the grace and signal loops of §3.2.3 E2, constants `MEMBER_GRACE = 2 s`, `MEMBER_TERM_GRACE = 2 s`, `KILL_ROUNDS = 3`, `SWEEP_POLL = 50 ms`; the pidfd open, recheck and send for session members; and `pub(super) fn unpinned_count(leader: &Leader, census: impl FnOnce() -> Option<u32>) -> Option<u32>` (steps U2-U4 after the reap: asserts the state is `Reaped`, runs the census, sends nothing). The escalate receiver is polled at every `SWEEP_POLL`.
  8. `server_proc.rs`: `pub(super) fn kill_group_only(pid: u32, sig: Signal) -> rustix::io::Result<()>` (`kill_process_group` only, no `SRCH` fallback, `pid <= 1` refused); `group_members` calls `exit_census(leader, false, &mut reap_orphans)`; a new `pub(super) fn pinned_members(leader: u32) -> Option<Vec<ProcStat>>` calls `exit_members(leader, true, &mut || reap_orphans_except(Some(leader)))`.
  9. `inspect.rs`: the `pinned` parameter of the census (§3.2.4); `#[path = "inspect_exit.rs"] mod exit;` with `exit_census` and `exit_members` re-exported as `pub use`; `group_members_reaping` delegates to `exit_census(leader, false, reap)`. Tests in `inspect_pinned_tests.rs`.
  10. `sim_codex.rs`: new modes `exit-leaving-stubborn-child <text>` (a child `sh -c 'trap "" TERM; sleep 30'` in the app-server's group) and `exit-leaving-group-child <text>` (a child `sleep 30` with `process_group(0)`: another group, the same session).
  11. `live_exit.rs`: the test `a_live_shim_exit_with_a_surviving_child_keeps_the_evidence_and_resume_stays_refused` becomes `a_live_shim_exit_with_a_surviving_child_stops_it_and_the_resume_is_admitted`: the row is `exited` / `exited`, the `sleep` child is gone, and the resume is accepted.
- **Tests:**
  - `inspect_pinned_tests.rs`: `a_pinned_zombie_leader_does_not_make_a_pass_unclean` (fake `/proc`: zombie leader, no member → `Some(0)` with `pinned`, `None` without); `a_zombie_member_still_makes_a_pinned_pass_unclean`; `a_live_member_is_counted_with_a_pinned_leader`.
  - `server_leader_tests.rs` (the `with_sender` spy; no real signal): `leader_running_signals_the_group`; `leader_ended_kill_escalates_and_sends_nothing`; `leader_ended_term_and_int_send_nothing`; `leader_unpinned_signals_only_through_the_pidfd`; `leader_unpinned_without_a_pidfd_sends_nothing`; `leader_reaped_sends_nothing_for_every_signal` (TERM, INT, KILL); `leader_kill_marks_the_cause_only_while_running` (the `on_running` closure runs in `Running` only).
  - `server_exit_tests.rs` (Linux, real children of the test process, under nextest `isolated()`): `leader_wait_keeps_the_zombie` (spawn `sh -c 'exit 3'` with `std::process::Command`; `wait_leader(pid)` is `Ended`; `/proc/<pid>/stat` shows `Z`; then `child.wait()` gives code 3); `leader_wait_after_a_foreign_reap_fails` (spawn `true`, reap it with `child.wait()`, then `wait_leader(pid)` is `Failed(Errno::CHILD)`); `unpinned_exit_sends_no_numeric_signal` (the regression of the old fallback: a spy `Leader` set `Unpinned` then `Reaped`, `unpinned_count` with a census that returns `Some(1)` → `Some(1)`, and the spy saw no send; then `signal(KILL)` and `signal(INT)` → `Sent::Nothing`, still no send).
  - `shim_exit_sweep.rs` (real processes, the `shim_rig` of `fleetdeck-cli/tests`): `a_member_that_outlives_the_provider_is_stopped_before_the_report` (`sleep 30 & echo done` → `Some(0)`, the `sleep` pid is gone); `a_member_that_ignores_term_is_killed` (`sh -c 'trap "" TERM; sleep 30' & echo done` → `Some(0)`, report within 7 s); `a_session_member_in_another_group_is_stopped` (a `setsid`-free child in a new group via `sh -c 'set -m; sleep 30 &'` → `Some(0)`); `an_escaped_process_is_not_signalled` (`setsid sleep 30 & echo done` → `Some(0)`, the escaped pid still lives, the test kills it at the end); `the_leader_status_is_kept` (`exit 3` → `code:3`); `late_kill_and_interrupt_after_the_reap_change_nothing` (`Rig::with` with the default 64 KiB buffer and the script `head -c 100000 /dev/zero | tr '\0' a; exit 0`: 64 KiB fill the shim buffer, the rest fits in the 64 KiB pipe, so the provider exits; attach and do not ack; wait until `/proc/<provider pid>` is gone, which means the leader is reaped and `wait_stdout_end` waits for acks; send `ToShim::Interrupt` and `ToShim::Kill`; then ack every output frame; the exit report has `cause == "exited"`, `termination == "code:0"` and `descendants_remaining == Some(0)`); `a_kill_during_the_sweep_skips_the_grace` (`sh -c 'trap "" TERM; sleep 30' & echo done`; send `ToShim::Kill` 100 ms after the provider pid is a zombie; the report arrives within 2 s with `Some(0)`).
  - `live_exit.rs`: the changed test above; `a_live_exit_with_a_child_in_another_group_is_proven` with `exit-leaving-group-child`.
- **Depends on:** T1.
- **Acceptance:**
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(pinned) | test(reap_orphans) | test(census) | test(leader) | test(unpinned)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(shim_exit_sweep) | binary(shim_resume_process) | binary(shim_process) | binary(shim_manager)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
  - `grep -n "signal_group" crates/fleetdeck-providers/src/host/shim/server*.rs` shows only the definition, the `use` line, the call in `serve` (identity error) and the call in `server_leader.rs`.

### T3. The station settle check at resume and at auto-continue (backend, ~520 lines)

- **Owned files:** `crates/fleetdeck-proto/src/event.rs`, `crates/fleetdeck-proto/src/event_tests.rs`, `web/src/gen/event/Event.ts`, `web/src/gen/event/ReconciledBy.ts` (new, generated), `web/src/gen/index.ts` (only when the generator adds the export), `crates/fleetdeck-station/src/store/schema_v44.rs` (new), `crates/fleetdeck-station/src/store/mod.rs` (only the `mod schema_v44;` line), `crates/fleetdeck-station/src/store/migrations.rs`, `crates/fleetdeck-station/src/store/shims.rs`, `crates/fleetdeck-station/src/store/resumes.rs`, `crates/fleetdeck-station/src/store/resumes_tests.rs`, `crates/fleetdeck-station/src/manager/manager_execution.rs`, `crates/fleetdeck-station/src/manager/mod.rs`, `crates/fleetdeck-station/src/manager/shims.rs`, `crates/fleetdeck-station/src/manager/auto_continue.rs`, `crates/fleetdeck-station/src/manager/auto_continue_shim_tests.rs`, `crates/fleetdeck-station/src/manager/shim_settle_tests.rs` (new), `crates/fleetdeck-station/src/manager/shim_resume_check_tests.rs`, `crates/fleetdeck-cli/tests/shim_resume_recovery.rs`.
- **Changes:**
  1. Proto: the `by` field and `ReconciledBy` (§3.3); the TS files regenerated with the project's ts-rs export (the same command that made `web/src/gen`).
  2. Every `ExecutionReconciled { shim_id }` pattern or constructor gets `by` or `..` (`manager/shims.rs:656` sets `by: None`; the two tests).
  3. Schema V44 (§3.3): `schema_v44.rs` with the SQL of §3.3 and its tests; `mod schema_v44;` in `store/mod.rs`; `M::up(super::schema_v44::V44)` in `migrations.rs`. In `migrations.rs`, the `vec![…]` of `migrations()` moves to a private `fn all() -> Vec<M<'static>>`; `migrations()` is `Migrations::new(all())`; a new `#[cfg(test)] pub(crate) fn migrations_upto(version: usize) -> Migrations<'static>` truncates `all()` to its first `version` entries.
  4. `store/shims.rs`: the two fields in `ShimRow`, `COLUMNS`, the decode.
  5. `store/resumes.rs`: `settle_eligible`, `proves_end`, `UNRESOLVED`, `shim_settle` (§3.3).
  6. `manager_execution.rs`: the free function `settle_unverified_exits(store, boot, session)` (§3.3, logs, never fails) and the method `pre_decision(&self, command) -> Option<ExecutionCheck>` (§3.3 "When it runs", item 1).
  7. `manager/mod.rs`: line 628 calls `self.pre_decision(&command)` instead of `self.reconcile_check(&command)`; lines 322-325 become the one call of `auto_continue::evaluate_at_start` (§3.3 item 2). `mod.rs` (797 lines) must not grow.
  8. `auto_continue.rs`: `evaluate_at_start`, `evaluate_settling`, and `evaluate` as a wrapper (§3.3 item 2). The module comment gets one sentence: on a shim host, an unverified exit of the same boot is settled before the decision (DL-432).
- **Tests (`shim_settle_tests.rs`, with `shim_resume_rig.rs`):**
  - `a_resume_after_an_unverified_exit_with_nothing_alive_is_admitted_and_records_the_station_proof` (row `exited/exit_unverified/1` with a dead identity on the real boot → accepted; `station_proof = checked_after_exit`; one `ExecutionReconciled { by: Some(Station) }`).
  - `a_live_group_member_keeps_the_resume_refused` (a test child in the identity's group, as test 8 of DL-182 does) → refused, no proof.
  - `an_exit_with_descendants_remaining_is_not_settled_by_the_station` → refused.
  - `a_lost_row_is_not_settled_by_the_station` (`lost/execution_unknown`) → refused.
  - `a_row_of_another_boot_is_not_settled_by_the_check` → refused, no station proof.
  - `a_legacy_exit_without_inspection_is_settled` (`exited/exited/0`) → accepted.
  - `a_prompt_to_a_hibernated_session_settles_and_resumes` → accepted.
  - `the_owner_reconcile_still_writes_no_by_field` (the event JSON has no `by`).
  - `auto_continue_shim_tests.rs` (its `Rig`; the boot reader is `shim_resume_rig::Boot` set to `"boot-now"`): `auto_continue_settles_an_unverified_exit_of_the_same_boot` (auto-continue on, a lost turn, row `exited/exit_unverified/1` with `dead_identity("boot-now")`; `evaluate_settling(…, Some(&reader))` → 1 intent, no skip, `station_proof = checked_after_exit`, one `ExecutionReconciled { by: Some(Station) }`); `auto_continue_off_is_not_settled` (the same row, auto-continue off → 0 intents, `station_proof` is NULL); `auto_continue_without_a_boot_reader_does_not_settle` (the same row with `evaluate` → 0 intents, skip `execution_unknown`, `station_proof` NULL). The existing `descendants_remaining_and_an_unverified_exit_are_not_a_proof` stays unchanged and passes (another boot, no boot reader).
  - `resumes_tests.rs`: `proves_end_accepts_a_station_proof`; `unresolved_skips_a_settled_row`.
  - `schema_v44.rs`: `v44_adds_the_station_proof_columns` (the CHECK refuses another value); `v44_database_is_too_new_for_the_v43_list` (open a file database with `migrations()`, then `Db::open(path, migrations_upto(43))` gives `DbError::TooNew { found: 44, .. }`).
  - `event_tests.rs`: `execution_reconciled_reads_without_by_and_with_an_unknown_by`.
- **Depends on:** T1 (order of merge only; no shared file).
- **Acceptance:**
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(execution_reconciled)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(settle) | test(proves_end) | test(unresolved) | test(v44) | test(shim_resume) | test(auto_continue)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(shim_resume_recovery) | binary(shim_resume)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T4. The board: a refused resume opens the check (ux, ~350 lines)

- **Owned files:** `web/src/session/resumeRefusal.ts` (new), `web/src/session/resumeRefusal.test.ts` (new), `web/src/session/ExecutionDialog.tsx`, `web/src/session/ExecutionStop.tsx`, `web/src/session/SessionView.tsx`, `web/src/session/execution.test.tsx`, `web/src/session/executionStop.test.tsx`, `web/src/styles/session.css`.
- **Changes:** §3.4 exactly (texts of §3.4.3). The dialog head uses `.fd-dialog-head`; the foot keeps only "Check again". Follow DESIGN-SYSTEM.md (load the `fleet-design-system` skill): `Loading`, `ErrorNotice`, at most one `.fd-primary`, the phone bottom sheet.
- **Tests:**
  - `resumeRefusal.test.ts`: `isUnprovenRun` is true for "Refused: not_resumable: the previous execution may still run (shim x is exited (exit_unverified))" and false for "Refused: only an ended session can be resumed" and for "not_resumable: another execution of this conversation may still run (…)".
  - `execution.test.tsx`: `a refused resume opens the dialog in resume mode` (owner); `a member sees only the refusal`; `mark as ended and resume calls reconcile then resume and closes`; `a refused reconcile shows its text and does not resume`; `resume mode with nothing unresolved offers Resume`; `the dialog has the head close button and no text Close button`.
  - `executionStop.test.tsx`: `stop them and resume marks and resumes after a done outcome and a clean check`; `a stop with processes_remain neither marks nor resumes`; `the confirmation names the mark and the resume`.
- **Depends on:** none (the refusal prefix exists on 497b2fbc).
- **Acceptance:**
  - `cd web && sh ../tools/heavy.sh bun run test src/session/execution src/session/executionStop src/session/resumeRefusal`
  - `cd web && bun run typecheck && bun run lint`
  - `sh tools/design-check.sh`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
  - The gallery shot of the dialog in both themes, desktop and phone (skill §3 step 3).

### Live smoke (architect, after T1–T4)

On Lighting, with the new station and a new session (not one that an older shim runs): one Claude and one Codex session; one turn; fleet `stop`; `resume_session` accepted at once; the row is `exited/exited/1`. Then an older-shim case: a session started before the upgrade, stopped after it: the resume is accepted with `station_proof = checked_after_exit`.

The recovery check of §3.5: after the upgrade, `ls ~/.local/share/fleetdeck/backups/` shows `station-v43-to-v44-<unix ms>.db`; on a copy of that file in `/tmp`, `sqlite3 <copy> 'PRAGMA user_version; PRAGMA integrity_check;'` gives `43` and `ok`. Then delete the copy. Do not start an older binary on the live data.

## 6. Decisions made in this draft

1. **The cause is fixed in the shim first** (T1), as a small separate task. Rejected: only a station-side settle, which would hide the bug and keep a 1 s unclean census at every exit.
2. **`waitpid(-1)` in the exit path**, not `reap_orphans_except(None)`. The provider is already reaped there, and the kernel call is complete without `/proc`. Rejected: the `/proc`-based reaper, which depends on `CONFIG_PROC_CHILDREN` or a full scan.
3. **The shim stops leftover members of the group and the session at every exit** (natural, Stop, Kill, hibernation), before the report. Rejected: only after a Stop (a crash leaves the same orphans; the group kill of today already runs at every exit).
4. **The leader stays unreaped until every numeric signal of the run is sent** (a dedicated thread with `waitid(P_PID, WEXITED | WNOWAIT)`, revision 2). Rejected: a sweep after the reap with numeric ids (a reused number could be a foreign group or session); a sweep without session members (Codex MCP servers are in other groups); the revision 1 waiter (`pidfd` + `AsyncFd` + `waitid(P_PIDFD)`), which needs Linux 5.4 although `pidfd_open` works on 5.3, and which had no defined path for its registration and wait errors.
5. **Session members outside the leader's group are signalled one by one through pidfds**; the leader's group through `kill(-L)` while `L` is pinned. Rejected: `kill(-G)` for each other group `G` (a number of a group can be reused once the group is empty).
6. **No numeric signal after the reap, on any path** (revision 2). Every signal of the provider goes through `Leader` and its lifecycle (§3.2.2): numeric signals only in `Running`; in `Ended` a Kill escalates the sweep; in `Unpinned` only the leader pidfd; in `Reaped` nothing. The legacy order (reap, then `kill(-L)`) is removed; a failed wait takes the unpinned path, which sends no numeric signal and reports its census as it is. Rejected: keeping today's fallback at `server.rs:419`; keeping a group kill after the reap in a legacy path (a reused `L` can be a foreign group).
7. **Timings:** members get 2 s to end by themselves, then SIGTERM and 2 s, then SIGKILL in at most 3 rounds; no grace on a Kill stop. Rejected: no grace (an MCP server gets no chance to flush) and the 30 s grace of the leader (the report would wait too long).
8. **The census budget at the exit stays 1 s.** With the reaper fixed, a busy machine gives clean passes in milliseconds (§1.3). Rejected: a longer budget.
9. **The station settles only `exited` rows with the effective reason `exit_unverified`.** Rejected: settling `exited_descendants_remaining` (the station saw live processes) and `lost` rows (the station saw no exit; `attach_refused` and `identity_mismatch` can mean a live shim).
10. **The settle runs at resume time only**: the `submit` of `session.resume` and of an input to an ended session, and, for auto-continue, at the station start before the auto-continue decision of each candidate (revision 2; auto-continue does not use `submit`). Rejected: a background settle after every exit (more code paths, a check of rows that nobody resumes); a settle of every row at the station start (slow start with many rows); removing auto-continue from the coverage (an auto-continue session with an unverified exit would stay `lost` although the station can prove the end).
11. **A new column `station_proof`**, not a new value of `proof`. The CHECK of `proof` (V23) cannot change without a table rebuild. Rejected: reusing `reconciled_by_owner` (a false audit) and a table rebuild (a riskier migration).
12. **The event `session.execution_reconciled` gets an optional `by`**, absent for the owner. Rejected: a new event kind (older boards and decks would carry an unknown kind for nothing) and `by: owner` on owner events (a wire change without need).
13. **No feature flag.** The station decides alone, and the event change is additive.
14. **`session.import` keeps the owner path.** Rejected: a settle of other sessions' rows at import (rare, and it widens the change).
15. **The board detects the refusal by its text prefix** "not_resumable: the previous execution may still run". Rejected: a new `RejectCode` (an older deck maps it to `unknown`) and a new structured field.
16. **"Stop them and resume" includes "Mark as ended"** in one owner action, with the warning in the confirmation. This changes BG-4 §5.5 ("Do not Mark as ended automatically after a stop") for this combined action only; the separate "Stop these processes…" of the menu dialog keeps the BG-4 rule. Rejected: a stop that then shows a second button.
17. **The dialog gets `.fd-dialog-head` and loses its text Close button** (a design-system rule that the dialog breaks today).
18. **The board does not render the reconciliation event** in the timeline.
19. **Schema number: V44** (assigned by the architect; V43 is the DL-402 fix). No in-place binary rollback after V44: an older binary refuses the database (`DbError::TooNew`); recovery is forward repair, or the restore of the V43 pre-upgrade backup of `operations.md` §13.6 (revision 2). Rejected: a compatibility build of the older release that accepts V44 (a second release line for a two-column addition).
20. **The leader pidfd is optional and used only in `Unpinned`** (revision 2). The pin comes from the unreaped zombie, not from the pidfd. Rejected: requiring a pidfd for the exit path (no sweep on Linux < 5.3 for no safety gain).
21. **A Kill frame after the provider ended does not change the exit cause** (revision 2): the cause `killed` is set only in `Running`, under the lifecycle lock. Rejected: today's unconditional `cause = killed` (a Kill that arrives while the output drains would rename a natural exit).
22. **`on_exit` moves to `server_exit.rs` and the census additions to `inspect_exit.rs`** (revision 2), so that `server.rs` (738 lines) and `inspect.rs` (710 lines) stay under the 800-line rule.

## 7. Owner questions

1. **Leftover processes at the end of a run.** When the agent's process ends, the shim stops every process that is still in the agent's session (MCP servers, background commands that did not detach). Today it stops only the agent's process group. Is that what you want? **Recommended default:** yes, at every end of a run (Stop, Force stop, crash, hibernation).
2. **Grace for leftover processes.** How long may a leftover process end by itself before Fleet Deck asks it to stop? **Recommended default:** 2 s, then SIGTERM and 2 s more, then SIGKILL.
3. **"Stop them and resume" marks the run as ended.** It is one click after the elevation, with the warning "Fleet Deck cannot see a process that left its process group." **Recommended default:** yes, one action.

## 8. Out of scope

- Import (`session.import`) of a conversation whose other session has an unverified exit: owner path as today.
- A timeline line for `session.execution_reconciled` (with `by: station`).
- cgroup containment of each run (a kernel-held list of every descendant, also escaped ones). It is the stronger long-term proof; a separate item.
- macOS (no shim host there, DL-182).
- The repair of existing rows: the owner's two unreconciled rows on Lighting (`01M5HJ5E…`, `01M5J6H5…`) get the station proof at their next resume; nothing rewrites them at the upgrade.

## 9. Recordings and scripts

| File (under `~/fleetdeck-briefs/backlog/resume-after-stop/`) | What |
| --- | --- |
| `recording-claude-stop1.md`, `watch_claude_stop1.log` | Claude stop: 4 session zombies for 1.02 s; row `exit_unverified`; resume refused |
| `recording-codex-stop1.md`, `watch_codex_stop1.log` | Codex stop: 2 session zombies for 1.02 s; MCP servers in their own groups; resume refused |
| `scripts/watch_exit.py` | Runs the census of `inspect.rs` from the leader's exit and prints every pass |
| `scripts/waitpid_zero.py`, `scripts/waitpid_zero.log` | `waitpid(0)` does not reap an orphan of another group; `waitpid(-1)` does |
| `scripts/census_churn.py` | Machine-wide churn of `/proc` passes |

The sessions of the reproduction (`01M5MRC26K9K51A4MCJH6CY638`, `01M5MS2NS6E3DMRNRD0Z2VCTKD`) were stopped; their rows stay `exit_unverified` (evidence).

## 10. Changes to other documents (by the build tasks)

- `shim-resume.md`: §1a gets the fourth proof (`checked_after_exit`, DL-432); §5 "a graceful stop gives `exited` …" holds after T1 and T2.
- `board-gaps.md` §5.5: a note that "Stop them and resume" (DL-433) marks the run as ended as part of one owner action.

## 11. Decision log entries (to append)

## DL-430: B13 root cause: the shim's exit reaper did not reap the provider's orphans (architect, 2026-10-23)

- Context: B13; design `docs/v2/design/resume-after-stop.md` §1. A resume after Stop failed with `exit_unverified` until the owner clicked "Mark as ended".
- Cause: `reap_orphans` used `rustix::process::waitpid(None, NOHANG)`, which is `waitpid(0)`: only children in the shim's own process group. The provider's orphans are in the provider's group, so their zombies stayed in the provider's session and made every census pass unclean for the 1 s exit budget. Reproduced on Lighting for Claude and Codex.
- Decision: the exit path reaps with `rustix::process::wait(NOHANG)` (`waitpid(-1)`), after tokio reaped the provider.

## DL-431: The shim stops the leftover members of a run before the exit report, with the leader pinned (architect, 2026-10-23)

- Context: design §3.2. A live member at the exit (a slow MCP server, a background command; Codex MCP servers run in their own groups) leaves `exited_descendants_remaining`.
- Decision: a dedicated thread waits for the exit with `waitid(P_PID, WEXITED | WNOWAIT)` without reaping, so the zombie leader keeps its pid number; the shim then gives members 2 s, sends SIGTERM (the group through `kill(-L)`, other session members through verified pidfds), 2 s, SIGKILL (at most 3 rounds), counts again, and only then reaps the leader. Every signal of the provider (stop, Kill, Interrupt) goes through one lifecycle gate: numeric signals only while the leader is unreaped and running; a Kill during the sweep escalates the sweep; nothing after the reap. When the wait fails, the shim sends no numeric signal and reports its census unchanged (revision 2).

## DL-432: The station proves an unverified exit by its own check at resume (architect, 2026-10-23)

- Context: design §3.3. Shims of an older build keep the bug of DL-430 until they end; a busy machine can still leave an incomplete exit census.
- Decision: before the decision of `session.resume`, of an input to an ended session, and of the auto-continue of a lost session at the station start, the station runs the check of "Mark as ended" for each `exited` row whose effective reason is `exit_unverified`. A complete check on the same boot with no live identity and no group or session member records `station_proof = 'checked_after_exit'` (new column, schema V44) and the event `session.execution_reconciled { by: station }`. `lost` and `exited_descendants_remaining` rows stay owner-only. The termination standard of DL-182 §1a does not change: this is the clean-exit proof, taken later.

## DL-433: A refused resume opens the check; "Mark as ended and resume" and "Stop them and resume" (architect, 2026-10-23)

- Context: design §3.4.
- Decision: when the station refuses a resume with "not_resumable: the previous execution may still run", the board opens the previous-run dialog for the owner in resume mode, with "Mark as ended and resume" (nothing lives) or "Stop them and resume" (BG-4 plan and elevation, then a fresh check, the owner mark and the resume). The combined stop action marks the run as ended; this changes BG-4 §5.5 for this action only. The dialog gets the `.fd-dialog-head`.

## DL-434: Compatibility of B13 (architect, 2026-10-23)

- Decision: no feature flag. The exit report wire does not change. `ExecutionReconciled` gets an optional `by` (`owner`, `station`, unknown values of newer peers). An older station binary refuses the V44 database (`DbError::TooNew`); the recovery is forward repair, or the restore of the V43 pre-upgrade backup (`operations.md` §13.6), which loses station writes since the upgrade. Shims that an older build launched are covered by DL-432.

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. When the agent's process ends, should the shim stop every process that is still in the agent's session (MCP servers, background commands that did not detach), and not only the agent's process group as today? — **The recommended default: Yes, at every end of a run (Stop, Force stop, crash, hibernation).**
2. How long may a leftover process end by itself before Fleet Deck asks it to stop? — **The recommended default: 2 s, then SIGTERM and 2 s more, then SIGKILL.**
3. After the elevation, may 'Stop them and resume' also mark the run as ended in the same click? The confirmation shows the warning 'Fleet Deck cannot see a process that left its process group'. — **The recommended default: Yes, one action.**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.

## Revision 2 (design review round 1)

Review: `~/fleetdeck-briefs/backlog/resume-after-stop/adv-design-r1.md` (verdict FAIL, reviewed commit 00055138). The deputy architect checked each finding against the code at 00055138 before the change. All five findings are confirmed and fixed. No finding stays open.

| # | Severity | Finding | Verified against | What changed |
| --- | --- | --- | --- | --- |
| 1 | BLOCKER | The legacy fallback reaps the leader, then sends `kill(-L)`; a reused `L` can be a foreign group. | `server.rs:382-419` (reap, census, `signal_group(KILL)`); `server_proc.rs:160-183`. | The legacy order is removed. The leader is pinned on every Linux kernel by a dedicated thread with `waitid(P_PID, WEXITED \| WNOWAIT)` (§3.2.1). When that wait fails, the unpinned path (§3.2.3 U1-U5) sends no numeric signal and reports its census as it is. New regression tests `leader_wait_after_a_foreign_reap_fails` and `unpinned_exit_sends_no_numeric_signal` (T2). §1.2 fact 9, §3.5 kernel row, §3.6 case 12, §4, decisions 4 and 6, DL-431 updated. |
| 2 | BLOCKER | Late `Kill` and `Interrupt` frames (and `stop()`) signal the numeric `L` after the reap, for example while the output waits for acks. | `server_conn.rs:223-234`; `server.rs:729`, `:736`; `wait_stdout_end` at `server.rs:441-467`. | New `Leader` lifecycle gate (§3.2.2): numeric signals only in `Running`; a Kill in `Ended` escalates the sweep (its kill runs while `L` is pinned); `Unpinned` uses only the leader pidfd; `Reaped` sends nothing. `set(Reaped)` and every send share one mutex, and the reap starts after `set(Reaped)`. T2 now owns `server_conn.rs` and the new `server_leader.rs`. Tests: the `leader_*` spy tests, `late_kill_and_interrupt_after_the_reap_change_nothing` (output blocked on acks), `a_kill_during_the_sweep_skips_the_grace`. A Kill after the provider ended no longer changes the exit cause (decision 21). §1.2 fact 11, §3.6 cases 10-11, §4 updated. |
| 3 | MAJOR | The promised binary rollback cannot open the V44 database. | `fleetdeck-core/src/db.rs:220-228` (`DbError::TooNew`); `store/migrations.rs:644` (V43 last); `operations.md` §13.6. | The rollback claim is replaced by the supported recovery: forward repair, or the restore of the V43 pre-upgrade backup (§3.5, §4, DL-434, decision 19). The schema number is V44 everywhere (`schema_v44.rs`, `store/mod.rs` added to T3). New test `v44_database_is_too_new_for_the_v43_list`; the live smoke checks the real backup on Lighting (`user_version` 43, `integrity_check` ok). |
| 4 | MAJOR | A working `pidfd_open` does not prove that `waitid(P_PIDFD)` works (Linux 5.3 vs 5.4); the `AsyncFd` and `waitid` errors were not specified. | rustix 1.1.5 `process/wait.rs` (`WaitId::Pid`, `WaitIdOptions::NOWAIT`), `process/pidfd.rs`. | The waiter does not use a pidfd, `AsyncFd` or `P_PIDFD` any more: `waitid(P_PID, WNOWAIT)` (Linux 2.6.9) on a dedicated thread, with `EINTR` retried, every other error and a closed channel → `LeaderWait::Failed` → the unpinned path (§3.2.1). The pinned census runs only after `LeaderWait::Ended`. The errors of the member pidfd path are specified (§3.2.3 E2). The leader pidfd is optional and used only in `Unpinned` (decision 20). Tests: `leader_wait_keeps_the_zombie`, `leader_wait_after_a_foreign_reap_fails` (the injected failure is a real foreign reap), the `leader_unpinned_*` spy tests. |
| 5 | MAJOR | Auto-continue bypasses the settle in `submit`, so an eligible same-boot unverified exit is still skipped. | `manager/mod.rs:322`; `auto_continue.rs:106-107`, `:229-232`; test `auto_continue_shim_tests.rs:276`. | Auto-continue settles each candidate before its decision at the station start: `evaluate_at_start` → `evaluate_settling` with the shim host's boot reader; the auto-continue rules do not change (§3.3 "When it runs", item 2). The settle is a free function shared with `submit` (`pre_decision`). T3 now owns `auto_continue.rs` and `auto_continue_shim_tests.rs`; new tests `auto_continue_settles_an_unverified_exit_of_the_same_boot`, `auto_continue_off_is_not_settled`, `auto_continue_without_a_boot_reader_does_not_settle`; the existing refusal test stays. `mod.rs` (797 lines) does not grow. Q3, §1.2 fact 14, §4, decision 10, DL-432 updated. |

Other changes in this revision: the base is 00055138; T2 moves `on_exit` to `server_exit.rs` and the census additions to `inspect_exit.rs` to keep the 800-line rule (decision 22); the T2 and T3 acceptance filters cover the new tests; T2 acceptance adds a `grep` that limits the callers of `signal_group`.

Findings not fixed: none.
