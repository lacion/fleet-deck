# ADR-005 — `ShimHost`: durable provider processes and reattachment

- **Status:** Proposed, revision 3. M3 round 10, 2026-10-12; DL-085. Revision 1 (commit 7965c6e with the round-3 §4.3 correction, DL-076) got adversary DESIGN REVIEW FAIL sa-f717e02d (9 MAJOR, 2 MINOR); revision 2 (DL-080, commit 2b7c937) applied it and got the re-review FAIL sa-013bfabf (4 MAJOR). Revision 3 applies every required text of the re-review; §12 maps the first review, §13 the re-review. Not implemented. The implementation starts only after the architect records an adversary DESIGN REVIEW PASS for this ADR in the "Design approvals" section of the gate-state file (M3 goal, GATE STATUS). DL-073 records an owner pre-approval; DL-074 records why the executor still waits for the review.
- **Date:** 2026-10-12
- **Deciders:** architect (goal loop, M3), owner (pre-approval, DL-073), adversary DESIGN REVIEW (revision 1: FAIL sa-f717e02d; revision 2: FAIL sa-013bfabf; revision 3: pending)
- **Implements:** PLAN A9, A10 (Phase 7 part, L19), Phase 7 "`ShimHost` (Revision 3 contract)" and its "Required ADR-005 decisions"; ADR-001 §host boundary (`SessionHost`, `ProcessControl` in `crates/fleetdeck-providers/src/host/mod.rs`)
- **Evidence:** none yet. §9 lists the tests that come before the implementation. The station SIGKILL test of DL-074 (`crates/fleetdeck-cli/tests/station_sigkill.rs`) is the baseline for the direct host.

## 1. Context

The MVP host is `DirectChildHost` (`crates/fleetdeck-providers/src/host/direct.rs`): the provider is a child of the station, in its own process group, with `kill_on_drop(true)`. A station restart ends every provider (DL-074 proves it for SIGKILL on Linux, for providers that end at stdin EOF), and A10 recovers through native resume, which can lose in-flight work and open approvals.

Phase 7 requires a host that keeps the provider and its output alive while the station restarts or upgrades, and lets the new station process reattach. PLAN Phase 7 fixes these rules: the shim preserves execution and output; a PID alone is not process identity; fencing and exclusive reattachment (one station incarnation owns an attachment at a time); bounded buffering and backpressure; stdout cursor recovery; an explicit exit state; socket permissions; update and restart failure tests; no reuse of the "last reference drop kills the child" behavior. The design review added four decision groups (attachment ownership, output recovery, platform and upgrades, auto-continue integration). This ADR decides them.

