# ADR-005 — `ShimHost`: durable provider processes and reattachment

- **Status:** Proposed (M3 round 2, 2026-10-12). Not implemented. The implementation starts only after the architect records an adversary DESIGN REVIEW PASS for this ADR in the "Design approvals" section of the gate-state file (M3 goal, GATE STATUS). DL-073 records an owner pre-approval; DL-074 records why the executor still waits for the review.
- **Date:** 2026-10-12
- **Deciders:** architect (goal loop, M3), owner (pre-approval, DL-073), adversary DESIGN REVIEW (pending)
- **Implements:** PLAN A9, A10 (Phase 7 part, L19), Phase 7 "`ShimHost` (Revision 3 contract)" and its "Required ADR-005 decisions"; ADR-001 §host boundary (`SessionHost`, `ProcessControl` in `crates/fleetdeck-providers/src/host/mod.rs`)
- **Evidence:** none yet. §9 lists the tests that come before the implementation. The station SIGKILL test of DL-074 (`crates/fleetdeck-cli/tests/station_sigkill.rs`) is the baseline for the direct host.

## 1. Context

The MVP host is `DirectChildHost` (`crates/fleetdeck-providers/src/host/direct.rs`): the provider is a child of the station, in its own process group, with `kill_on_drop(true)`. A station restart ends every provider (DL-074 proves it for SIGKILL), and A10 recovers through native resume, which can lose in-flight work and open approvals.

Phase 7 requires a host that keeps the provider and its output alive while the station restarts or upgrades, and lets the new station process reattach. PLAN Phase 7 fixes these rules: the shim preserves execution and output; a PID alone is not process identity; fencing and exclusive reattachment (one station incarnation owns an attachment at a time); bounded buffering and backpressure; stdout cursor recovery; an explicit exit state; socket permissions; update and restart failure tests; no reuse of the "last reference drop kills the child" behavior. The design review added four decision groups (attachment ownership, output recovery, platform and upgrades, auto-continue integration). This ADR decides them.

## 2. Shape

- **Shim process.** `fleetdeck shim` (a hidden subcommand of the same binary) runs one provider process. The station starts the shim; the shim starts the provider as its own child and owns the provider's stdin, stdout and stderr pipes.
- **Detach.** The shim calls `setsid()` and so leaves the station's session and process group. A station death (also SIGKILL) does not signal it. The provider runs in its own process group under the shim. On Linux the shim sets `PR_SET_PDEATHSIG(SIGKILL)` on the provider, so a shim death ends the provider; on macOS (no equivalent) a shim death closes the provider's stdin, and the provider ends at EOF (the behavior proved for the direct host by DL-074).
- **Station side.** `ShimHost: SessionHost` (`kind() == HostKind::Shim`, `survives_station_restart() == true`) and a `ShimControl: ProcessControl` over the shim socket. Dropping a `ShimControl` or a `HostedProcess` only closes the station's connection; it never stops or kills the provider. Only `stop`, `kill` and an explicit owner action end a provider.
- **Selection.** The host is a station setting (`host = "direct" | "shim"`, default `direct` until 7-1 passes its tests; then `shim` on Linux and macOS). A session keeps the host it started with.

## 3. Attachment ownership (decision group 1)

- **Two separate values.** The *station incarnation* is the id of the station database (PLAN §10 `meta`); it changes only when the database is replaced. The *attachment-owner generation* is a per-shim counter (u64) that rises at each attach. They are never mixed: the incarnation answers "is this my shim?", the generation answers "which station process owns it now?".
- **Shim record.** Before it starts a shim, the station commits a `shims` row: `shim_id` (ULID), `session_id`, `incarnation`, `state = starting`, `owner_generation = 0`, `socket_path`, `cursor = 0`. It then starts the shim. The shim reports `ready { provider_identity }`, and the station stores the identity (`state = running`). A crash between the two leaves a `starting` row; recovery treats it like any other row (§3.4).
- **Attach key.** The station creates 32 random bytes per shim and passes them to the shim through an inherited pipe (never argv, environment, URL or log). The station writes the key to `<private dir>/<shim_id>.key` (mode 0600 inside the 0700 directory, §5.1) and never into the database or events. The key proves that the attaching station holds the record of the station that created the shim.
- **Attach handshake** (one Unix socket connection):
  1. The shim checks the peer UID (`SO_PEERCRED` on Linux, `getpeereid` on macOS) equals its own; else it closes the connection.
  2. The shim sends `hello { protocols, shim_build, shim_id, nonce }`. The station picks a protocol (§5.3).
  3. The station sends `attach { incarnation, generation, proof = HMAC-SHA256(key, nonce ‖ incarnation ‖ generation) }`, where `generation` is the stored `owner_generation + 1`, committed to the database before the send.
  4. The shim accepts only if the proof is valid, `incarnation` equals the one it got at start, and `generation` is greater than its current owner generation. Else it answers `refused { reason: bad_proof | foreign_station | stale_owner }` and closes.
  5. On accept, the shim sets its owner generation, closes any older attachment connection at once (takeover), and answers `attached { generation, provider_identity, cursor_floor, exit? }`.
