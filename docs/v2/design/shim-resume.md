# Resume on the shim host (DL-182)

- **Status:** Accepted design (revision 4, 2026-10-14): extra RE-REVIEW PASS (Codex approve, `/tmp/adversary-report-618cad40.md`). Revision 3 (commit e0d1bf5f) got RE-REVIEW FAIL (1 blocker; `/tmp/adversary-report-5f21c469.md`); the round limit was reached and, following the owner's choice for over-limit designs, revision 4 applies Codex's replacement sentence for one extra focused review (§12). Earlier: Revision 2 (commit 403a1d69) got RE-REVIEW FAIL (4 blockers, 3 major; `/tmp/adversary-report-403a1d69.md`); §11 maps them; this is the last review round allowed for this design. Earlier: Revision 1 (commit 3fda6b7f) got DESIGN REVIEW FAIL (4 blockers, 4 major, 2 minor; `/tmp/adversary-report-9fbb0832.md`); §10 maps them. Extends ADR-005 (revision 3, accepted) §4.5, §4.6 and §6. Needs a PASS before code.
- **Platform scope:** Linux only. Shim resume, the reboot proof and the execution reconciliation exist only on Linux. On macOS a shim cannot launch today (`host/shim/identity.rs:55,61` return `Unsupported`, so `server.rs:163` fails every launch), so a macOS station keeps the direct host; a `session.resume` on a station whose shim host is not Linux is refused at decision with `unsupported` ("resume on the shim host needs Linux").
- **Owner request (2026-10-14):** "do 2. The real fix: build resume on the shim host." The owner's station runs `host = "shim"`; every resume ends with `start_failed: resume is not supported on the shim host yet` (DL-118).
- **Unchanged:** every ADR-005 rule (launch journal, attach handshake, identities, cursors, stdin settlement, exit acknowledgment, recovery order). A resume starts a **new** shim and a **new** provider process; it never reattaches, and it is shown as `resumed`, never `reattached` (ADR-005 §6).

## 1. Facts today

