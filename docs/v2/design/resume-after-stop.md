# Resume after Stop without manual steps (B13)

- **Status:** Draft, revision 1 (2026-10-23). Risk: **high** (it changes the termination proof of DL-182 and the process-termination path of the shim). It needs an adversary DESIGN REVIEW before the build, and a PRE-PR REVIEW before the merge.
- **Owner report (B13, 2026-10-22):** the owner stops a session and then resumes it. The resume fails. The owner must open "Check the previous run" and click "Mark as ended". After that, the resume works.
- **Wanted:** Stop and then Resume work without manual steps. The station does the check and the reconciliation itself when it is safe. The owner sees the manual dialog only when the station cannot decide.
- **Base:** `v2` at 497b2fbc. All `file:line` references are on that commit.
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
9. The exit path also has an unsafe fallback: `signal_group` sends the signal to the single pid when the group is gone (`server_proc.rs:175-181`). At `server.rs:419` the provider is already reaped, so that pid number is free and can belong to an unrelated process.
10. BG-4 ("Stop previous run processes", `docs/v2/design/board-gaps.md` §5) stops processes of a lost run through pidfds, after an elevation, and says "Do not Mark as ended automatically after a stop" (`board-gaps.md:783`).

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

**Q3. Can Resume run the check first?** Yes. The station runs the settle check of §3.3 inside `submit`, before the decision of `session.resume` and of an input that resumes a hibernated session. The error shows only when the check finds a live process, is incomplete, or the row is not eligible.

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
- This task changes no wire, no row and no station code.

### 3.2 The exit sweep with a pinned leader (shim)

Goal: before the exit report, the shim ends every live member of the provider's group and session, and counts again. The report then gives `Some(0)` in the normal case also when an MCP server or a background command is slow to end.

**Safety rule: the leader stays a zombie until the sweep ends.** While the provider's zombie exists, Linux keeps its pid number allocated; no new process can get that number, so no new process group and no new session can have the number `L` (a new group or session id is the pid of its creator). Every process with `pgrp == L` or `session == L` is therefore a member of this run. The shim reaps the leader only after the sweep. This is the BG-4 witness case (a) "the leader is alive" (`board-gaps.md` §5.3), with the zombie as the anchor.

**New exit sequence (Linux, when `pidfd_open` works):**

1. Right after the spawn, the shim opens a pidfd for the provider: `rustix::process::pidfd_open(pid, PidfdFlags::empty())`. On an error (`ENOSYS`, other), `pinned = None` and the shim uses the legacy sequence (step 9).
2. `run` waits for the exit without reaping: `tokio::io::unix::AsyncFd::new(pidfd)` → `readable().await` (a pidfd is readable when its process has terminated), then `rustix::process::waitid(WaitId::PidFd(fd.as_fd()), WaitIdOptions::EXITED | WaitIdOptions::NOWAIT)` for the status. The termination text is the same as today: `code:<n>`, `signal:<n>` or `unknown`.
3. `exited` is set (as today, `server.rs:383`), so `stop()` ends its waits.
4. On a blocking thread (new function `exit_sweep` in a new file `server_exit.rs`):
   1. Reap the zombie children except the leader: `reap_orphans_except(Some(leader))`.
   2. Census with the pinned leader (§3.2.1). Result `Some(0)` → go to 4.6.
   3. `Some(n > 0)`: wait for the members to end by themselves. Every 50 ms: reap (except the leader) and count again, for at most `MEMBER_GRACE` = 2 s. When the stop cause is `killed` (a Kill stop), the grace is 0.
   4. Members remain: send SIGTERM. The group: `kill_process_group(L, TERM)` (safe: `L` is pinned). Each live session member whose `pgrp != L`: `pidfd_open(pid)`, then read `/proc/<pid>/stat` again, then `pidfd_send_signal(TERM)` only when the read shows `session == L` and the same start time as the census read. Wait up to `MEMBER_TERM_GRACE` = 2 s with the same 50 ms count loop.
   5. Members remain: SIGKILL in the same way, to every member of a fresh census (new members are members of this run too), at most 3 rounds 100 ms apart.
   6. The final census (budget 1 s) gives `descendants_remaining`.