A reattachment has two halves: the **process** half (the shim keeps the provider and its pipes; §3–§4) and the **adapter** half (the station's provider adapter keeps protocol state in memory: Codex RPC correlations and open approvals in `crates/fleetdeck-providers/src/codex/core.rs:106–190`, Claude initialization in `crates/fleetdeck-providers/src/claude/driver.rs:129–213`). A fresh adapter cannot continue a live process. §2.1 defines the adapter recovery path.

## 2. Shape

- **Shim process.** `fleetdeck shim --id <shim_id>` (a hidden subcommand of the same binary; the id is not secret) runs one provider process. The station starts the shim; the shim starts the provider as its own child and owns the provider's stdin, stdout and stderr pipes.
- **Detach.** The shim calls `setsid()` and so leaves the station's session and process group. A station death (also SIGKILL) does not signal it. The provider runs in its own session and process group under the shim (`setsid()` in the child). On Linux the shim is a child subreaper (`PR_SET_CHILD_SUBREAPER`) and sets `PR_SET_PDEATHSIG(SIGKILL)` on the provider.
- **Shim loss is not proof that provider execution ended.** Linux PDEATHSIG terminates the direct child only; descendants survive it. EOF on macOS is not a termination guarantee. §4.6 defines what the station does after a shim loss.
- **Station side.** `ShimHost` (`kind() == HostKind::Shim`, `survives_station_restart() == true`) and a `ShimControl` over the shim socket. Dropping a `ShimControl` or a `HostedProcess` only closes the station's connection; it never stops or kills the provider. Only `stop`, `kill` and an explicit owner action end a provider.
- **Host interface.** `SessionHost::spawn` and `ProcessControl::write(bytes)` are not enough for a durable host. `ShimHost` adds a `DurableProcess` interface:
  - `attach(record) -> Attached { provider_identity, cursor_floor, exit? }` (§3);
  - offset-bearing output: `OutputChunk { offset, read_at, bytes }`, where `offset` is the absolute stdout byte offset and `read_at` is the shim's wall-clock time when it read the bytes from the pipe;
  - `ack(cursor)`: acknowledged-cursor advancement after the commit (§4.1);
  - `write(meta: WriteMeta, bytes)`: metadata-bearing writes (§2.1, §4.3);
  - the control frames of §4.4.
  `DirectChildHost` keeps the M2 interface; nothing in the shim path reuses its drop behavior.
- **Selection.** The host is a station setting (`host = "direct" | "shim"`, default `direct` until 7-1 passes its tests; then `shim` on Linux and macOS). A session keeps the host it started with.

### 2.1 Adapter recovery path

- Reattachment uses a separate adapter recovery path. It never calls `spawn`, `initialize`, `thread/start`, or native resume.
- Every adapter transition is recoverable, including command-side transitions that produce no provider output (for example a Codex JSON-RPC request id and its pending correlation, a pending turn before `turn/start` is answered, a queued follow-up while a turn runs, and a mode or model change). Before outbound I/O, the station commits the corresponding adapter state or replayable transition with its effect and immutable dispatch metadata, in the claim transaction of §4.3.
- For an output transaction, the station commits generated internal effects, adapter state, events, and cursor together before acknowledgment (§4.1). Recovery reconciles these transitions without resending claimed frames. Proven unwritten answers (§4.3 `retired_not_dispatched` rows) retain or restore their open request mappings.
- The adapter state is a versioned adapter checkpoint plus the replayable transitions committed after it. It includes native identities, event-id counters, the RPC id counter, RPC correlations, pending inputs and pending turns, open request mappings, and permission, mode and model state. It holds no secret (credentials stay in `Secret<T>` and are never part of it).
- Recovery restores the latest checkpoint into a new adapter instance and applies the transitions committed after it, in commit order, before replay. The replay from the stored cursor (§4.1) then continues exactly where that state ends, because the state and the cursor were committed in the same transactions. A restored RPC id counter is never below an id that a committed transition used, so no RPC id is reused, and a response for a committed outbound request finds its correlation.
- Every outbound provider frame receives durable dispatch metadata before I/O. `WriteMeta { effect_id, frame_ordinal, shim_id, write_seq }` is committed in the claim transaction (§4.3). Frames generated by protocol handling (for example a Codex JSON-RPC response to a server request, or an initialization frame) use internal effects with stable identities (`internal:<session>:<kind>:<ordinal>`), so they follow the same §4.3 rules.
- An unsupported checkpoint version makes the session `not_resumable` (`adapter_checkpoint_unsupported`). It does not create a fresh adapter for the live process; the owner can stop the process.
- Each adapter declares its checkpoint version. A station build reads its own version and the previous one (§5.3).

## 3. Attachment ownership (decision group 1)

- **Two separate values.** The *station incarnation* is the id of the station database (PLAN §10 `meta`); it changes only when the database is replaced. The *attachment-owner generation* is a per-shim counter (u64) that rises at each attach. They are never mixed: the incarnation answers "is this my shim?", the generation answers "which station process owns it now?".
- **Launch journal.** Launch fencing uses a durable journal and lock in the persistent station directory, independent of socket placement: `<station dir>/shims/<shim_id>.journal` and `<station dir>/shims/<shim_id>.lock` (directory mode 0700, files 0600; never in `$XDG_RUNTIME_DIR` or a temporary directory, §5.1). Under that lock, the shim durably records `spawn_attempted` (write, fsync the file, fsync the directory) before spawning the provider. The station creates the journal with the record `created { shim_id, incarnation }` before it starts the shim.
- **Launch.** Before spawning, the station durably publishes the key file (below; written, fsynced, renamed, directory fsynced) and the launch journal, and commits the `starting` row with its immutable launch identity: `shim_id` (ULID), `session_id`, `incarnation`, `state = starting`, `owner_generation = 0`, `socket_path`, `cursor = 0`, `next_write_seq = 1`. It then starts the shim. A missing provider identity is valid only in `starting`.
- **Launch lock.** At start the shim takes an exclusive `flock` on `<station dir>/shims/<shim_id>.lock` and holds it for its whole life. It then reads the journal; with a `fenced` record it exits without binding a socket or starting a provider. Without it, the shim binds its socket, durably appends `spawn_attempted` to the journal, then starts the provider, then reports `ready { provider_identity }`. The station stores the identity (`state = running`).
- **Attach key.** The station creates 32 random bytes per shim and passes them to the shim through an inherited pipe (never argv, environment, URL or log). The key file `<station dir>/shims/<shim_id>.key` (mode 0600 inside the 0700 directory of the launch journal, §3 "Launch journal") holds the same bytes; it never enters the database or events. The key proves that the attaching station holds the record of the station that created the shim.
- **Attach handshake** (one Unix socket connection):
  1. The shim checks the peer UID (`SO_PEERCRED` on Linux, `getpeereid` on macOS) equals its own; else it closes the connection.
  2. The shim sends `hello { protocols, shim_build, shim_id, nonce }`. The station picks a protocol (§5.3).
  3. The station sends `attach { incarnation, generation, proof = HMAC-SHA256(key, nonce ‖ incarnation ‖ generation) }`, where `generation` is the stored `owner_generation + 1`, committed to the database before the send.
  4. The shim accepts only if the proof is valid, `incarnation` equals the one it got at start, and `generation` is greater than its current owner generation. Else it answers `refused { reason: bad_proof | foreign_station | stale_owner }` and closes.
  5. On accept, the shim sets its owner generation, closes any older attachment connection at once (takeover, §4.3), and answers `attached { generation, provider_identity, cursor_floor, exit? }`.
- **Stale writers.** Every frame from the station carries the generation. The shim drops the frame and closes the connection when the generation is not the current one, so an old station process (or a paused one that wakes up) cannot write stdin or stop the provider after a takeover. Only one connection is attached at a time; a second `attach` with the same or a lower generation gets `stale_owner`.

### 3.1 Process identity

A PID alone is never identity. `provider_identity = { pid, start_time, boot_id }`: Linux `/proc/<pid>/stat` field 22 (start time in clock ticks) and `/proc/sys/kernel/random/boot_id`; macOS `proc_pidinfo(PROC_PIDTBSDINFO)` start time and `sysctl kern.bootsessionuuid`. The shim reports the identity of its own child; the station compares it with the stored one at each attach. A difference (PID reuse, reboot) means the session is `lost` with the reason `identity_mismatch`; the station never adopts that process.

### 3.2 Recovery at station start (before new commands, A10 / L2)

For each `shims` row of this incarnation in state `starting` or `running`:

1. **`starting` rows first.** A failed connection during `starting` does not prove that spawning never occurred, and missing runtime files (socket, exit file) do not prove that a launch never occurred: a reboot or a cleaner can remove them. Recovery fences or resolves the launch before classifying it: it tries the non-blocking `flock` of `<station dir>/shims/<shim_id>.lock`.
   - Lock acquired: no live shim holds it. Recovery durably appends `fenced` to the journal while it holds the lock, so a late shim start exits without a provider. Recovery classifies a launch as never started only from an intact journal that proves no spawn attempt occurred (a valid `created` record of this shim and incarnation, no `spawn_attempted` record, then `fenced`): the start effect is `retired_not_dispatched` (`shim_never_started`). Missing, invalid, or lost launch evidence (no journal, an unreadable or truncated journal, another shim id or incarnation, or a `spawn_attempted` record) produces `outcome_unknown` for the start effect and a `lost` session (`execution_unknown`, §4.6), never `retired_not_dispatched`. A socket file or an exit file adds evidence for step 3; its absence proves nothing.
   - Lock held: a shim is alive. Recovery waits at most 30 s for its socket and attaches (step 2). After authenticating that launch, recovery commits the first reported provider identity before admitting input. If no attach succeeds, the session is `lost` (`execution_unknown`, §4.6).
2. **Attach succeeds with the same provider identity** → restore the adapter checkpoint (§2.1), replay from the stored cursor (§4.1), settle stdin writes (§4.3), reconcile open requests (§6), then the session is `reattached`.
3. **Socket refuses connections** (stale file) → read `<shim_id>.exit` if it is valid (§4.5) → report the exit. Otherwise the session is `lost` (`execution_unknown`, §4.6). Remove the stale socket file only after this check.
4. **Attach refused** with `foreign_station` or `bad_proof` → `lost` (`attach_refused`, `execution_unknown`); the station leaves that shim alone (it may belong to another station directory).
5. **Protocol incompatible** (§5.3) → `lost` (`shim_incompatible`), `not_resumable` (§6).

Only after every row is settled does the station admit commands.

### 3.3 Database rollback and restore

- Binary rollback preserves the latest station database, receipts, effect journal, and write-sequence counters. It does not restore historical database contents.
- The 7-3 launcher snapshot (main, WAL and SHM) is taken while the old station is stopped and before the new build migrates. An activation trial eligible for snapshot rollback performs no attachment-generation advancement, output acknowledgment, exit acknowledgment, or durable recovery reconciliation. It performs compatibility and migration checks on a disposable database copy, and it reads shim protocol and checkpoint versions from the `shims` rows and journals, without an attach (§5.3). Only then does the snapshot equal the latest committed state.
- After any live recovery state advances (an attach generation, an output or exit acknowledgment, a committed reconciliation, an admitted command or a claimed effect), rollback preserves the latest committed database. The launcher never restores its pre-activation snapshot after that point; a later failure rolls back the binary only (first bullet).
- Historical database restore (any restore that loses a committed receipt, effect or write sequence) is refused while any shim from that database remains live or has an unresolved execution outcome.
- A permitted historical restore creates a new database incarnation before command admission. Commands addressed to the previous incarnation are rejected under G3 (`incarnation_mismatch`). Old shims are not adopted automatically: their attaches get `foreign_station`. Their sessions remain `lost`/`not_resumable` until the owner resolves them (`fleetdeck station shims --stop-foreign`, 7-1).

## 4. Output, input and exit (decision group 2)

### 4.1 Cursor

- The shim numbers the provider's stdout bytes with an absolute offset from 0. It keeps every byte at or above the *acknowledged cursor* in a buffer of B bytes (station setting, default 16 MiB, 1–256 MiB).
- The station parses only complete lines. For an output transaction, the station commits generated internal effects, adapter state (§2.1), events, and `cursor = offset after the last complete line` (in the `shims` row) together before acknowledgment. After the commit it sends `ack { generation, cursor }`; the shim frees the bytes below it.
- **Crash after a read, before the commit.** The bytes are not acknowledged, so the shim still holds them. The new station attaches and asks `replay { from: stored cursor }`. The events of those bytes are produced once, because the cursor, the adapter state, the internal effects and the events were committed together.
- **Partial lines.** A partial line at the end of a read stays in the station's parser buffer and is never acknowledged. Replay and gaps never begin inside an accepted line: the stored cursor is always a line boundary.
- **stderr** keeps a bounded tail (8 KiB, `STDERR_TAIL_BYTES`) in the shim; it is part of the exit report.

### 4.2 Backpressure, line size and gaps

- When the unacknowledged bytes reach B, the shim stops reading the provider's stdout. The provider then blocks on its own write. No byte is dropped. The shim reads again when an `ack` frees space. While no station is attached, the same rule applies; the board shows the session as `detached` until the reattach (no claim of progress).
- **Maximum line size.** B also defines the maximum line size, including its newline. A line that reaches B bytes without a newline fails the session with `output_frame_too_large`. The shim detects it itself (the buffer is full and holds no newline after the acknowledged cursor), stops the provider (§4.4 stop policy) and reports the discarded uncommitted range `{ from, to }` explicitly; it never waits indefinitely for an acknowledgment. The station commits a `session.output_gap { from, to, reason: output_frame_too_large }` event and the failure.
- **Unterminated last line.** A trailing unterminated line at provider EOF is malformed output. The station commits its explicit failure or gap record (`session.output_gap { reason: unterminated_line }`) and the final consumed cursor before acknowledging exit (§4.5).
- **Gaps below the floor.** A request below the shim's `cursor_floor` (the oldest byte it still holds) is a defect after §3.3. The shim answers `gap { from, to }` and replays from `to`. The station commits `session.output_gap { from, to, reason: below_floor }` (a new kind, decoded as `EventBody::Unknown` by older peers, L16), marks every effect whose confirmation could be inside the gap `outcome_unknown`, and invalidates the adapter checkpoint: the session becomes `not_resumable` (`adapter_state_gap`), because protocol state inside the gap is unknown. It never invents events for the missing bytes.

### 4.3 Stdin and duplicate writes

The M2 effect rules stay binding (`crates/fleetdeck-station/src/store/effects.rs`): an effect is `queued → claimed → confirmed`, or ends `retired_not_dispatched` or `outcome_unknown`; a claim is never dispatched again; a pipe write is never a confirmation. The shim adds one thing only: in narrow, proven cases it lets recovery retire a `claimed` stdin write as not dispatched instead of `outcome_unknown`. Every other case stays `outcome_unknown`. A partial or in-flight write is never `not_dispatched`.

**Immutable dispatch identity.** A dispatch identity is immutable. A retired effect identity never accepts another dispatch. Releasing an answer slot requires a new command and a new effect identity (M2 `crates/fleetdeck-station/src/store/effects.rs:453–476`), and so a new `write_id` and a new `write_seq`.

**Write sequence.** In the claim transaction (before any I/O) the station stores `WriteMeta { effect_id, frame_ordinal, shim_id, write_seq }` on the effect; `write_seq` comes from the `shims` row counter `next_write_seq` (incremented in the same transaction), so it is unique and strictly rising per shim. The frame is `write { generation, write_id, write_seq, bytes }`; `write_id` is the effect id plus the frame ordinal.

**Bounds.** A transport frame is at most 1 MiB. The station checks the encoded size at admission: an oversized input gets a committed rejection (§6.4 rejection receipt) before any effect is claimed. The shim permits one active stdin write and at most 256 pending writes, with a combined pending-byte limit of 4 MiB. A complete write refused for capacity creates a `failed_unwritten { error: capacity }` result and sends no provider bytes.

**Admission checks of the shim.** Before creating a write record or touching stdin, the shim rejects:
- an absent write identity whose `write_seq` is at or below `evicted_through`;
- a `write_seq` that another record or tombstone already holds (sequence reuse);
- a known `write_id` with another `write_seq` (identity/sequence mismatch).
Such writes return `forgotten` (or `mismatch`) and perform no I/O. A known `write_id` with its own `write_seq` is a duplicate: it is not written again, and the shim answers with the stored state.

**Shim record.** The shim keeps one record per write identity, in memory, bound to its immutable identity and sequence. Records live as long as the shim. After a shim loss no further frame can reach the provider's stdin (the shim held the only write end), and every open write is `outcome_unknown` (§4.6). States:

| State | Meaning |
| --- | --- |
| `received` | The complete frame was parsed and admitted. The shim creates this record **before** the first byte goes to the pipe. |
| `writing { bytes_done }` | The write loop has started; `bytes_done` may be 0 or more. |
| `written` | All bytes are in the pipe. |
| `partial { bytes_done, error }` | The write stopped with an error after 1 or more bytes. |
| `failed_unwritten { error }` | No byte was written: an error before the first byte (for example `EPIPE` on the first call), a capacity refusal, or a pending write cancelled by a takeover (`superseded`). |
| `closed_unwritten` | Tombstone from a `not_received` status answer (below). It refuses every later frame of its identity. |

- A frame is processed only when it is complete (length prefix satisfied). An incomplete frame from a closed or replaced connection is discarded; no byte of it reaches the pipe.
- A frame whose generation is not current is dropped and creates no record (§3 "Stale writers").
- **Takeover.** Takeover discards the old connection's buffered frames without waiting for further bytes. Pending writes of the old generation that have not started become `failed_unwritten { superseded }`; the active write (if any) continues and keeps its record. No old frame is processed after `attached`.
- **Eviction.** The shim keeps up to 4,096 records and tombstones in a terminal state (`written`, `partial`, `failed_unwritten`, `closed_unwritten`) and never evicts `received` or `writing`. It keeps `evicted_through` = the highest `write_seq` that it ever evicted. Eviction can remove detailed results, but it never removes the rejection watermark.
- While the station is attached, the shim reports each terminal state with `write_done { write_id, write_seq, state, bytes_done }`. The live worker maps it with the table below; `written` gives `Dispatch::Sent`.

**Status after a reattach.** Recovery (§3.2) runs `write_status { write_id, write_seq }` for every `claimed` stdin write of the shim, **before** the generic M2 recovery turns `claimed` into `outcome_unknown`. The answer is valid only on an attachment that passed the full handshake (proof, incarnation, generation, provider identity). The shim answers `not_received` only when it has no record for the identity **and** `write_seq > evicted_through` **and** no record holds that `write_seq`; it then stores the `closed_unwritten` tombstone in the same step, so the old frame can never be written later.

| Answer | Effect result |
| --- | --- |
| `not_received` | `retired_not_dispatched` (reason `shim_not_received`). A later dispatch needs a new command and a new effect identity. |
| `failed_unwritten` | `retired_not_dispatched` (reason `shim_write_failed`). |
| `written` | `outcome_unknown`; provider evidence in the replayed output can still confirm it (M2 `observe`). |
| `received` or `writing` (in flight) | The station asks again for at most 30 s. A terminal answer in this time uses this table. Else `outcome_unknown`; a later `write_done` can only move it to `confirmed` through provider evidence, never to `retired_not_dispatched`. |
| `partial` | `outcome_unknown`, never resent. Stdin may now hold a broken line, so the session gets `input_blocked { reason: stdin_partial_write }`: the station rejects new inputs (§6.4 rejection receipt) and admits only interrupt, stop and kill. |
| `forgotten`, `mismatch`, no answer, shim lost, `identity_mismatch` | `outcome_unknown`, never resent. |

- The bounded waits of all shims run in parallel, so they add at most 30 s to recovery; commands are admitted only after them (§3.2).
- An effect that is already `outcome_unknown` (for example from an earlier recovery) never goes back to `retired_not_dispatched`. Only provider evidence can settle it. No recovery result derives not-dispatched status from shim death.
- **Gate in the implementation.** The two `retired_not_dispatched` rows are enabled only in the same change that adds the crash-boundary tests 26–35 (§9), and only when they pass. Until then, the station maps every `write_status` answer to `outcome_unknown`.

### 4.4 Control and independent paths

- `stop { policy }`, `kill`, `close_stdin`, `interrupt` and `status` are frames with the generation. They work only on the current attachment.
- Socket reading, response writing, and provider stdin writing run independently in the shim. Takeover, status, interrupt, `close_stdin`, stop, and kill do not wait for stdin capacity or for a blocked stdin write. Queues remain bounded during disconnect and cancellation (§4.3 bounds; at most 64 queued control frames, then the connection is closed).
- `stop` and `kill` signal the provider's process group and session (§2). On provider exit, the Linux subreaper shim also signals the remaining members of that process group before it reports the exit.

### 4.5 Exit state

- When the provider ends, the shim stores `ExitReport { termination, cause, stderr_tail, descendants_remaining }` and the final cursor. It keeps running and serving the replay.
- The station sends `ack_exit` only after committing the exit report, the terminal session state, and the final output cursor or an explicit gap (§4.2). The shim then removes its socket, key file and lock file, and ends.
- If no station acknowledges within 7 days, the shim publishes `<shim_id>.exit` (mode 0600, JSON, no secret) with the report and the final cursor by atomic replacement (write a temporary file, fsync, rename, fsync the directory) and ends. The remaining unacknowledged output is lost; the station records it as a gap at recovery.
- If publication fails, including `ENOSPC`, the shim retains the report and continues serving it; it does not discard the report and exit successfully. It tries the publication again every hour.
- A missing or invalid exit file never proves a clean exit.

### 4.6 Shim loss

- After shim loss, the session becomes `lost` with `execution_unknown`. Claimed effects remain `outcome_unknown`.
- The station admits no new execution and performs no native auto-resume until it verifies that the original provider execution has ended.
- Absence from the original provider session and process group does not prove complete execution termination: a descendant can call `setsid()` and leave both. After shim loss, automatic resume requires evidence that covers escaped descendants. A verified reboot is sufficient evidence: the current `boot_id` differs from the stored one. This revision defines no other sufficient evidence.
- The session-and-group check (no live process has the stored `provider_identity`, and no live process is a member of the provider's session or process group; Linux `/proc/*/stat` fields 5–6, macOS `proc_listpids(PROC_PGRP_ONLY)` and the session id) is a necessary condition only. The station shows its result to the owner; it never enables a resume on its own.
- Without complete termination evidence, the session remains `execution_unknown` and `not_resumable` until audited owner reconciliation (the owner stops the remaining processes, then marks the session reconciled; an audited owner action). Process-inspection errors fail closed: an unreadable `/proc` entry, a permission error or an interrupted listing counts as missing evidence.

## 5. Platform and upgrades (decision group 3)

### 5.1 Socket placement and authentication

- Directory: `$XDG_RUNTIME_DIR/fleetdeck/<incarnation>/` on Linux when `XDG_RUNTIME_DIR` is set; otherwise `<station dir>/run/shims/`. macOS: `<station dir>/run/shims/`. The station creates the directory with mode 0700 and refuses to use it if its owner is another UID or its mode allows group or other access.
- Socket file: `<shim_id>.s` with the full 26-character ULID (collision-resistant), mode 0600. The full path must fit `sun_path` (108 bytes Linux, 104 macOS); if it does not, the station uses `${TMPDIR:-/tmp}/fd-<uid>-<station incarnation>/` with the same owner and mode checks. A path that still does not fit refuses the shim start (`socket_path_too_long`); no name is shortened.
- Authentication: directory and file modes, the peer-UID check, and the HMAC attach proof (§3). The same-UID boundary of ADR-006 §3 also applies here: a same-UID process can read the key file and attach; it already controls the station.

### 5.2 Stale sockets

A socket file whose connect fails with `ECONNREFUSED` or `ENOENT` on the target is stale. The station removes it only in recovery step 3 (§3.2). A shim refuses to start when its socket path exists and accepts connections.

### 5.3 Protocol negotiation and upgrades

- The shim protocol is length-prefixed JSON frames with a version number. A shim build supports its own version and the previous one; a station build also supports its own and the previous one, for the shim protocol and for the adapter checkpoint (§2.1). `hello` lists the shim's versions; the station picks the highest common one.
- A station upgrade restarts only the station process; running shims keep their old code (the executable stays mapped). New sessions get the new shim build.
- **Incompatible upgrade.** The 7-3 activation trial lists the protocol and checkpoint versions of all running shims from their `shims` rows and launch journals (recorded at launch), without an attach or an acknowledgment (§3.3). A new build that cannot speak one of them is not activated; the old build keeps running (rollback). A forced activation leaves those sessions `lost` (`shim_incompatible`) and `not_resumable` (§6); it never kills them; the owner can manage them with a compatible build.
- **Rollback** to build N−1 after N started shims: shims of N speak N−1's protocol (they support the previous version), and N writes adapter checkpoints that N−1 can read only if N kept the previous checkpoint version; otherwise the activation trial of N−1 refuses (same rule as above). The database is the latest one (§3.3).

## 6. Auto-continue integration (decision group 4, L19)

- **Reattachment is not resume.** A reattach continues the same process with the same native identity; the station sends nothing to the provider for it and claims no continuation. The board shows `reattached`. Native resume (A10) starts a new process; the board shows `resumed`. A session is never shown as reattached when the station started a new process.
- **Resume eligibility.** Resume eligibility requires complete termination evidence for the previous provider execution, including escaped descendants (§4.6): a verified reboot, or an audited owner reconciliation. `lost` is a display state, not proof of termination. At recovery the station first tries to reattach (§3.2); only a `lost` session with that evidence can enter the L19 auto-continue path, which stays per-session, default OFF, with all A10 identity matches and dispatch rechecks.
- **Incompatible or unresolved shims.** A live incompatible shim becomes `lost`/`shim_incompatible` and `not_resumable`. Auto-continue never resumes it. The owner can manage it with a compatible build. The same rule applies when attachment refusal or an unresolved identity prevents proof that the old execution ended.
- **Permission mode.** A reattached process keeps its mode; the adapter checkpoint restores the station's view of it, and the station never sends a mode change at reattach. An L19 resume passes the stored mode, never a more permissive one; a mode that the provider does not offer at resume time runs as the most restrictive mode (L8).
- **Approvals across a restart.** Recovery preserves each session's stored offline approval policy. Existing requests retain their original `requested_at` and `deny_at`; reattachment never restarts their timeout.
- Replayed requests use a proven original request time when available: the `read_at` of the `OutputChunk` that carried the request line (§2), recorded by the shim when it read the bytes. A `deny_after` request with an unprovable original time expires conservatively and receives the provider's safe deny/cancel response. Replay time is never its original request time.
- Recovery restores or reconciles already-committed open request channels before command admission: an open request whose adapter mapping is in the restored checkpoint (§2.1) stays answerable; an unverified channel expires (`approval.expired { reason: channel_unverified }`) and gets the safe deny/cancel response when the protocol allows it. Queued allow effects are rechecked against request expiry before dispatch; expired requests never dispatch an allow, also when a board answer for them arrives later.
- **Uncertain inputs first.** Before any continuation work or new command for a reattached session, the station settles every `claimed` stdin write with `write_status` (§4.3). An input that stays `outcome_unknown` (in flight, partial, written without evidence, forgotten) is never sent again, and a continuation is never a new prompt for it.

## 7. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| Station SIGKILL or crash | Shims and providers keep running; output waits in the buffer (backpressure when full); recovery restores the adapter checkpoint and reattaches. |
| Station crash during a launch | The launch lock and journal fence or resolve the launch (§3.2 step 1); a launch is never classified from a failed connection or from missing runtime files. |
| Station graceful stop | The station detaches (sends nothing that stops providers). `station serve --stop-sessions` stops them explicitly. |
| Shim crash | The session is `lost` (`execution_unknown`); its `claimed` stdin writes become `outcome_unknown`; no new execution and no resume without complete termination evidence (a verified reboot or an audited owner reconciliation, §4.6); no automatic replacement. |
| Station crash after the provider spawn, before the identity commit, then a reboot | The persistent launch journal holds `spawn_attempted`; the start effect is `outcome_unknown`, never `retired_not_dispatched` (§3.2 step 1). |
| Line larger than the buffer | The session fails `output_frame_too_large` with an explicit gap; the provider is stopped (§4.2). |
| Partial stdin write | The write is `outcome_unknown`; the session is `input_blocked` (`stdin_partial_write`) and admits only interrupt, stop and kill (§4.3). |
| Stdin blocked | Control frames and takeover still work (§4.4); new writes beyond the bounds are `failed_unwritten { capacity }`. |
| Exit file cannot be written | The shim keeps the report and keeps serving it (§4.5). |
| Reboot | Every shim is gone; `boot_id` differs; termination is verified by the new boot; sessions are `lost`; L19 may resume if enabled. |
| Two station processes on one database | The local-socket lock (DL-038) stops the second; generations fence the shim if it still tries. |
| Database replaced or historically restored | Another incarnation: attaches are `foreign_station`; the old sessions stay `lost`/`not_resumable` until the owner resolves them (§3.3). |

## 8. Alternatives

- **Reuse `DirectChildHost` with `kill_on_drop(false)`.** Output written while the station is down is lost (the pipe reader is gone), and nothing fences a second owner. Rejected by PLAN Phase 7.
- **A terminal multiplexer (tmux, screen).** It gives a PTY, not the provider's JSON stdout as bytes with offsets; it adds an external dependency and a terminal emulation layer.
- **One shim for all sessions.** One crash ends every session, and one upgrade affects all; per-session shims keep failures independent.
- **systemd `KillMode=process` on the station unit only.** Linux-only and still loses the output pipe.
- **Re-initialize a fresh adapter on the live process.** A second `initialize` or `thread/start` changes the conversation and loses RPC correlations; rejected (§2.1).

## 9. Tests (written before the implementation)

Real-process tests in `crates/fleetdeck-cli/tests/shim_*.rs`, unit tests in `crates/fleetdeck-providers/src/host/shim_tests.rs`. Each name is binding.

1. `a_shim_session_survives_a_station_sigkill_and_is_reattached_with_the_same_identity`
2. `output_written_while_the_station_was_down_is_replayed_from_the_committed_cursor_exactly_once`
3. `a_crash_after_an_output_read_and_before_the_commit_replays_those_bytes` (fault point between read and commit)
4. `a_partial_line_is_never_acknowledged`
5. `a_full_buffer_pauses_the_provider_and_loses_no_byte`
6. `a_cursor_below_the_floor_gives_an_explicit_gap_marks_effects_outcome_unknown_and_makes_the_session_not_resumable`
7. `a_takeover_closes_the_old_attachment_and_a_stale_generation_cannot_write_or_stop`
8. `an_attach_with_another_incarnation_or_a_bad_proof_is_refused`
9. `a_peer_with_another_uid_is_refused` (unit test of the peer check with injected credentials)
10. `a_reused_pid_with_another_start_time_or_boot_id_is_lost_not_reattached`
11. `a_duplicate_write_id_is_written_once`
12. `write_status_maps_each_answer_to_the_effect_state_of_section_4_3` (unit test of the full answer table; an effect already `outcome_unknown` never becomes `retired_not_dispatched`)
13. `an_exit_while_the_station_was_down_is_reported_with_its_exit_state_after_reattach`
14. `an_unacknowledged_exit_is_published_atomically_to_the_exit_file_and_read_at_recovery`
15. `approval_deadlines_survive_a_restart_and_an_expired_or_unprovable_request_is_denied_and_never_allowed` (an open request keeps `requested_at`/`deny_at`; a replayed request uses `read_at`; a request without a proven time expires; a queued allow after expiry is not dispatched)
16. `a_stale_socket_without_a_live_shim_is_removed_and_the_session_is_lost_with_execution_unknown`
17. `a_socket_directory_with_another_owner_or_open_mode_is_refused`
18. `dropping_every_station_handle_never_stops_the_provider`
19. `a_killed_shim_leaves_the_session_lost_and_not_resumable_until_termination_is_verified`
20. `a_station_of_the_previous_build_attaches_to_a_shim_of_the_new_build` (protocol and checkpoint N/N−1)
21. `an_upgrade_that_cannot_speak_a_running_shim_protocol_is_not_activated` (with 7-3)
22. `a_historical_restore_is_refused_while_a_shim_is_live_and_a_permitted_one_gets_a_new_incarnation` (snapshot S, command C accepted and effect E written, response lost; restore of S is refused while the shim lives; after it ends, the permitted restore gets a new incarnation and a retry of C is `incarnation_mismatch`, never a second acceptance)
23. `a_graceful_station_stop_detaches_and_the_next_start_reattaches`
24. `a_reattached_session_is_never_resumed_and_auto_continue_is_off_by_default`
25. `uncertain_inputs_are_settled_before_any_command_is_admitted_after_reattach`

**§4.3 crash-boundary tests.** They are a condition for the `retired_not_dispatched` rows of §4.3 (§4.3 "Gate in the implementation"). Each real-process test uses a fake provider that records every stdin byte it reads, and a test-build fault point in the station or the shim. Each asserts the effect state, the bytes that the provider read, and that no write is sent twice.

26. `a_station_killed_after_the_claim_and_before_the_frame_retires_the_write_as_not_received` (the provider read no byte)
27. `a_station_killed_in_the_middle_of_a_frame_leaves_no_byte_in_the_pipe_and_retires_the_write`
28. `a_station_killed_after_the_full_frame_and_before_written_keeps_the_write_outcome_unknown_and_never_resends` (the shim finishes the write; the provider read the bytes exactly once; replayed evidence confirms it)
29. `a_write_blocked_on_a_full_stdin_pipe_at_reattach_stays_outcome_unknown_after_the_bounded_wait` (a later `write_done` never retires it; only provider evidence confirms it)
30. `a_partial_write_is_outcome_unknown_blocks_input_and_is_never_resent` (shim unit test with an injected writer that fails after k > 0 bytes; then a real-process check that the session rejects new inputs with a receipt and admits stop)
31. `a_write_that_fails_before_the_first_byte_is_retired_as_not_dispatched` (injected `EPIPE` on the first call)
32. `an_evicted_write_record_is_forgotten_stays_outcome_unknown_and_a_resubmitted_frame_is_refused_without_io` (eviction followed by a duplicate write of the same identity under the current generation)
33. `a_complete_frame_of_the_old_generation_after_a_takeover_is_dropped_and_never_written` (the status is `not_received`; the provider read no byte of it)
34. `a_shim_killed_during_a_write_leaves_the_session_lost_and_the_write_outcome_unknown`
35. `a_not_received_tombstone_refuses_every_later_frame_of_its_identity_and_a_released_slot_needs_a_new_effect`

**Review additions (sa-f717e02d).**

36. `a_reattached_codex_session_answers_an_approval_opened_before_the_crash_with_the_restored_checkpoint` (station commits and acks the request line, crashes; the new station restores the checkpoint, processes later responses, answers the open approval; no `initialize` or `thread/start` frame reaches the provider)
37. `an_unsupported_adapter_checkpoint_version_makes_the_session_not_resumable_without_a_fresh_adapter`
38. `a_line_of_the_buffer_size_without_a_newline_fails_the_session_with_an_explicit_gap_and_stops_the_provider`
39. `an_unterminated_last_line_at_eof_is_committed_as_malformed_before_the_exit_is_acknowledged`
40. `an_eof_ignoring_provider_or_a_surviving_descendant_after_shim_loss_keeps_the_session_not_resumable` (Linux: a fake provider that ignores EOF, a descendant in its process group, and a descendant that called `setsid()`; the station never resumes; after the original provider and its group disappear, the escaped descendant is still alive and the session is still `execution_unknown`/`not_resumable` with no automatic resume; a process-inspection error is treated as missing evidence; only a simulated new `boot_id` or an audited owner reconciliation makes it eligible)
41. `a_blocked_stdin_write_does_not_block_kill_takeover_or_status`
42. `pending_writes_beyond_256_or_4_mib_are_failed_unwritten_and_an_oversized_input_gets_a_rejection_receipt`
43. `a_station_crash_during_a_launch_is_fenced_or_resolved_by_the_launch_lock_before_classification`
44. `an_exit_file_publication_failure_keeps_the_report_served` (injected `ENOSPC`)
45. `ack_exit_is_sent_only_after_the_exit_state_and_final_cursor_are_committed` (fault point between the commit and `ack_exit`, and before the commit)

**Re-review additions (sa-013bfabf).**

46. `a_crash_after_an_outbound_dispatch_before_any_response_recovers_the_correlation_and_never_reuses_an_rpc_id` (Codex: claim and send a `turn/start`, kill the station before the response; the new station restores the committed transition, the replayed response finds its correlation, and the next request id is above every committed one)
47. `a_busy_follow_up_and_a_mode_or_model_change_without_later_stdout_survive_a_restart` (the transitions have no provider output; after reattach the adapter state holds the queued follow-up and the new mode and model, and nothing is resent)
48. `a_failed_activation_after_an_output_commit_and_ack_keeps_events_checkpoint_cursor_and_generation` (the new build attaches, commits and acknowledges output, then fails before command admission; the launcher keeps the latest database and does not restore its snapshot; events, adapter state, cursor and attachment generation are unchanged; and a trial that stays eligible for rollback performs no attach and no acknowledgment)
49. `a_launch_interrupted_after_the_provider_spawn_and_before_the_identity_commit_is_never_retired_as_not_dispatched` (kill the station after `spawn_attempted`, delete the volatile runtime directory as a reboot model; recovery reports `outcome_unknown` and `execution_unknown`; a missing or truncated journal gives the same result)
50. `only_an_intact_journal_without_spawn_attempted_retires_a_launch_as_shim_never_started` (the fenced late shim exits without a provider)

macOS runs of these tests are owner/platform validation (this machine is Linux).

## 10. Risks

- A paused provider (full buffer, no station) can hit its own provider-side timeouts. The board shows `detached`; the owner can stop it.
- Same-UID processes can attach (§5.1); accepted, as for ADR-006.
- A descendant that leaves the provider's session cannot be verified as ended without a reboot (§4.6). After a shim loss, the session therefore needs a reboot or an owner reconciliation before any resume; it never resumes on its own.
- The adapter checkpoint is a second persisted protocol format; a version change needs the N/N−1 rule (§5.3).

## 11. Open points

- `input_blocked { reason }` (§4.3) and `execution_unknown` (§4.6) are session conditions; 7-1 adds their PLAN §7.2 read-model mapping together with the Phase 6 `reconciliation_required` state.
- None blocks the review. Defaults (16 MiB buffer, 7-day exit retention, 4,096 write records, 256 pending writes, 4 MiB pending bytes, 1 MiB frames) are executor choices that the owner may change.

## 12. Review map (sa-f717e02d → revision 2)

| Finding | Sections |
| --- | --- |
| MAJOR 1 — adapter state and host interface | §1, §2 (host interface), §2.1, §3.2 step 2, §4.1, tests 36, 37 |
| MAJOR 2 — historical restore repeats commands | §3.3, §5.3 rollback, §7, test 22 |
| MAJOR 3 — eviction and tombstone identity | §4.3 (immutable identity, admission checks, eviction watermark, tombstone), tests 32, 35 |
| MAJOR 4 — shim death is not provider death | §2, §4.3, §4.5, §4.6, §7, tests 19, 40 |
| MAJOR 5 — line larger than the buffer | §4.1, §4.2, tests 38, 39 |
| MAJOR 6 — startup and exit crash boundaries | §3 (launch, launch lock), §3.2 step 1, §4.5, tests 43, 44, 45 |
| MAJOR 7 — approval timestamps | §2 (`read_at`), §6, test 15 |
| MAJOR 8 — incompatible shim and auto-continue | §3.2 step 5, §5.3, §6, test 21 |
| MAJOR 9 — stdin bounds and control path | §4.3 (bounds, takeover), §4.4, tests 41, 42 |
| MINOR — §3.4 reference | §3 now refers to §3.2 |
| MINOR — socket name collisions | §5.1 (full ULID, no shortening) |

## 13. Re-review map (sa-013bfabf → revision 3)

| Finding | Sections |
| --- | --- |
| F1 MAJOR — output-only checkpoints miss outbound adapter transitions | §2.1 (every transition recoverable, committed before outbound I/O, output transaction contents, no RPC id reuse), §4.1, tests 46, 47 |
| F2 MAJOR — activation-trial rollback discards acknowledged state | §3.3 (rollback-eligible trial performs no attach, ack or reconciliation; disposable copy; no snapshot restore after live state advances), §5.3, test 48 |
| F3 MAJOR — escaped `setsid()` descendant | §4.6 (session/group absence is necessary only; reboot is sufficient evidence; inspection errors fail closed), §6, §7, §10, test 40 extended |
| F4 MAJOR — missing runtime files do not prove no launch | §3 (persistent launch journal and lock, `spawn_attempted` before spawn), §3.2 step 1 (never started only from an intact journal), §7, tests 49, 50 |
| Note — status line DL id | Status line names DL-085; DL-080 records revision 2 |