- **Stale writers.** Every frame from the station carries the generation. The shim drops the frame and closes the connection when the generation is not the current one, so an old station process (or a paused one that wakes up) cannot write stdin or stop the provider after a takeover. Only one connection is attached at a time; a second `attach` with the same or a lower generation gets `stale_owner`.
- **Snapshot restore.** If the station database is restored from an older snapshot (7-3 rollback), the stored generation can be lower than the shim's. The station then reads the shim's generation from `refused { stale_owner, current }` only after a valid proof, stores `current + 1`, and attaches again. The proof (the key file) is the authority; the generation only orders station processes.

### 3.1 Process identity

A PID alone is never identity. `provider_identity = { pid, start_time, boot_id }`: Linux `/proc/<pid>/stat` field 22 (start time in clock ticks) and `/proc/sys/kernel/random/boot_id`; macOS `proc_pidinfo(PROC_PIDTBSDINFO)` start time and `sysctl kern.bootsessionuuid`. The shim reports the identity of its own child; the station compares it with the stored one at each attach. A difference (PID reuse, reboot) means the session is `lost` with the reason `identity_mismatch`; the station never adopts that process.

### 3.2 Recovery at station start (before new commands, A10 / L2)

For each `shims` row of this incarnation in state `starting` or `running`:

1. Socket connects and attach succeeds with the same provider identity → reattach (§4.3, §6).
2. Socket refuses connections (stale file) → read `<shim_id>.exit` if it exists (§4.5) → report the exit; else the session is `lost` (`shim_gone`). Remove the stale socket file only after this check.
3. Attach refused with `foreign_station` or `bad_proof` → `lost` (`attach_refused`); the station leaves that shim alone (it may belong to another station directory).

Only after every row is settled does the station admit commands.

## 4. Output, input and exit (decision group 2)

### 4.1 Cursor

- The shim numbers the provider's stdout bytes with an absolute offset from 0. It keeps every byte at or above the *acknowledged cursor* in a ring buffer of 16 MiB (station setting, 1–256 MiB).
- The station parses only complete lines. In the same transaction that commits the events of a line set, it stores `cursor = offset after the last complete line` in the `shims` row. After the commit it sends `ack { generation, cursor }`; the shim frees the bytes below it.
- **Crash after a read, before the commit.** The bytes are not acknowledged, so the shim still holds them. The new station attaches and asks `replay { from: stored cursor }`. The events of those bytes are produced once, because the cursor and the events were committed together.
- **Partial frames.** A partial line at the end of a read stays in the station's parser buffer and is never acknowledged. At replay, the shim sends from the stored cursor, which is always a line boundary.
- **stderr** keeps a bounded tail (8 KiB, `STDERR_TAIL_BYTES`) in the shim; it is part of the exit report.

### 4.2 Backpressure and buffer exhaustion

- When the unacknowledged bytes reach the buffer bound, the shim stops reading the provider's stdout. The provider then blocks on its own write. No byte is dropped. The shim keeps reading again when an `ack` frees space.
- While no station is attached, the same rule applies: a long station outage pauses a talkative provider instead of losing its output. The board shows the session as `detached` after a station restart until the reattach (no claim of progress).
- **Explicit gaps.** A gap is possible only when the station asks for a cursor below the shim's `cursor_floor` (the oldest byte it still holds), which happens after a database snapshot restore (§3) or a defect. The shim then answers `gap { from, to }` and replays from `to`. The station commits a `session.output_gap { from, to, reason }` event (a new kind, decoded as `EventBody::Unknown` by older peers, L16) and marks every effect whose confirmation could be inside the gap `outcome_unknown`. It never invents events for the missing bytes.

### 4.3 Stdin and duplicate writes

