# Resume on the shim host (DL-182)

- **Status:** Proposed (revision 1, 2026-10-14). Extends ADR-005 (revision 3, accepted) §4.6 and §6. Needs an adversary DESIGN REVIEW PASS before code.
- **Owner request (2026-10-14):** "do 2. The real fix: build resume on the shim host." The owner's station runs `host = "shim"`; every resume ends with `start_failed: resume is not supported on the shim host yet` (DL-118).
- **Unchanged:** every ADR-005 rule (launch journal, attach handshake, identities, cursors, stdin settlement, exit acknowledgment, recovery order). A resume starts a **new** shim and a **new** provider process; it never reattaches, and it is shown as `resumed`, never `reattached` (ADR-005 §6).

## 1. Facts today

- The station decision already admits `session.resume` only when every earlier shim execution of the session is proven ended (`Decision::unended_execution`, `crates/fleetdeck-station/src/store/resumes.rs:121-152`): each `shims` row is `never_started`, or `exited` with reason `exited` (`PROVEN_EXIT`: the shim reported the provider's exit with no remaining member of its process group or session, `descendants_remaining == 0`, `manager/shims.rs:46`).
- The dispatch then refuses every resume on the shim host (`manager/target.rs:270-276`, `REASON_RESUME_UNSUPPORTED`), because no durable resume launch exists: `ProviderAdapter::durable_launch(spec)` builds only a new session (`claude/durable_launch.rs`: `Launch.resume = None`, `CoreConfig.resumed = None`; `codex/durable_launch.rs`: `thread.resume = None`).
- Recovery never uses the boot id: a row whose shim is gone without a valid exit file becomes `lost` (`execution_unknown`), also after a reboot, so a reboot leaves every shim session unresumable. ADR-005 §4.6 names a verified reboot as sufficient termination evidence and an audited owner reconciliation as the other path; neither is built.
- Hibernation (`manager/hibernate.rs`) stops idle sessions and relies on resume, without a host check.

## 2. Durable resume launch (providers)

- New `ProviderAdapter` method `durable_resume_launch(&self, spec: &SessionSpec, native_id: &NativeSessionId) -> Result<DurableLaunch>`; the default returns `ProviderError::Unsupported("durable resume")`.
- Claude: the same as `launch_durable` with `Launch { resume: Some(native_id), fork: false, .. }` (so `cli_args` adds `--resume <id>`, exactly as the direct resume) and `CoreConfig.resumed = Some(true)`.
- Codex: the same as `launch_durable` with `thread.resume = Some(native_id)`, `fork = false` (the durable handshake sends `thread/resume`), and `CoreConfig.resumed = Some(true)`.
- The mode, model, capability and workspace rules are those of `launch_durable` (a mode the instance does not offer fails before any process). The resumed spec is the stored spec (DL-118), never more permissive.
- The native id is not secret; it goes into argv for Claude as the direct resume already does.

## 3. Station dispatch

In `SessionTarget::start` with `resume = Some(native)` and a shim host:

1. **Recheck termination at dispatch:** read `unended_execution(session)` again (a store read, no decision). `Some(why)` → `start_failed("the previous execution is not proven ended: <why>")`, `NotDispatched("execution_not_ended")`. This closes the window between the decision and the dispatch.
2. `adapter.durable_resume_launch(spec, native)`: an error → `start_failed(<error>)`, `NotDispatched("start_failed: …")`. No fallback to a direct-host resume (a session keeps the host of its station, ADR-005 §2).
3. `start_durable(effect, spec, prompt, shims, launch, cancel)` exactly as a new durable start: a new `shim_id`, the `starting` row, the journal, the attach, the driver, then the first input (the resume's prompt, if any). The new row is the session's newest shim row; earlier rows stay as they are (their termination is what the decision checked).
4. The session tools (DL-165) follow the shim-host rule of a new durable start (today: `prepare_fleet` with `durable = true` gives "no session tools: durable host" and the info log).
5. `REASON_RESUME_UNSUPPORTED` is removed; the `start_failed` text "resume is not supported on the shim host yet" no longer exists.

The same path serves every resume: the board's Resume and "Resume all", a hibernated session's input (DL-180), the TUI handover's return, auto-continue (ADR-007, when enabled) and import (`session.import` with a native id on a shim station).

## 4. Termination evidence after a shim loss

### 4.1 Verified reboot (ADR-005 §4.6)

- At recovery (ADR-005 §3.2), before a row is classified `lost` with `execution_unknown` (shim socket gone, no valid exit file, or the attach failed because the shim is gone), the station compares the row's stored `provider_identity.boot_id` with the current boot id (Linux `/proc/sys/kernel/random/boot_id`; macOS `kern.bootsessionuuid`, the same reader as `host/shim/identity.rs`).
- Different, both non-empty and readable → the row becomes `exited` with the new reason `ended_by_reboot`; its open effects follow the exit rules (claimed effects `outcome_unknown` as for a lost shim; the session ends with `session.ended { reason: "station_reboot" }`).
- Equal, empty, missing (a row in `starting` without an identity) or unreadable → unchanged: `lost`, `execution_unknown` (fail closed).
- `unended_execution` accepts `exited` with reason `exited` **or** `ended_by_reboot` (new constant `PROVEN_BY_REBOOT`).
- A row that the station already recorded as `lost` before this build: at the first recovery of this build, the same comparison runs for every `lost` row with `execution_unknown` whose session is not running; a match of the rule moves it to `ended_by_reboot` (the boot id cannot go back, so a later reboot keeps it true).

### 4.2 Owner reconciliation (ADR-005 §4.6)

For a session whose newest shim row is `lost` (`execution_unknown`, `attach_refused`, `identity_mismatch`, `exit_not_reported`) or `exited` with `exited_descendants_remaining`:

- **Check** (`GET /api/sessions/{station}/{session}/execution`, owner only; station method `session.execution_check`, non-mutating): the station runs the ADR-005 §4.6 session-and-group check for the stored identity: (a) a live process with the stored `provider_identity` (pid + start time + boot id), (b) live members of the provider's process group or session (Linux `/proc/*/stat` fields 5–6; macOS `proc_listpids(PROC_PGRP_ONLY)` and the session id). Answer: `{ shim_id, reason, same_boot: bool, identity_alive: bool, group_members: [{pid, command (first 120 chars, no args)}], session_members: [...], complete: bool }`. Any read error → `complete: false` and the error text. The check never kills anything.
- **Board:** a lost or descendants-remaining session shows "The previous process may still run" with the check result. With live members it lists them and says "Stop these processes on the station, then check again". With none and `complete: true` it offers "Mark the previous run as ended" with the warning "Fleet Deck cannot see a process that left its process group. Mark it ended only if you know that the agent is not running."
- **Mark ended** (`POST …/execution/reconcile { shim_id }`, owner only, CSRF, audited, station command `session.reconcile_execution { session_id, shim_id }`, mutating with a receipt): the station runs the check again inside the decision path; it refuses (`invalid_state`, stored) when `identity_alive` or any group or session member is live, or when the check is incomplete; else it moves that row to `exited` with reason `reconciled_by_owner` and appends `session.execution_reconciled { shim_id }`. `unended_execution` accepts this reason too (`PROVEN_BY_OWNER`). It never touches a row that is `starting` or `running`.
- Member and token principals get `403`; the action needs the owner, like other owner-only station actions. No elevation (it changes no policy and no credential; the audit row records it).

## 5. Hibernation

- `hibernate` candidates on a shim station: allowed, because a resume now works. A hibernation stop is graceful, so the shim reports the exit; a resume after it is admitted only when that exit is proven (`exited`, no descendants). When the exit leaves descendants (`exited_descendants_remaining`), the session is not resumable until §4.2, and the board shows that state instead of "Hibernated".
- Before this design ships, no hibernation change is needed for safety: the decision already refuses a resume without proof.

## 6. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| The earlier shim's exit is not proven | The decision refuses (`invalid_state`, with the shim row's state and reason), as today; the board shows §4.2. |
| Proof changes between decision and dispatch | §3 step 1 recheck → `execution_not_ended`, nothing launched. |
| The provider cannot resume the native id (deleted transcript, unknown thread) | The durable driver's open fails → the shim's provider is killed, the exit is committed, `start_failed` with the provider's message (the same as a failed durable start). |
| A station crash during the resume launch | ADR-005 §3.2 recovery of the new `starting` row (fence or attach); the session is then running, `never_started` (resumable again) or `lost`. |
| Reboot with an unreadable boot id | Fail closed: `execution_unknown`, §4.2 path. |

## 7. Tests

- Providers: `durable_resume_launch` for Claude (argv has `--resume <id>`, the stored mode, `resumed = Some(true)`) and Codex (`thread/resume` with the thread id in the handshake, `resumed = Some(true)`); an unoffered mode fails; the trait default is `Unsupported`.
- Station (shim host, the existing shim test harness with the fake agents, `crates/fleetdeck-cli/tests/shim_*.rs` style):
  1. A session that exited cleanly under a shim resumes under a new shim; the resume prompt is the first input; the board state is `resumed`; the old row stays `exited`.
  2. A Codex session does the same with `thread/resume`.
  3. The dispatch recheck: a test hook makes `unended_execution` non-empty between decision and dispatch → `execution_not_ended`, no shim launched.
  4. A resume of a session whose newest row is `exited_descendants_remaining` is refused at decision.
  5. A hibernated session on a shim station gets a follow-up → resumed under a new shim (DL-180 path).
  6. Recovery with a different boot id (test override of the boot-id reader) moves a gone shim row to `ended_by_reboot` and the session becomes resumable; with the same boot id it stays `execution_unknown`; with an unreadable boot id it stays `execution_unknown`.
  7. An old `lost` row with `execution_unknown` and a different boot id is moved at the first recovery.
  8. `session.execution_check` reports a live group member (a test child that stays in the provider's group) and `complete`; `session.reconcile_execution` is refused while it lives and accepted after it ends; the receipt, the event and the audit row; a `running` row is never reconciled.
  9. A provider that cannot resume the native id → `start_failed` with its message and a committed exit.
- Deck and board: the two routes (owner only, CSRF, audit), the "previous process may still run" panel and its three states, Resume enabled after reconciliation.

## 8. Risks

- **Escaped descendants.** A process that left the provider's session and group is invisible to the check (ADR-005 §4.6). The owner's "Mark ended" accepts that risk explicitly, with the warning text; the action is audited.
- **Reboot detection** depends on the boot id reader; a VM snapshot restore with the same boot id keeps sessions `execution_unknown` (safe side).
- **Two executions of one native conversation** are possible only through a wrong owner reconciliation; the station never infers it.
