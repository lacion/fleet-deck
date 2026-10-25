# ADR-007 — Restart auto-continuation (L19)

- **Status:** Accepted (owner pre-approval, 2026-10-12; DL-127). Written in M3 round 39 (DL-118); implemented in M3 lane A round 46 with the deviations of DL-127.
- **Date:** 2026-10-12
- **Deciders:** architect (goal loop, M3); owner pre-approval (DL-073 owner answers, DL-127); the adversary DESIGN REVIEW was not possible in the loop (no subagents) and is part of the architect PRE-PR REVIEW
- **Implements:** PLAN A10 L19, Phase 7 item 7-2 and the ADR-005 "Auto-continue integration" bullet; ADR-005 §6 (reattach is not resume, resume eligibility); ADR-004 §9.3 envelopes and §9.6 method table (one new row, §4)
- **Depends on:** `session.resume` (DL-118: an owner-requested native resume of an ended session, with the admission rules of `crates/fleetdeck-station/src/manager/resume.rs`)

## 1. Context

A station restart can end a session's provider process: with the direct host always, with the shim host when the shim is lost. L19 lets the owner choose, per session, that the station continues such a session after the restart without a new owner command. The risk is a second execution of one conversation, a resend of an input whose fate is unknown, or a continuation that overrides newer owner intent. This ADR states when the station may continue, what it sends, and the tests that must exist before wiring.

## 2. Contract

### 2.1 The preference

- `auto_continue` is a per-session boolean preference. The default is `false` for every session, also for sessions created before this ADR (stored rows without the column read as `false`).
- The owner sets it with the mutating link method `session.set_auto_continue { session_id, enabled }` (scope `drive`, feature `auto_continue.v1`, deck → station, `CommandEnvelope`, station receipt). A coordinator operation and the station-local socket cannot set it to `true` (`forbidden`); they can set it to `false`.
- The preference is stored on the session row and in the read model (`SessionSummary.auto_continue`), and every change appends `session.auto_continue_changed { enabled }`.

### 2.2 Eligibility at a station start

After the station start of ADR-005 §3.2 and `Store::recover` committed, and before the station admits commands, the station evaluates every session that recovery made `lost` in this start. A session is eligible only when all of these hold in one decision transaction:

1. `auto_continue` is `true`.
2. The session's app run, app session, provider instance and native resume identity (Claude `session_id`, Codex `threadId`) are the ones recorded before the restart, and the provider instance is still configured with the same provider kind.
3. The previous execution is proven ended: the direct host's process group is gone (DirectChildHost kills it with the station, verified by the M3 SIGKILL test), or for a shim session a verified reboot (`boot_id` changed) or an audited owner reconciliation (ADR-005 §4.6, §6). Every shim row of the session must prove its end: only `never_started`, and `exited` with the reason `exited`, do (DL-119). Any other row — open, `exited_descendants_remaining`, `exit_not_reported`, or `lost` for any reason (`execution_unknown`, `attach_refused`, `shim_incompatible`, `identity_mismatch`, an unsupported or corrupt adapter checkpoint, no driver, or a reason that this build does not know) — makes the session ineligible (fail closed); the same rule refuses `session.resume`.
4. The session was in a turn or had a run open when it was lost (there is work to continue). An idle session is not continued.
5. No newer owner work: no command of the session was admitted after the last committed `turn.started`, other than answers, interrupts and stops.
6. No requested stop: no `session.stop` was admitted for the session, and the session was not `stopping`.
7. The lost turn was not a maintenance turn (`/compact` or another station-issued maintenance input).
8. The lost turn did not complete: no `turn.completed` for it is committed.
9. The continuation window has not expired: the station start is within `auto_continue_window` (default 10 minutes) of the last committed event of the session.
10. The provider reported the `resume` capability, and every `session.resume` admission rule holds (workspace in the allowed roots, a free session slot, no revert that blocks input or holds the worktree).
11. No input of the session is `outcome_unknown` from this restart. An input whose fate is unknown makes the session ineligible: the station never resends it and never sends a continuation in its place.

An ineligible session stays `lost`; the station appends `session.auto_continue_skipped { reason }` with the first failed rule as a stable code (`disabled`, `identity_changed`, `execution_unknown`, `idle`, `newer_owner_work`, `stop_requested`, `maintenance_turn`, `turn_completed`, `expired`, `unsupported`, `not_resumable`, `input_outcome_unknown`).

### 2.3 The continuation intent