- Each stdin write is a frame `write { generation, write_id, bytes }`. `write_id` is the station's effect id (stable across restarts). The shim writes the bytes to the pipe once and answers `written { write_id }` after the write call completes.
- The shim keeps the result of the last 4,096 write ids. A frame with a known `write_id` is not written again; the shim answers `written` with the stored result.
- After a reattach, for every effect in state `sent` or `outcome_unknown` that is a stdin write, the station asks `write_status { write_id }`. `written` → the effect stays `sent` and waits for provider evidence (unchanged M2 rules). `unknown_write` (never received by the shim, and the shim's id window covers the station's last attach) → the effect is `not_dispatched`, so the M2 rules may release or report it. If the id window does not cover it, the effect stays `outcome_unknown` (never resent).

### 4.4 Control

`stop { policy }`, `kill`, `close_stdin` and `interrupt` are frames with the generation. They work only on the current attachment.

### 4.5 Exit state

- When the provider ends, the shim stores `ExitReport { termination, cause, stderr_tail }` and the final cursor. It keeps running and serving the replay until a station acknowledges both the last output and the exit (`ack_exit`). Then it removes its socket and key file and ends.
- If no station acknowledges within 7 days, the shim writes `<shim_id>.exit` (mode 0600, JSON, no secret) with the report and the final cursor and ends. The remaining unacknowledged output is lost; the station records it as a gap at recovery.
- A shim that dies itself (crash, SIGKILL) leaves no exit record; the provider ends (§2). The station reports `lost` (`shim_gone`).

## 5. Platform and upgrades (decision group 3)

### 5.1 Socket placement and authentication

- Directory: `$XDG_RUNTIME_DIR/fleetdeck/<incarnation-prefix>/` on Linux when `XDG_RUNTIME_DIR` is set; otherwise `<station dir>/run/shims/`. macOS: `<station dir>/run/shims/`. The station creates the directory with mode 0700 and refuses to use it if its owner is another UID or its mode allows group or other access.
- Socket file: `<12-char shim prefix>.s`, mode 0600. The full path must fit `sun_path` (108 bytes Linux, 104 macOS); if it does not, the station uses `${TMPDIR:-/tmp}/fd-<uid>-<8-char station prefix>/` with the same owner and mode checks.
- Authentication: directory and file modes, the peer-UID check, and the HMAC attach proof (§3). The same-UID boundary of ADR-006 §3 also applies here: a same-UID process can read the key file and attach; it already controls the station.

### 5.2 Stale sockets

A socket file whose connect fails with `ECONNREFUSED` or `ENOENT` on the target is stale. The station removes it only in recovery step 2 (§3.2). A shim refuses to start when its socket path exists and accepts connections.

### 5.3 Protocol negotiation and upgrades

- The shim protocol is length-prefixed JSON frames with a version number. A shim build supports its own version and the previous one; a station build also supports its own and the previous one. `hello` lists the shim's versions; the station picks the highest common one.
- A station upgrade restarts only the station process; running shims keep their old code (the executable stays mapped). New sessions get the new shim build.
- **Incompatible upgrade.** The 7-3 activation trial lists the protocol versions of all running shims. A new build that cannot speak one of them is not activated; the old build keeps running (rollback). A forced activation leaves those sessions `lost` (`shim_incompatible`) and never kills them; the owner can stop them with the old CLI.
- **Rollback** to build N−1 after N started shims: shims of N speak N−1's protocol (they support the previous version), so N−1 can attach. A database snapshot restore during rollback uses §3 "Snapshot restore" and §4.2 gaps.

## 6. Auto-continue integration (decision group 4, L19)

- **Reattachment is not resume.** A reattach continues the same process with the same native identity; the station sends nothing to the provider for it and claims no continuation. The board shows `reattached`. Native resume (A10) starts a new process; the board shows `resumed`. A session is never shown as reattached when the station started a new process.
- **Order.** At recovery the station first tries to reattach (§3.2). Only a session that is `lost` can enter the L19 auto-continue path; that path stays per-session, default OFF, with all A10 identity matches and dispatch rechecks.
- **Permission mode.** A reattached process keeps its mode; the station never sends a mode change at reattach. An L19 resume passes the stored mode, never a more permissive one; a mode that the provider does not offer at resume time runs as the most restrictive mode (L8).
- **Approvals while detached.** An approval request that the provider opened while no station was attached arrives at replay. The station applies the station's offline approval policy and the request deadline using the request's provider timestamp and the station clock. An approval whose deadline passed is answered with the provider's deny or cancel option and recorded `approval.expired { reason: deadline }`. An expired approval never becomes an allow, also when a board answer for it arrives later.
- **Uncertain inputs first.** Before any continuation work or new command for a reattached session, the station settles every `sent` or `outcome_unknown` stdin effect with `write_status` (§4.3). A continuation is never a new prompt for an input whose outcome is unknown.