- The station decision already admits `session.resume` only when every earlier shim execution of the session is proven ended (`Decision::unended_execution`, `crates/fleetdeck-station/src/store/resumes.rs:121-152`): each `shims` row is `never_started`, or `exited` with reason `exited` (`PROVEN_EXIT`: the shim reported the provider's exit with no remaining member of its process group or session, `descendants_remaining == 0`, `manager/shims.rs:46`).
- The dispatch then refuses every resume on the shim host (`manager/target.rs:270-276`, `REASON_RESUME_UNSUPPORTED`), because no durable resume launch exists: `ProviderAdapter::durable_launch(spec)` builds only a new session (`claude/durable_launch.rs`: `Launch.resume = None`, `CoreConfig.resumed = None`; `codex/durable_launch.rs`: `thread.resume = None`).
- Recovery never uses the boot id: a row whose shim is gone without a valid exit file becomes `lost` (`execution_unknown`), also after a reboot, so a reboot leaves every shim session unresumable. ADR-005 §4.6 names a verified reboot as sufficient termination evidence and an audited owner reconciliation as the other path; neither is built.
- Hibernation (`manager/hibernate.rs`) stops idle sessions and relies on resume, without a host check.

## 1a. The termination standard (what "ended" means)

- A resume or an import may start a provider for a native conversation only when no earlier **provider execution** of that conversation may still run. The provider execution is the provider process (the stored `provider_identity`: pid, start time, boot id) and every live member of its process group and session.
- **Escaped descendants** (a process that called `setsid()` and left both the group and the session) are not part of the provider execution in this design. **This is the same guarantee that the direct host gives today:** a direct-host provider runs in its own process group, the station's stop kills that group, and a descendant that left it survives, also when a resume follows (DL-118). A detached hook worker that keeps a writable descriptor on the transcript file (Claude passes `transcript_path` to hooks) can therefore write to a resumed conversation on either host. This risk is accepted, stated in the operations guide ("a hook that detaches with write access to the transcript can write to it after a resume"), and not blocked by this design; a shim resume is never weaker than a direct-host resume. (Architect decision, stated to the owner; the adversary's stricter standard would also block every direct-host resume.)
- **Inspection must be complete to count as proof.** Today `group_members` (`crates/fleetdeck-providers/src/host/shim/server_proc.rs:102`) returns 0 when `/proc` cannot be listed, skips entries it cannot read, and returns 0 on every non-Linux platform, so an incomplete or impossible inspection reads as "no members" (fail open). This design changes it to return `Option<u32>`: `None` when `/proc` cannot be listed, when any `/proc/<pid>/stat` read fails with an error other than `ENOENT`/`ESRCH` (a process that ended between the listing and the read is skipped), or on a platform without a reader. `ExitReport.descendants_remaining` becomes `Option<u32>` on the wire (shim protocol version +1, N/N−1 per ADR-005 §5.3: an older shim's value is treated as unverified). The station maps `None` to the new row reason `exit_unverified`, which is not proof.
- **Inspection provenance.** The shim protocol version that carries the nullable count also writes an `inspection: 1` field into the exit report and the exit file; the station stores it on the row (new column `inspection_version`). A clean-exit proof needs `inspection_version >= 1`. Every existing row that is `exited/exited` without it, and every exit file without it, is treated as `exit_unverified` (not proof): those sessions need the reconciliation of §4.2 (on Linux the check is complete, so it is one click when nothing runs).
- **N/N−1 encoding.** On a link that negotiated the older shim protocol, the shim encodes an unavailable count as `u32::MAX` (a conservative non-zero value that the older station reads as descendants remaining) instead of `null`. Rollback test: an N shim with an unverified exit attached by an N−1 station.
- Proof is therefore one of: `exited` with `descendants_remaining == Some(0)` and `inspection_version >= 1` (the shim saw the provider exit through `waitpid` and found no group or session member), `ended_by_reboot` (§4.1, revalidated at launch), or `reconciled_by_owner` (§4.2).

## 2. Durable resume launch (providers)

- New `ProviderAdapter` method `durable_resume_launch(&self, spec: &SessionSpec, native_id: &NativeSessionId) -> Result<DurableLaunch>`; the default returns `ProviderError::Unsupported("durable resume")`.
- Claude: the same as `launch_durable` with `Launch { resume: Some(native_id), fork: false, .. }` (so `cli_args` adds `--resume <id>`, exactly as the direct resume) and `CoreConfig.resumed = Some(true)`.
- Codex: the same as `launch_durable` with `thread.resume = Some(native_id)`, `fork = false` (the durable handshake sends `thread/resume`), and `CoreConfig.resumed = Some(true)`.
- The mode, model, capability and workspace rules are those of `launch_durable`: Claude falls back to the safest mode when the stored mode is not offered (`claude/args.rs:101`, `effective_mode`), Codex refuses an unoffered mode before any process (`codex/durable_launch.rs:19`). The resumed spec is the stored spec (DL-118), never more permissive.
- The native id is not secret; it goes into argv for Claude as the direct resume already does.

## 3. Station dispatch

In `SessionTarget::start` with `resume = Some(native)` and a shim host:

1. **Recheck termination at dispatch, across sessions:** read the new `unended_native(provider_instance, native_id, except_session)` (below) again (a store read, no decision). `Some(why)` → `start_failed("the previous execution is not proven ended: <why>")`, `NotDispatched("execution_not_ended")`. For an `ended_by_reboot` row it also rereads the current boot id and requires it to differ from the stored one (§4.1). This closes the window between the decision and the dispatch.

**One native conversation, one execution (all sessions, all instances).** `unended_execution` today checks only the shim rows of one Fleet Deck session (`store/resumes.rs:137`), and `native_session_in_use` (`:49`) ignores `lost` and `exited` sessions, so an import of a lost session's native id creates a second session with no rows and passes. New store function `unended_native(instance, native_id, except)`: every session of this station of the same **provider kind** (any provider instance: two instances may share one `CODEX_HOME` or Claude config folder, `fleetdeck-core/src/config.rs:468`, `childenv.rs:81`, so instance ids do not separate native storage) whose native resume identity (the stored native session id, for Claude the `session_id`, for Codex the `thread_id`) equals `native_id` is checked; **and every session of the same provider kind with an unresolved execution whose native id is unknown** (the station crashed before `session.started` committed it, `store/log.rs:166`) blocks every resume or import of that provider kind on this station until its identity is recovered or its termination is proven (`execution_not_ended`, naming that session). The resume or import target is also written on the shim row before the spawn (new column `native_target`: the native id that a resume or import continues; null for a new session), so a crash after the spawn still names the conversation. Each checked session it must not be live, starting, holding a start or resume effect that is not settled, or have any shim row without proof (§1a). It is used by the decision of `session.resume`, of `session.import` and of the hibernated-input resume (DL-180), and again at dispatch. The decision that admits a resume or an import also reserves the native conversation: the session's `state = starting` row and its unsettled effect are themselves an unresolved execution for every other session, so two admissions cannot both pass (the station decides in one writer transaction).
2. `adapter.durable_resume_launch(spec, native)`: an error → `start_failed(<error>)`, `NotDispatched("start_failed: …")`. No fallback to a direct-host resume (a session keeps the host of its station, ADR-005 §2).
3. `start_durable(effect, spec, prompt, shims, launch, cancel)` as a new durable start. Both durable cores (`manager/shim_claude.rs:86`, `manager/shim_codex.rs:84`) are changed to accept `EffectPayload::Resume { prompt: Some(input), .. }` as the first input through the same durable first-input path as `Start` (its checkpoint, frame identity, write settlement and failure recording); today they reject every `Resume` payload (`not_a_shim_command`). The flow: a new `shim_id`, the `starting` row, the journal, the attach, the driver, then the first input (the resume's prompt, if any). The new row is the session's newest shim row; earlier rows stay as they are (their termination is what the decision checked).
4. The session tools (DL-165) follow the shim-host rule of a new durable start (today: `prepare_fleet` with `durable = true` gives "no session tools: durable host" and the info log).
5. `REASON_RESUME_UNSUPPORTED` is removed; the `start_failed` text "resume is not supported on the shim host yet" no longer exists.

The same path serves every resume: the board's Resume and "Resume all", a hibernated session's input (DL-180), the TUI handover's return, auto-continue (ADR-007, when enabled) and import (`session.import` with a native id on a shim station).

## 4. Termination evidence after a shim loss

### 4.1 Verified reboot (ADR-005 §4.6), Linux only

- **Launch boot evidence (before the spawn).** The station writes the current boot id into the launch journal's `created` record (`created { shim_id, incarnation, boot_id }`, `crates/fleetdeck-providers/src/host/shim/journal.rs:23-34`) before it starts the shim, and stores it on the `starting` row (new column `launch_boot_id`). The shim reads its own boot id at start and refuses to spawn the provider (exits without `spawn_attempted`) when it differs from the journal's. So every launch has boot evidence from before the provider could exist, also when the provider identity is never committed.
- **Rows without a provider identity.** For a row that has no `provider_identity` (power loss after the spawn and before `shim_running` committed), recovery first fences the launch exactly as ADR-005 §3.2 step 1 does (lock acquired, `fenced` appended); only then does the reboot rule apply, using `launch_boot_id`: a current boot id that differs from it proves that the launch's provider (and everything it started) ended with the old boot. Revalidation at launch (below) uses `launch_boot_id` for such rows, and the "stored identity not alive" check is skipped only because no identity exists.

- At recovery (ADR-005 §3.2), for **every** unresolved shim row of this incarnation that has a stored `provider_identity` (whatever its diagnostic reason: `execution_unknown`, `attach_refused`, `identity_mismatch`, `exit_not_reported`, `exit_unverified`, a checkpoint or driver failure from `manager/shims.rs:214`, `manager/shim_station.rs:195-257`, or `exited_descendants_remaining`), the station reads the current boot id (`/proc/sys/kernel/random/boot_id`). When both values are non-empty and differ, it records the proof `ended_by_reboot` **next to** the row (a new column `proof`, the row's `state` and diagnostic `reason` stay as they are), with the boot id it read (`proof_boot_id`). Equal, empty, missing or unreadable → no proof (fail closed).
- **Revalidation at launch.** `ended_by_reboot` is not permanent: the dispatch of a resume or import (§3 step 1) reads the boot id again and accepts the proof only when the current boot id still differs from the row's stored `provider_identity.boot_id` **and** no live process has the stored `provider_identity` (pid + start time). A VM memory restore that brings back the old boot id therefore makes the proof invalid again.
- The session's state stays as recovery set it (`lost` stays `lost`), so ADR-007 auto-continue discovery (`manager/auto_continue.rs:167-201`) is not disturbed. **Auto-continue on shim stations** was off in this design; DL-188 step 3 (2026-10-16) lifted it: a lost shim session is continued only with this proof, checked again at the dispatch. The board's Resume uses the proof.
- `unended_execution` and `unended_native` accept a row with `proof = 'ended_by_reboot'` (revalidated at dispatch) or `proof = 'reconciled_by_owner'`, or `state = 'exited'` with reason `exited` (a verified zero count, §1a).

### 4.2 Owner reconciliation (ADR-005 §4.6)

For a session with any unresolved shim row (any `lost` reason, `exit_unverified`, or `exited_descendants_remaining`), whatever the diagnostic reason:

- **Check** (`GET /api/sessions/{station}/{session}/execution`, owner only; station method `session.execution_check`, non-mutating): the station runs the ADR-005 §4.6 session-and-group check for the stored identity: (a) a live process with the stored `provider_identity` (pid + start time + boot id), (b) live members of the provider's process group or session (Linux `/proc/*/stat` fields 5–6; macOS `proc_listpids(PROC_PGRP_ONLY)` and the session id). Answer: `{ shim_id, reason, same_boot: bool, identity_alive: bool, group_members: [{pid, command (first 120 chars, no args)}], session_members: [...], complete: bool }`. Any read error → `complete: false` and the error text. The check also lists, for information only, processes outside the group and session whose parent chain leads to the provider's former pid (best effort, may be empty): these are escaped descendants (§1a) and do not block. The check never kills anything.
- **Board:** a lost or descendants-remaining session shows "The previous process may still run" with the check result. With live members it lists them and says "Stop these processes on the station, then check again". With none and `complete: true` it offers "Mark the previous run as ended" with the warning "Fleet Deck cannot see a process that left its process group. Mark it ended only if you know that the agent is not running."
- **Mark ended** (`POST …/execution/reconcile { shim_id }`, owner only, CSRF, audited, station command `session.reconcile_execution { session_id, shim_id }`, mutating with a receipt): the station runs the check again inside the decision path; it refuses (`invalid_state`, stored) when `identity_alive` or any group or session member is live; when the check is incomplete it is refused (`invalid_state`, "the process check is incomplete: <reason>"); there is no acknowledgment override. Accepted: it sets `proof = 'reconciled_by_owner'` on that row (state and reason unchanged) and appends `session.execution_reconciled { shim_id }`. `unended_execution` accepts this reason too (`PROVEN_BY_OWNER`). It never touches a row that is `starting` or `running`.
- Member and token principals get `403`; the action needs the owner, like other owner-only station actions. No elevation (it changes no policy and no credential; the audit row records it).

## 5. Hibernation

- Today hibernation never stops a running shim session: candidate selection requires `Running` (`manager/hibernate.rs:51`), then admission refuses any result of `unended_execution` (`:86`), and the session's own running row always is one. The admission changes to `unended_execution_except_current(session, current_shim_id)`: the row of the execution that the hibernation stops is excluded; every **earlier** unresolved row still refuses. Normal Stop admission is unchanged.
- A later resume needs the §1a proof for the stopped execution (a graceful stop gives `exited` with a verified zero count in the normal case).

## 6. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| The earlier shim's exit is not proven | The decision refuses (`invalid_state`, with the shim row's state and reason), as today; the board shows §4.2. |
| Proof changes between decision and dispatch | §3 step 1 recheck → `execution_not_ended`, nothing launched. |
| The provider cannot resume the native id (deleted transcript, unknown thread) | The durable driver's open fails → the shim's provider is killed, the exit is committed, `start_failed` with the provider's message (the same as a failed durable start). |
| A station crash during the resume launch | ADR-005 §3.2 recovery of the new `starting` row (fence or attach); the session is then running, `never_started` (resumable again) or `lost`. |
| Reboot with an unreadable boot id | Fail closed: `execution_unknown`, §4.2 path. |

## 7. Tests

- Providers: `durable_resume_launch` for Claude (argv has `--resume <id>`, the stored mode, `resumed = Some(true)`; an unoffered mode falls back to the safest mode) and Codex (`thread/resume` with the thread id in the handshake, `resumed = Some(true)`; an unoffered mode is refused); the trait default is `Unsupported`. Shim: `group_members` returns `None` for an unlistable `/proc`, for an unreadable entry (not `ENOENT`/`ESRCH`) and on non-Linux; `Some(0)` only after a complete listing; the exit report carries `None` → `exit_unverified`; an N−1 shim's report is unverified.
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
  10. The durable first input of a resume: the provider receives the prompt bytes once; a crash after the claim and before the write settles as `outcome_unknown` and is never resent (ADR-005 §4.3).
  11. Import of a lost session's native id while that session's row is unresolved → refused; import after proof → accepted, then a resume of the original session → refused (the import's session holds the conversation); two concurrent admissions for one native id → exactly one passes.
  12. Reboot proof revalidation: A (boot 1) → recovery in boot 2 records the proof → the boot id reader returns boot 1 again (simulated memory restore) → dispatch refuses; a live process with the stored identity → refused.
  13. Reboot proof for each existing lost reason and for `exited_descendants_remaining`; the session stays `lost`; auto-continue does not pick it.
  14. Hibernation of a normal running shim session is admitted; with an unresolved earlier row it is refused.
  15. A `setsid()` descendant that outlives a normal provider exit: the exit is `exited` with `Some(0)` and the resume is admitted (§1a scope); the check of §4.2 lists it as an escaped descendant.
  16. A resume on a non-Linux shim station is refused at decision (`unsupported`).
  17. A Codex import through instance B of a thread whose execution under instance A is unresolved (both instances share a home) → refused; after proof → accepted.
  18. A crash after the spawn and before `session.started` committed: the row has `native_target` (resume) or an unknown native id (new session); a resume or import of that provider kind is refused until recovery or reconciliation.
  19. Legacy `exited/exited` rows and exit files without `inspection` are `exit_unverified`; reconciliation with a complete check makes them resumable.
  20. Reconciliation with an incomplete check is refused.
  22. Power loss after the spawn and before the identity commit: the row has `launch_boot_id` and no identity; with the same boot id it stays unresolved (resume and import of the provider kind refused); after a reboot, recovery fences the launch, the reboot proof applies, and resume and import become available again across instances.
  23. A shim whose boot id differs from its journal's refuses to spawn the provider.
  21. N−1 encoding: an N shim attached by an N−1 station reports `u32::MAX` for an unverified exit, and the older station treats it as descendants remaining.
- Deck and board: the two routes (owner only, CSRF, audit), the "previous process may still run" panel and its three states, Resume enabled after reconciliation.

## 8. Risks

- **Escaped descendants.** A process that left the provider's session and group is invisible to the check (ADR-005 §4.6). The owner's "Mark ended" accepts that risk explicitly, with the warning text; the action is audited.
- **Reboot detection** depends on the boot id reader; a VM snapshot restore with the same boot id keeps sessions `execution_unknown` (safe side).
- **Two executions of one native conversation** are possible only through a wrong owner reconciliation; the station never infers it.

## 10. Review map (revision 1 → revision 2)

| Finding (rev 1) | Change |
| --- | --- |
| B escaped descendants not covered by a clean exit | §1a: explicit standard — the provider process and its group/session; escaped descendants are out of scope and reported (a deliberate decision, told to the owner); incomplete inspection never proves (`Option` count, `exit_unverified`). |
| B per-session check allows a duplicate via import | §3 `unended_native` across all sessions of the conversation, used at decision and dispatch, admission reserves the conversation. |
| B owner reconciliation with a live escaped descendant | §1a scope makes escaped descendants non-blocking; reconciliation still refuses while the identity or any group/session member lives; an incomplete inspection needs an explicit owner acknowledgment. |
| B reboot proof undone by a memory restore | §4.1 proof stored apart from the state and revalidated at launch (boot id differs again, stored identity not alive). |
| M resume prompt rejected by durable cores | §3 step 3 cores accept `Resume` first inputs; test 10. |
| M hibernation refuses running shim sessions | §5 exclude only the current execution; test 14. |
| M auto-continue and the reboot event | §4.1 the state stays `lost`; auto-continue stays off on shim stations (stated). |
| M other lost reasons excluded | §4.1 every unresolved row with an identity; §4.2 any unresolved row. |
| m macOS reader missing | Status: Linux-only evidence; macOS exits are unverified; reconciliation with acknowledgment. |
| m Claude mode fallback | §2 states the per-provider rule; tests. |

## 11. Re-review map (revision 2 → revision 3)

| Finding (rev 2) | Change |
| --- | --- |
| B escaped hook worker with transcript write access | §1a: same guarantee as the direct host, accepted and documented (architect decision, told to the owner); not followed. |
| B instance ids do not separate native storage | §3 exclusion across all instances of the provider kind; test 17. |
| B unknown native id not seen | §3 an unresolved execution with an unknown native id blocks the provider kind; `native_target` on the row before spawn; test 18. |
| B legacy `exited/exited` accepted | §1a inspection provenance, legacy rows unverified; test 19. |
| M nullable count breaks N−1 | §1a N−1 encoding `u32::MAX`; test 21. |
| M acknowledgment override | §4.2 removed; test 20. |
| M macOS cannot launch | Status: Linux only, refused elsewhere; test 16. |

## 12. Extra review map (revision 3 → revision 4)

| Finding (rev 3) | Change |
| --- | --- |
| B unknown-id guard has no recovery when the identity was never committed | §4.1 launch boot evidence in the journal and on the row before the spawn, shim check, fence-then-reboot-proof for identity-less rows; tests 22–23. |

## 13. Architect refinement during the build (2026-10-14)

The implementation found that §3's rule "every session of the same provider kind with an unresolved execution whose native id is unknown blocks every resume or import of that kind" would make any fresh session (a Claude process reports its native id only after its first input) block all resumes and imports of its provider kind. Refinement, recorded in DL-182: a row or session blocks a resume or import of native id N only when (a) it is a session of the same provider kind whose stored native id is N and whose execution is not proven ended, or (b) it is a shim row of the same provider kind whose `native_target` is N and has no proof. A row whose `native_target` is NULL started a new conversation and cannot be N, so it never blocks another conversation. This is narrower than revision 4 and keeps the safety goal (no second execution of one conversation). It has not been through an adversary design round; the PRE-PR review of the code covers it.

Review fix 1 (PRE-PR review, blocker 1). A session of the same provider kind whose native id is still unknown (a fresh provider that created a conversation but never committed `session.started`, then a station crash and a failed reattach) can hold any conversation. An import therefore fails closed for it: while such a session is busy (a state that is not final, a live process, or an unsettled start or resume effect), or while any of its shim rows has no proof (§1a), an import of any id is refused with "session `<id>` has a run whose conversation id is not known yet; an import waits until that session reports its id or its run is proven ended". A resume is not blocked by it, because a resume uses the native id that its own session stored, and a different session cannot hold that id. The import effect carries `import: true`, so the dispatch applies the same rule again (`execution_not_ended`); a stored effect without the flag is a resume.

Review fix 2 (PRE-PR review, blocker 2). A single `/proc` pass can report zero members while a member lives: a member forks after the listing, and its parent ends before its `stat` is read. A pass is clean only when no listed pid vanished before its read and no zombie is the leader or belongs to the leader's group or session (RE-REVIEW round 2: a leader that forks and dies before its read is such a zombie). A process cannot join a session from outside it, so a member alive at the end of a clean pass descends from a member that was listed and alive when read (counted), or that died before its read (a vanished pid or a zombie of the group or session, so the pass is not clean). The census (the shim's exit count and the owner's execution check) makes up to 200 passes, 10 ms apart (a busy machine starts and ends processes all the time; many short tries find a clean pass). A pass that shows a member (or, for the check, a live provider identity) is final at once. A result of zero members counts only from a clean pass. When no pass is clean, the result is incomplete: no count (`descendants_remaining` unknown, so the exit is `exit_unverified`) and `complete: false` with "processes kept changing during the check; try again". The shim reaps its own ended children before the census, and the census runs on a blocking thread.