- For an eligible session the same decision transaction stores one `continue` effect in the outbox with a stable id `continue:<session_id>:<lost turn id>` and an immutable payload: the native resume identity, the stored spec (instance, workspace, mode, model), and the continuation text below. The effect id makes a second station start for the same lost turn find the existing effect; it never stores a second one.
- The continuation text is the fixed station message `"The station restarted while you were working. Continue the task where you stopped."`. It is a `RunOrigin::Continuation` input, never a copy of the owner's last prompt.
- The resumed process gets the stored mode, never a more permissive one (L8).

### 2.4 Dispatch

- The worker claims the effect and, right before the external I/O, rechecks rules 2, 3, 5, 6, 7, 8, 9 and 10 against the committed state and the station policy (rules 7 and 8 added by DL-133). A failed recheck retires the effect `retired_not_dispatched` with the rule's code; nothing is spawned.
- Dispatch is two external steps with durable steps between them: (a) native resume (the `session.resume` start path), (b) the continuation input to the resumed process. The effect records `resumed` after (a) is confirmed by `session.started { resumed: true }` with the same native identity. A different native identity ends the new process and the effect becomes `outcome_unknown` (`identity_changed`).
- After a crash between the steps, recovery uses the committed step: before `resumed` the effect is `outcome_unknown` (a process may have started; it is never started again); after `resumed` with the input claimed the input follows the ordinary §6.4 ambiguous-send rules and is never resent. A continuation is never retried as a new prompt.
- Any owner command admitted for the session after the intent was stored (rule 5) makes a not-yet-dispatched continuation retire `newer_owner_work` at the recheck.

### 2.5 What the board shows

- `resumed` for a continued session (the new `session.started { resumed: true }`), never `reattached`.
- The continuation input is shown with its origin `continuation`.
- A skipped continuation shows its reason.

## 3. Required decisions

| Decision | Choice |
| --- | --- |
| Default | Off for every session, also old rows (§2.1). |
| Who can enable | Only owner-authorized deck commands (§2.1). |
| Identity match | Run, session, provider instance and kind, native resume identity (§2.2 rule 2). |
| Termination evidence | Direct host: process group gone; shim: reboot or owner reconciliation (§2.2 rule 3). |
| Rechecks | At intent and again right before dispatch (§2.2, §2.4). |
| Intent storage | Outbox effect with a stable id per lost turn (§2.3). |
| Ambiguity | Never a new prompt; never resent (§2.4). |
| Unsupported or uncertain | Stays `lost`/`not_resumable` with a skip reason; no replacement is spawned (§2.2). |

## 4. Wire changes

- ADR-004 §9.6 gains one row: `session.set_auto_continue` (mutating, `CommandEnvelope`, `drive`, deck → station, feature `auto_continue.v1`).
- New event kinds `session.auto_continue_changed` and `session.auto_continue_skipped`; older peers decode them as unknown events.
- `RunOrigin::Continuation` is a new input origin (the existing `RunOrigin::Resume` names an owner resume); older peers decode it as unknown.

## 5. Negative tests (before wiring)

1. `auto_continue_is_off_by_default_and_a_restart_never_continues_a_session_without_it` (also ADR-005 test 24: a reattached session is never resumed)
2. `a_coordinator_or_the_local_socket_cannot_enable_auto_continue`
3. `a_session_lost_with_execution_unknown_is_never_continued`
4. `an_idle_session_is_not_continued`
5. `a_command_admitted_after_the_last_turn_start_prevents_the_continuation`
6. `a_requested_stop_prevents_the_continuation`
7. `a_maintenance_turn_is_not_continued`
8. `a_completed_turn_is_not_continued`
9. `an_expired_window_prevents_the_continuation`
10. `an_input_with_an_unknown_outcome_prevents_the_continuation_and_is_never_resent`
11. `a_changed_provider_instance_or_native_identity_prevents_the_continuation`
12. `a_second_station_start_finds_the_stored_intent_and_stores_no_second_one`
13. `an_owner_command_after_the_intent_retires_the_undispatched_continuation`
14. `a_crash_after_the_resume_before_the_input_never_starts_a_second_process_and_never_resends`
15. `a_resume_that_reports_another_native_identity_ends_the_new_process_with_outcome_unknown`
16. `the_continued_session_runs_with_the_stored_mode_never_a_more_permissive_one`
17. `a_continued_session_is_shown_as_resumed_with_a_continuation_input`

## 6. Status of the dependencies

- `session.resume` exists (DL-118) with the direct host; a shim-host resume is refused (`resume_not_supported_on_the_shim_host`) until the ADR-005 §6 termination evidence and a durable resume launch exist.
- The DirectChildHost SIGKILL behavior is verified by `crates/fleetdeck-cli/tests/station_sigkill.rs` (`providers_of_a_sigkilled_station_exit_and_the_restarted_station_reports_them_lost`).