## 7. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| Station SIGKILL or crash | Shims and providers keep running; output waits in the buffer (backpressure when full); recovery reattaches. |
| Station graceful stop | The station detaches (sends nothing that stops providers). `station serve --stop-sessions` stops them explicitly. |
| Shim crash | The provider ends (§2); the session is `lost` (`shim_gone`); no automatic replacement. |
| Reboot | Every shim is gone; `boot_id` differs; sessions are `lost`; L19 may resume if enabled. |
| Two station processes on one database | The local-socket lock (DL-038) stops the second; generations fence the shim if it still tries. |
| Database replaced | Another incarnation: attaches are `foreign_station`; the old shims keep running until the owner stops them (`fleetdeck station shims --stop-foreign`, 7-1). |

## 8. Alternatives

- **Reuse `DirectChildHost` with `kill_on_drop(false)`.** Output written while the station is down is lost (the pipe reader is gone), and nothing fences a second owner. Rejected by PLAN Phase 7.
- **A terminal multiplexer (tmux, screen).** It gives a PTY, not the provider's JSON stdout as bytes with offsets; it adds an external dependency and a terminal emulation layer.
- **One shim for all sessions.** One crash ends every session, and one upgrade affects all; per-session shims keep failures independent.
- **systemd `KillMode=process` on the station unit only.** Linux-only and still loses the output pipe.

## 9. Tests (written before the implementation)

Real-process tests in `crates/fleetdeck-cli/tests/shim_*.rs`, unit tests in `crates/fleetdeck-providers/src/host/shim_tests.rs`. Each name is binding.

1. `a_shim_session_survives_a_station_sigkill_and_is_reattached_with_the_same_identity`
2. `output_written_while_the_station_was_down_is_replayed_from_the_committed_cursor_exactly_once`
3. `a_crash_after_an_output_read_and_before_the_commit_replays_those_bytes` (fault point between read and commit)
4. `a_partial_line_is_never_acknowledged`
5. `a_full_buffer_pauses_the_provider_and_loses_no_byte`
6. `a_cursor_below_the_floor_gives_an_explicit_gap_event_and_marks_effects_outcome_unknown`
7. `a_takeover_closes_the_old_attachment_and_a_stale_generation_cannot_write_or_stop`
8. `an_attach_with_another_incarnation_or_a_bad_proof_is_refused`
9. `a_peer_with_another_uid_is_refused` (unit test of the peer check with injected credentials)
10. `a_reused_pid_with_another_start_time_or_boot_id_is_lost_not_reattached`
11. `a_duplicate_write_id_is_written_once`
12. `write_status_resolves_an_unknown_write_after_a_restart` (`written` → `sent`; `unknown_write` → `not_dispatched`; outside the window → `outcome_unknown`)
13. `an_exit_while_the_station_was_down_is_reported_with_its_exit_state_after_reattach`
14. `an_unacknowledged_exit_is_written_to_the_exit_file_and_read_at_recovery`
15. `an_approval_whose_deadline_passed_while_detached_is_denied_and_never_allowed`
16. `a_stale_socket_without_a_live_shim_is_removed_and_the_session_is_lost`
17. `a_socket_directory_with_another_owner_or_open_mode_is_refused`
18. `dropping_every_station_handle_never_stops_the_provider`
19. `a_killed_shim_ends_its_provider_and_the_station_reports_lost`
20. `a_station_of_the_previous_build_attaches_to_a_shim_of_the_new_build` (protocol N/N−1)
21. `an_upgrade_that_cannot_speak_a_running_shim_protocol_is_not_activated` (with 7-3)
22. `a_restored_older_snapshot_reattaches_with_a_gap_and_no_duplicate_event`
23. `a_graceful_station_stop_detaches_and_the_next_start_reattaches`
24. `a_reattached_session_is_never_resumed_and_auto_continue_is_off_by_default`
25. `uncertain_inputs_are_settled_before_any_command_is_admitted_after_reattach`

macOS runs of these tests are owner/platform validation (this machine is Linux).

## 10. Risks

- A paused provider (full buffer, no station) can hit its own provider-side timeouts. The board shows `detached`; the owner can stop it.
- Same-UID processes can attach (§5.1); accepted, as for ADR-006.
- PDEATHSIG is Linux-only; on macOS a provider that ignores stdin EOF can outlive a crashed shim until its next stdout write (same limit as DL-074).

## 11. Open points

- None blocks the review. Defaults (16 MiB buffer, 7-day exit retention, 4,096 write ids) are executor choices that the owner may change.