5. `kill_process_group(L, KILL)` once more (today's `server.rs:419`), **without** the single-pid fallback of `signal_group`.
6. The shim reaps the leader: `child.wait().await` (it returns at once; tokio keeps the status).
7. `reap_orphans()` (the fixed `waitpid(-1)` loop of §3.1).
8. The rest is as today: `wait_stdout_end`, the exit report, the publication.
9. **Legacy sequence** (no pidfd): `child.wait()`, the fixed reaper, the census as today (no pinned leader, no session sweep), then `kill_process_group(L, KILL)` without the single-pid fallback. The report is honest: a zombie or a member gives `None` or `Some(n)`.

The longest exit path is about 1 + 2 + 2 + 0.3 + 1 ≈ 6.3 s (a member that ignores SIGTERM). A run with no live member at the exit is as fast as today, or faster (no 1 s of unclean passes).

#### 3.2.1 The census with a pinned leader

- New parameter of the census: `pinned: bool`. With `pinned = true`, the zombie of the leader itself does not make a pass unclean. Reason: the leader was a zombie before the first listing (step 2 confirmed it), so it cannot fork during any pass. Every other rule of a clean pass stays (`inspect.rs:157-168`): a vanished pid or a zombie member makes the pass unclean, and a live member is final at once.
- New public function in `inspect.rs`: `pub fn exit_census(leader: u32, pinned: bool, reap: &mut dyn FnMut()) -> Option<u32>` (the `group_members_reaping` of today becomes `exit_census(leader, false, reap)`). It returns the count of live members (the leader excluded) and a list of their `(pid, pgrp, session, start_time)` for the sweep: `pub fn exit_members(...) -> Option<Vec<ProcStat>>`.

#### 3.2.2 What does not change

- `INSPECTION` stays 1 and the wire does not change (`wire.rs`). The meaning of `Some(0)` is the same: a clean pass with zero live members.
- The stop policy (`StopPolicy`, the 30 s grace and the 5 s TERM grace of the leader) does not change. The sweep starts only after the leader ended.
- Escaped descendants (left both the group and the session) are not touched. A member that escapes between the check and the signal can still get the signal (the same accepted limit as BG-4 §5.4).

### 3.3 The station settle check (station)

**When it runs.** In `SessionManager::submit` (`manager/mod.rs:594`), after `stop_prepare` (`:624`, which holds the per-session lock for `session.resume`) and next to `reconcile_check` (`:628`):

- `SessionCommand::Resume { session_id }` (board Resume, "Resume all", auto-continue, fleet `resume_session`): settle that session under the lock that `stop_prepare` holds.
- A command of `restart_gate::may_resume` (`Prompt`, `FollowUp`, `Steer`, `ShellCommand`, `restart_gate.rs:179-187`) for a session whose summary state is `exited` or `lost` (a hibernated session): take `self.target.stops().locks.hold(session)` for the settle only.
- No other command. `session.import` keeps the owner path (out of scope, §8).

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

**Schema (the next free version; V43 on 497b2fbc).** Forward only, like V23 (`store/migrations.rs:553-572`):

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
| Rollback of the station binary after V43 | The older binary does not read `station_proof`: a settled row is unproven again (fail closed); the owner can still mark it. The CHECK and the columns stay. |
| Older deck | The deck passes `by` through (unknown fields are kept, `event.rs:6-8`). |
| Older board | It ignores `by`. Without the board task, the owner still gets the settle at resume (no dialog). |
| macOS station | The shim host refuses on macOS (DL-182 status line); the settle check is incomplete there (`check_row`, `NO_INSPECTION`): nothing changes. |
| Kernel without pidfd | The legacy exit sequence (§3.2 step 9) with the fixed reaper. |

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

## 4. Risk class and threat notes

**Risk: high.** The design changes the termination proof of DL-182 (a new proof kind that no owner gives) and the process-termination path of the shim (new signals to session members), and it adds a schema migration.

| Threat | Why it holds |
| --- | --- |
| The station proves an end while a provider of the run still runs, and a second provider starts on the same conversation | The settle needs: the row is `exited` (the shim saw the leader end through `waitpid`), a complete check on the same boot, no live process with the stored identity (pid + start time), and no live member of the group or session. Membership of a group or session comes only from a fork of a member; when a clean pass sees no member, no member can appear later. A later process that reuses the number `L` makes a **new** group or session, which the check counts as a member: a false "still runs" (fail closed), never a false proof. |
| An escaped descendant (left the group and the session) writes to the resumed conversation | Out of scope of every proof since DL-182 §1a, and the same on the direct host. The settle accepts it without the owner's click, as the clean exit proof does today. The owner path keeps its warning. The design does not make it worse than a clean exit. |
| A member alive at the exit escapes before the later check | The same as a member that escapes just before the exit census. With the sweep (§3.2), the members that the shim sees are stopped before the report. A row with live members (`exited_descendants_remaining`) is never settled by the station. |
| The sweep signals an unrelated process | The leader is not reaped until the sweep ends, so the number `L` cannot be reused: every process with `pgrp == L` or `session == L` belongs to the run. A session member in another group is signalled only through a pidfd, after a fresh read shows `session == L` and the same start time. No numeric single-pid signal is sent after the reap (the fallback of `server_proc.rs:175-181` is not used in the exit path). |
| The sweep kills something that the owner wanted to keep (a dev server that the agent started) | A process of the agent's session ends with the agent. Today the shim already kills the leader's group at every exit (`server.rs:419`); only session members in other groups survived. A process that must outlive the agent must leave the session (`setsid`); then it is an escaped descendant and is not touched. Owner question 1. |
| A long exit path blocks the station | The longest path is about 6.3 s and runs on a blocking thread of the shim; the station waits for the report as it does for any exit. A takeover during the path attaches as today. |
| A wrong automatic proof cannot be undone | The proof is a separate column; state and reason stay as evidence. An audit row (the event with `by: station`) names the row. |
| Rollback | An older station ignores the column and refuses (fail closed). |
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

### T2. The exit sweep with a pinned leader (backend, ~550 lines)

- **Owned files:** `crates/fleetdeck-providers/src/host/shim/server.rs`, `crates/fleetdeck-providers/src/host/shim/server_exit.rs` (new), `crates/fleetdeck-providers/src/host/shim/server_proc.rs`, `crates/fleetdeck-providers/src/host/shim/inspect.rs`, `crates/fleetdeck-providers/src/host/shim/inspect_pinned_tests.rs` (new), `crates/fleetdeck-testkit/src/sim_codex.rs`, `crates/fleetdeck-cli/tests/shim_manager_parts/live_exit.rs`, `crates/fleetdeck-cli/tests/shim_exit_sweep.rs` (new).
- **Changes:**
  1. `server.rs` `serve`: open the pidfd after the spawn (§3.2 step 1); keep it in `Shim` as `pinned: Option<OwnedFd>`.
  2. `server.rs` `run`: wait through the pidfd and `waitid(NOWAIT)` (step 2); then `exited`; then `on_exit`; then `child.wait()` (step 6). Without a pidfd: the legacy order.
  3. `server.rs` `on_exit`: call `server_exit::exit_sweep(leader, pinned, killed)` on a blocking thread; use its count; replace `signal_group(KILL)` with a group-only kill (no single-pid fallback).
  4. `server_exit.rs`: `exit_sweep`, the grace and signal loops of §3.2 step 4, constants `MEMBER_GRACE = 2 s`, `MEMBER_TERM_GRACE = 2 s`, `KILL_ROUNDS = 3`, `SWEEP_POLL = 50 ms`; the pidfd open, recheck and send for session members; a group-only `kill_group(L, sig)`.
  5. `server_proc.rs`: `pub(super) fn kill_group_only(pid, sig)` (no `SRCH` fallback); `group_members` calls the new census entry.
  6. `inspect.rs`: the `pinned` parameter of `census` (§3.2.1), `exit_census`, `exit_members`; `group_members_reaping` delegates to `exit_census(leader, false, reap)`. Tests in `inspect_pinned_tests.rs` (the file is at 710 lines).
  7. `sim_codex.rs`: new modes `exit-leaving-stubborn-child <text>` (a child `sh -c 'trap "" TERM; sleep 30'` in the app-server's group) and `exit-leaving-group-child <text>` (a child `sleep 30` with `process_group(0)`: another group, the same session).
  8. `live_exit.rs`: the test `a_live_shim_exit_with_a_surviving_child_keeps_the_evidence_and_resume_stays_refused` becomes `a_live_shim_exit_with_a_surviving_child_stops_it_and_the_resume_is_admitted`: the row is `exited` / `exited`, the `sleep` child is gone, and the resume is accepted.
- **Tests:**
  - `inspect_pinned_tests.rs`: `a_pinned_zombie_leader_does_not_make_a_pass_unclean` (fake `/proc`: zombie leader, no member → `Some(0)` with `pinned`, `None` without); `a_zombie_member_still_makes_a_pinned_pass_unclean`; `a_live_member_is_counted_with_a_pinned_leader`.
  - `shim_exit_sweep.rs` (real processes, the `shim_rig` of `fleetdeck-cli/tests`): `a_member_that_outlives_the_provider_is_stopped_before_the_report` (`sleep 30 & echo done` → `Some(0)`, the `sleep` pid is gone); `a_member_that_ignores_term_is_killed` (`sh -c 'trap "" TERM; sleep 30' & echo done` → `Some(0)`, report within 7 s); `a_session_member_in_another_group_is_stopped` (a `setsid`-free child in a new group via `sh -c 'set -m; sleep 30 &'` → `Some(0)`); `an_escaped_process_is_not_signalled` (`setsid sleep 30 & echo done` → `Some(0)`, the escaped pid still lives, the test kills it at the end); `the_leader_status_is_kept` (`exit 3` → `code:3`).
  - `live_exit.rs`: the changed test above; `a_live_exit_with_a_child_in_another_group_is_proven` with `exit-leaving-group-child`.
- **Depends on:** T1.
- **Acceptance:**
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(pinned) | test(reap_orphans) | test(census)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(shim_exit_sweep) | binary(shim_resume_process) | binary(shim_process) | binary(shim_manager)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T3. The station settle check at resume (backend, ~450 lines)

- **Owned files:** `crates/fleetdeck-proto/src/event.rs`, `crates/fleetdeck-proto/src/event_tests.rs`, `web/src/gen/event/Event.ts`, `web/src/gen/event/ReconciledBy.ts` (new, generated), `web/src/gen/index.ts` (only when the generator adds the export), `crates/fleetdeck-station/src/store/schema_v43.rs` (new; the number the architect assigns), `crates/fleetdeck-station/src/store/migrations.rs`, `crates/fleetdeck-station/src/store/shims.rs`, `crates/fleetdeck-station/src/store/resumes.rs`, `crates/fleetdeck-station/src/store/resumes_tests.rs`, `crates/fleetdeck-station/src/manager/manager_execution.rs`, `crates/fleetdeck-station/src/manager/mod.rs`, `crates/fleetdeck-station/src/manager/shims.rs`, `crates/fleetdeck-station/src/manager/shim_settle_tests.rs` (new), `crates/fleetdeck-station/src/manager/shim_resume_check_tests.rs`, `crates/fleetdeck-cli/tests/shim_resume_recovery.rs`.
- **Changes:**
  1. Proto: the `by` field and `ReconciledBy` (§3.3); the TS files regenerated with the project's ts-rs export (the same command that made `web/src/gen`).
  2. Every `ExecutionReconciled { shim_id }` pattern or constructor gets `by` or `..` (`manager/shims.rs:656` sets `by: None`; the two tests).
  3. Schema V43 (§3.3) and its migration test (`v43_adds_the_station_proof_columns`).
  4. `store/shims.rs`: the two fields in `ShimRow`, `COLUMNS`, the decode.
  5. `store/resumes.rs`: `settle_eligible`, `proves_end`, `UNRESOLVED`, `shim_settle` (§3.3).
  6. `manager_execution.rs`: `pub(super) async fn settle_unverified_exits(&self, session: &SessionId)` (§3.3, logs, never fails).
  7. `manager/mod.rs` `submit`: call it after `stop_prepare` for `Resume`, and for `may_resume` commands of an ended session with its own lock hold (§3.3).
- **Tests (`shim_settle_tests.rs`, with `shim_resume_rig.rs`):**
  - `a_resume_after_an_unverified_exit_with_nothing_alive_is_admitted_and_records_the_station_proof` (row `exited/exit_unverified/1` with a dead identity on the real boot → accepted; `station_proof = checked_after_exit`; one `ExecutionReconciled { by: Some(Station) }`).
  - `a_live_group_member_keeps_the_resume_refused` (a test child in the identity's group, as test 8 of DL-182 does) → refused, no proof.
  - `an_exit_with_descendants_remaining_is_not_settled_by_the_station` → refused.
  - `a_lost_row_is_not_settled_by_the_station` (`lost/execution_unknown`) → refused.
  - `a_row_of_another_boot_is_not_settled_by_the_check` → refused, no station proof.
  - `a_legacy_exit_without_inspection_is_settled` (`exited/exited/0`) → accepted.
  - `a_prompt_to_a_hibernated_session_settles_and_resumes` → accepted.
  - `the_owner_reconcile_still_writes_no_by_field` (the event JSON has no `by`).
  - `resumes_tests.rs`: `proves_end_accepts_a_station_proof`; `unresolved_skips_a_settled_row`.
  - `event_tests.rs`: `execution_reconciled_reads_without_by_and_with_an_unknown_by`.
- **Depends on:** T1 (order of merge only; no shared file).
- **Acceptance:**
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(execution_reconciled)'`
  - `sh tools/heavy.sh cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(settle) | test(proves_end) | test(unresolved) | test(v43) | test(shim_resume)'`
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

## 6. Decisions made in this draft

1. **The cause is fixed in the shim first** (T1), as a small separate task. Rejected: only a station-side settle, which would hide the bug and keep a 1 s unclean census at every exit.
2. **`waitpid(-1)` in the exit path**, not `reap_orphans_except(None)`. The provider is already reaped there, and the kernel call is complete without `/proc`. Rejected: the `/proc`-based reaper, which depends on `CONFIG_PROC_CHILDREN` or a full scan.
3. **The shim stops leftover members of the group and the session at every exit** (natural, Stop, Kill, hibernation), before the report. Rejected: only after a Stop (a crash leaves the same orphans; the group kill of today already runs at every exit).
4. **The leader stays a zombie until the sweep ends** (pidfd + `waitid(NOWAIT)`). Rejected: a sweep after the reap with numeric ids (a reused number could be a foreign group or session) and a sweep without session members (Codex MCP servers are in other groups).
5. **Session members outside the leader's group are signalled one by one through pidfds**; the leader's group through `kill(-L)` while `L` is pinned. Rejected: `kill(-G)` for each other group `G` (a number of a group can be reused once the group is empty).
6. **The single-pid fallback of `signal_group` is not used after the reap.** Rejected: keeping today's fallback at `server.rs:419`.
7. **Timings:** members get 2 s to end by themselves, then SIGTERM and 2 s, then SIGKILL in at most 3 rounds; no grace on a Kill stop. Rejected: no grace (an MCP server gets no chance to flush) and the 30 s grace of the leader (the report would wait too long).
8. **The census budget at the exit stays 1 s.** With the reaper fixed, a busy machine gives clean passes in milliseconds (§1.3). Rejected: a longer budget.
9. **The station settles only `exited` rows with the effective reason `exit_unverified`.** Rejected: settling `exited_descendants_remaining` (the station saw live processes) and `lost` rows (the station saw no exit; `attach_refused` and `identity_mismatch` can mean a live shim).
10. **The settle runs at resume time only** (the `submit` of `session.resume` and of an input to an ended session). Rejected: a background settle after every exit (more code paths, a check of rows that nobody resumes) and a sweep at station start (slow start with many rows).
11. **A new column `station_proof`**, not a new value of `proof`. The CHECK of `proof` (V23) cannot change without a table rebuild. Rejected: reusing `reconciled_by_owner` (a false audit) and a table rebuild (a riskier migration).
12. **The event `session.execution_reconciled` gets an optional `by`**, absent for the owner. Rejected: a new event kind (older boards and decks would carry an unknown kind for nothing) and `by: owner` on owner events (a wire change without need).
13. **No feature flag.** The station decides alone, and the event change is additive.
14. **`session.import` keeps the owner path.** Rejected: a settle of other sessions' rows at import (rare, and it widens the change).
15. **The board detects the refusal by its text prefix** "not_resumable: the previous execution may still run". Rejected: a new `RejectCode` (an older deck maps it to `unknown`) and a new structured field.
16. **"Stop them and resume" includes "Mark as ended"** in one owner action, with the warning in the confirmation. This changes BG-4 §5.5 ("Do not Mark as ended automatically after a stop") for this combined action only; the separate "Stop these processes…" of the menu dialog keeps the BG-4 rule. Rejected: a stop that then shows a second button.
17. **The dialog gets `.fd-dialog-head` and loses its text Close button** (a design-system rule that the dialog breaks today).
18. **The board does not render the reconciliation event** in the timeline.
19. **Schema number:** V43 on 497b2fbc; the architect assigns the number when other tracks also add migrations.

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
- Decision: the shim waits for the exit through a pidfd without reaping, so the zombie leader keeps its pid number; it then gives members 2 s, sends SIGTERM (the group through `kill(-L)`, other session members through verified pidfds), 2 s, SIGKILL (at most 3 rounds), counts again, and only then reaps the leader. No single-pid signal after the reap. Without a pidfd, the legacy order with the fixed reaper.

## DL-432: The station proves an unverified exit by its own check at resume (architect, 2026-10-23)

- Context: design §3.3. Shims of an older build keep the bug of DL-430 until they end; a busy machine can still leave an incomplete exit census.
- Decision: before the decision of `session.resume` and of an input to an ended session, the station runs the check of "Mark as ended" for each `exited` row whose effective reason is `exit_unverified`. A complete check on the same boot with no live identity and no group or session member records `station_proof = 'checked_after_exit'` (new column, schema V43) and the event `session.execution_reconciled { by: station }`. `lost` and `exited_descendants_remaining` rows stay owner-only. The termination standard of DL-182 §1a does not change: this is the clean-exit proof, taken later.

## DL-433: A refused resume opens the check; "Mark as ended and resume" and "Stop them and resume" (architect, 2026-10-23)

- Context: design §3.4.
- Decision: when the station refuses a resume with "not_resumable: the previous execution may still run", the board opens the previous-run dialog for the owner in resume mode, with "Mark as ended and resume" (nothing lives) or "Stop them and resume" (BG-4 plan and elevation, then a fresh check, the owner mark and the resume). The combined stop action marks the run as ended; this changes BG-4 §5.5 for this action only. The dialog gets the `.fd-dialog-head`.

## DL-434: Compatibility of B13 (architect, 2026-10-23)

- Decision: no feature flag. The exit report wire does not change. `ExecutionReconciled` gets an optional `by` (`owner`, `station`, unknown values of newer peers). A station rollback ignores `station_proof` and refuses (fail closed). Shims that an older build launched are covered by DL-432.

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. When the agent's process ends, should the shim stop every process that is still in the agent's session (MCP servers, background commands that did not detach), and not only the agent's process group as today? — **The recommended default: Yes, at every end of a run (Stop, Force stop, crash, hibernation).**
2. How long may a leftover process end by itself before Fleet Deck asks it to stop? — **The recommended default: 2 s, then SIGTERM and 2 s more, then SIGKILL.**
3. After the elevation, may 'Stop them and resume' also mark the run as ended in the same click? The confirmation shows the warning 'Fleet Deck cannot see a process that left its process group'. — **The recommended default: Yes, one action.**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.
