# P5b: monitor recordings, video artifacts and playback

- **Status:** proposed contract (2026-10-20, from the advisor answer `advisor-p5b-answer.md`). The architect accepts the recommendations OD-1 to OD-6 (decision O12-6; the owner may change any of them). Adversary DESIGN REVIEW required before implementation.
- **Parent:** [omarchy.md](./omarchy.md), especially the deferred recording proposals in §6.5A.
- **Prerequisites:** accepted [P5a](./omarchy-p5a.md); P5b-2 additionally requires accepted [P6](./omarchy-p6.md) and its completed implementation.
- **P5b-1:** owner recording from the board Desktop panel, recording lifecycle and stop, the `video` artifact kind, and byte-range playback.
- **P5b-2:** separately enabled coordinator monitor recording.
- **Authority:** this annex overrides the deferred recording proposals in the parent only after acceptance. P5a screenshot behavior and P6 screenshot permissions otherwise remain unchanged.

## 1. Recommendation and defects in option A

Use option A, subject to these corrections:

1. **A process group does not provide parent-death containment.** Reuse P5a’s dedicated spawning thread and `shim-exec` parent-death protection. Do not merely retain a Tokio child handle.
2. **A one-second policy check cannot establish deck liveness.** A half-open connection may remain connected after the deck has disappeared. Recordings require a renewable, expiring continuation lease.
3. **The ten-second authority is a start permit.** It cannot authorize a two-minute recording by itself, and its expiry must not terminate an otherwise authorized recording. Continuation and import require separate live checks.
4. **“Stop” needs distinct outcomes.** Duration completion and an explicit owner stop may save a valid clip. Policy loss, authority uncertainty, crashes and resource failures discard uncommitted output. A generic “save when SIGINT succeeds” would publish captures after authority loss.
5. **SIGINT is not an instantaneous pixel boundary.** The recorder can continue briefly while handling the signal. State the signal and escalation deadlines separately from durable completion.
6. **A file-size poll cannot enforce 256 MiB.** The recorder writes directly. Enforce the file-size ceiling in the child as well as validating the finished file.
7. **P5a’s importer copies the file.** Recording staging and the destination can coexist. Reserving only the output ceiling understates peak disk usage.
8. **`video → other` is insufficient compatibility behavior.** Older peers may also reject `capture.kind = "recording"`. Both the artifact kind and optional recording metadata need a negotiated projection.
9. **Range support must reach the station read.** Reading from offset zero and discarding earlier bytes, or filling the entire cache before answering a small range, defeats bounded seeking.
10. **P6 grants must not silently gain recording permission.** Existing screenshot grants retain their accepted meaning. Recording requires an explicit elevated owner opt-in.
11. **“Never video bytes to the agent” describes this tool interface.** It cannot establish confidentiality against processes running as the owner’s UID or revoke ordinary artifact access accepted under DL-230.

Option B would reduce the first implementation’s scope but duplicate the authority and lifecycle design later. Independent feature negotiation provides that release separation within option A.

Option C is outside this contract because `wf-recorder` is the required backend. No claim that every alternative necessarily requires privilege is needed.

## 2. Owner decisions

Every unresolved product or policy choice is listed here. References to an OD below mean the proposed rule depends on that decision.

| Decision | Proposed choice |
|---|---|
| **OD-1 — Scope and rollout** | Accept option A. Release P5b-1 independently; enable P5b-2 only after P6 and recording-specific proofs pass. |
| **OD-2 — Stop and crash semantics** | Save on duration completion or explicit owner stop only. Discard on safety, policy, authority, link or resource failure. Accept the bounded detection intervals in §7 and recovery-based physical cleanup. |
| **OD-3 — Resource and timing limits** | Accept §8 and §10, including the 512 MiB peak recording reservation required by the copying importer. |
| **OD-4 — Coordinator permission and budgets** | Require explicit recording opt-in under the existing `capture` class. Existing grants receive zero recording allowance. Accept §13’s counters and defaults. |
| **OD-5 — Recorded content** | Record one complete monitor, silently, including its composed visible content and the recorder’s normal cursor capture. No region, window, audio, cropping or resizing. Accept the dimension limits in §8. |
| **OD-6 — Browser and login lifetime** | Closing or reloading the board does not stop recording. Revocation or expiry of the initiating owner login stops and discards an uncommitted owner recording. Another current owner login may stop it. |

Existing binding decisions require no new approval:

- No per-capture prompt.
- Captures are ordinary artifacts, retained until deleted.
- Ordinary artifact readers may read captures.
- The wall does not play video.
- All settings, grants and approvals are available from the board.
- Agents run as the owner’s UID.

“No orphan or partial artifact” means:

- no indefinitely surviving recorder after loss of its supervising authority;
- no readable or published partial artifact;
- durable identification of every staging/import allocation;
- cleanup before station service resumes after a crash;
- preservation and eventual deck indexing of a committed station artifact.

It cannot mean instantaneous unlinking while the station is powered off, instantaneous detection of a network partition, or a hard completion bound for uninterruptible kernel I/O. **OD-2 requires acceptance of this operational meaning.**

## 3. Features and wire actions

### 3.1 Features

Add:

| Feature | Meaning |
|---|---|
| `desktop_recording.v1` | Owner recording action, recording status/progress, active recording discovery, stop, live policy checks and continuation leases. |
| `artifacts_video.v1` | Understands `video`, recording provenance and recording metadata. On the deck, also advertises the HTTP video/Range contract. |
| `capture_recording_mcp.v1` | P5b-2 coordinator recording, grant fields, tool schema and recording status projection. |

Owner recording requires the current target link to negotiate:

```text
desktop.v1
desktop_authority.v1
desktop_capture.v1
artifacts_delete.v1
artifacts_video.v1
desktop_recording.v1
```

P5b-2 adds P6’s complete caller/target feature requirements and `capture_recording_mcp.v1` on both relevant links.

Features describe implemented protocol behavior. Executable availability and permission remain separate.

### 3.2 Start action

Keep the existing route:

```http
POST /api/stations/{station}/desktop/act
X-FleetDeck-Capture-Id: <canonical ULID>
Content-Type: application/json

{"action":"screen_record","monitor":"DP-1","seconds":30}
```

The corresponding flat `desktop.act` parameters are:

```json
{
  "action": "screen_record",
  "monitor": "DP-1",
  "seconds": 30,
  "authority": "<DesktopAuthority object>"
}
```

Rules:

- `seconds` is required and is an integer from 1 through 120.
- Monitor validation follows P6’s monitor grammar and the actual current monitor inventory.
- Reject unknown fields.
- The capture ID is the authority action ID.
- Include `screen_record` in both closed action enums and both typed/serialized `target_monitor` implementations.
- The canonical action hash covers the action, monitor, seconds and target fingerprint. Coordinator actions additionally include the complete P6 coordinator context.
- Existing action hashes remain byte-for-byte unchanged.
- No output path, executable, codec, frame-rate, audio or arbitrary encoder arguments are accepted.

Owner authorization requires owner login, Admin scope, CSRF, deck `desktop_control`, target `allow_desktop`, target `allow_desktop_capture`, and the live monitor allowlist.

Enabling capture retains the existing elevated settings flow. Starting a recording requires no additional prompt.

### 3.3 Methods

Add these method-table entries:

| Method | Direction | Scope | Frame command ID | Feature |
|---|---|---|---|---|
| `desktop.capture_active` | deck → station | Admin | absent | `desktop_recording.v1` |
| `desktop.capture_stop` | deck → station | Admin | absent | `desktop_recording.v1` |
| `desktop.capture_policy_now` | deck → station | Read | absent | `desktop_recording.v1` |
| `desktop.capture_lease` | station → deck | Read | absent | `desktop_recording.v1` |

`desktop.capture_stop` is an idempotent operation control, explicitly excluded from generic mutation receipts and outboxes. Its durable identity is the capture operation it controls.

Continue using:

- `desktop.act` for start, with its existing Drive scope and authority requirement;
- `desktop.capture_status` for durable lookup;
- `artifacts.file` for bounded file reads with explicit offset and length.

The recording feature is additionally checked inside the relevant `desktop.act` and status handlers.

Update ADR-004’s method table and its agreement tests.

## 4. Results, discovery and owner stop

### 4.1 Start result

A successful recording returns the existing `DesktopActResult` with:

- `address: null`;
- `capture.capture_id`;
- `capture.published`, containing the committed artifact;
- the last valid desktop information, with `info_fresh:false` if the post-recording refresh failed.

No video bytes appear in the action response.

A station commit followed by failed deck indexing returns `202` and a pending capture state. It must not report recording failure or remove the station artifact.

### 4.2 Status

Keep:

```http
GET /api/stations/{station}/desktop/captures/{capture_id}
```

Keep P5a’s tagged states:

```text
pending
unknown
committed
failed
deleted
```

For recording operations, feature-aware responses add:

```ts
recording?: {
  phase: "preparing" | "recording" | "stopping" | "finalizing";
  requested_seconds: number;
  started_at: number | null;
  elapsed_ms: number | null;
};
```

These progress fields describe a current station answer. A stale deck observation must not be presented as proof that recording is still active.

Committed recording metadata contains:

```ts
{
  requested_seconds: number;
  duration_ms: number;
  width: number;
  height: number;
  stop_reason: "duration" | "owner_stop";
}
```

Unknown never authorizes a resend.

### 4.3 Active recording discovery

Add:

```http
GET /api/stations/{station}/desktop/recording
```

The link request is:

```text
desktop.capture_active
params: {}
```

Return:

```ts
type ActiveRecording =
  | { state: "idle" }
  | { state: "active"; capture_id: string; status: CaptureStatus };
```

This is owner/Admin only. It finds owner and coordinator recordings holding the station’s capture slot, including preparation and finalization.

An offline target produces an unreachable result, not `idle`. The board may show its last observation explicitly as stale.

### 4.4 Owner stop

Add:

```http
POST /api/stations/{station}/desktop/captures/{capture_id}/stop
Content-Type: application/json

{}
```

Requirements:

- current owner login;
- Admin scope;
- CSRF;
- a deck operation belonging to that stable target station;
- no elevation and no capture-enable requirement.

Turning capture off must not disable the stop control.

The deck commits `stop_requested` before sending:

```json
{
  "capture_id": "<ULID>",
  "reason": "owner_stop"
}
```

through `desktop.capture_stop`.

Internal callers may use the fixed discard reasons in §7. Public HTTP callers cannot select them.

The station derives the deck identity from the authenticated link. It accepts no PID, process group, monitor substitution or pathname.

Return current durable status:

- `200` when terminal;
- `202` when stopping, finalizing or awaiting target acknowledgement.

A repeated stop never affects a different capture. It cannot change a committed operation or restart anything.

Stopping before the recorder starts produces `recording_cancelled` with no artifact. Stopping a screenshot through this route returns `409 not_recording`.

Stop intent is a monotonic latch:

```text
none → save → discard
none → discard
```

A later safety failure overrides a graceful save request until the publication boundary. No response or reconnect can clear a discard latch.

## 5. Durable operation records

Reuse P5a’s tables:

- station `capture_ops`, introduced by V34;
- deck `desktop_captures`, introduced by V32.

Allocate new migration numbers against the implementation branch. Do not assume the next numbers while P6 is changing schemas.

### 5.1 Shared identity

Add `operation_kind = screenshot | recording`, defaulting existing rows to `screenshot`.

Persist for recording:

- requested seconds;
- allocated artifact ID;
- action hash;
- authenticated caller provenance;
- original link/desktop epoch;
- start and end timestamps;
- progress phase;
- stop disposition and fixed reason;
- reservation and cleanup accounting.

The station key remains `(authenticated deck fingerprint, capture_id)`. The deck key remains `(stable station ID, capture_id)`.

Same identity and hash returns recorded state without execution. A changed kind, monitor, seconds, target or coordinator context conflicts.

### 5.2 Deck states

Retain:

```text
prepared → attempted → committed | failed | unknown
```

- Commit `prepared` before dispatch.
- Commit `attempted` before calling the link.
- Never resend a prepared or attempted recording after process loss.
- Preserve P5a’s startup treatment of abandoned prepared records.
- `committed` may temporarily have `indexed = false`.
- Atomically commit artifact index, version/provenance and indexed completion.
- Store stop intent independently of completion state.
- A deck timeout must not overwrite a known committed result with failure.

### 5.3 Station states

Retain terminal states:

```text
running → committed | failed
```

Add a recording phase within `running`:

```text
preparing → recording → stopping → finalizing
```

The phase is progress, not permission to resume.

Before spawning:

1. Complete startup recovery.
2. Claim the shared station capture slot.
3. Reserve quota and disk allowance.
4. Persist the operation and artifact ID.
5. Prepare the retained output descriptor.
6. Establish continuation supervision.
7. Perform final authority and local checks.

The artifact, version, immutable provenance, recording metadata and `committed` operation result are one station transaction.

A failed operation retains its reservation until its physical cleanup finishes. A committed operation retains accounting for any remaining staging copy until that copy is removed.

Permanent compact operation identities survive cleanup and receipt pruning.

### 5.4 Recovery

Station startup:

1. Finish pending artifact deletions.
2. Resolve all interrupted recording operations.
3. Mark uncommitted recordings `failed` with `recording_interrupted`.
4. Remove their staging and uncommitted destination trees.
5. Finish staging cleanup for committed recordings.
6. Reconcile reservations.
7. Admit new captures and artifact reads only after required recovery succeeds.

Never finalize, import or resume an interrupted recording after restart, even if its staging file appears playable.

Never signal a PID or process group recovered from the database. Process ownership is established by the live supervisor, not a reusable numeric PID.

Deck reconciliation runs at startup, link-up and every 30 seconds while unresolved. It uses lookup only. A committed station result repairs indexing, including after owner-login or coordinator-grant loss.

P5a’s unresolved-result ageing may remain, but a terminal unknown result never permits redispatch.

Tombstones take precedence over every recovered result.

## 6. Continuation authority

### 6.1 Start permit

Preserve P3b’s ten-second single-use authority, epoch, clock and final-start checks.

The final check occurs immediately before recorder execution, after preparation and any disclosure notification. No unrelated asynchronous preparation follows it.

Expiry after successful start does not invalidate continuation. A continuation lease cannot start or restart a recorder.

### 6.2 Live policy query

`desktop.capture_policy_now` takes:

```json
{
  "capture_id": "<ULID>",
  "action_hash": "<hash>"
}
```

It answers only for that deck’s operation, using current target settings and a fresh, bounded desktop probe:

```ts
{
  valid: boolean;
  desktop_epoch: string;
  allow_desktop: boolean;
  allow_desktop_capture: boolean;
  monitor_allowed: boolean;
  monitor_present: boolean;
  unlocked: boolean;
}
```

Unknown lock state is not unlocked. Missing, malformed or timed-out policy answers fail closed.

The target rereads local settings after the desktop probe. It does not answer from a published deck projection.

### 6.3 Lease request

`desktop.capture_lease` takes:

```ts
{
  capture_id: string;
  action_hash: string;
  sequence: number;
  phase: "start" | "record" | "import";
}
```

`sequence` is a monotonically increasing internal counter for the current operation and link. The authenticated station is the target; it supplies no alternate target or caller identity.

The deck resolves the durable operation, verifies the hash, kind, current link and permitted phase, then checks:

- deck `desktop_control`;
- target enrollment and applicable link scopes/features;
- current target policy through `desktop.capture_policy_now`;
- the initiating owner login for owner recordings;
- all P6 grant, binding, generation, caller-policy, target-policy and fleet-control gates for coordinator recordings;
- durable stop intent and tombstones.

For coordinator recordings, use P6’s private authorization path and current caller-policy round trip. No bare grant ID authorizes continuation.

Repeat deck-local checks after awaited policy reads.

Return:

```ts
{
  sequence: number;
  decision: "continue" | "save" | "discard";
  reason: string | null; // fixed enum only
}
```

- `continue`: all current gates permit the phase.
- `save`: explicit owner stop, with other applicable gates still valid.
- `discard`: policy loss, authority loss/uncertainty, deletion or another safety stop.

### 6.4 Lease timing

Under **OD-2/OD-3**:

- Obtain an initial successful lease before spawning.
- Attempt renewal every one second.
- Allow one renewal in flight per recording.
- A lease request has a five-second total deadline, including policy reads.
- A successful lease expires six seconds after the station’s local request-start instant, not six seconds after response arrival.
- Reject late, wrong-sequence or wrong-link answers.
- Keep an independent monotonic expiry timer; an awaited RPC cannot postpone it.
- A stopped operation never becomes running because of a late successful response.

Use a suspend-aware elapsed-time source, or detect suspend and discard on resume before accepting continuation. Do not let a suspended station preserve an apparently fresh lease.

Lease checks have bounded dedicated capacity. They do not consume agent metadata-query budgets or wait behind video transfers.

Before import, perform a fresh import-phase check and then reread local gates. The original start token is not reused.

## 7. Process lifecycle and stop triggers

### 7.1 Owned process

Add a recording runner separate from the text runner and PNG stdout runner.

Reuse P5a’s dedicated OS spawning thread and hidden `shim-exec` facilities:

- establish an owned session/process group;
- register parent-death `SIGKILL`;
- perform the parent check after registration;
- keep the spawning thread alive until child reaping;
- retain the child/process ownership handle;
- supervise cancellation independently of an HTTP waiter.

Linux parent-death signaling follows the spawning thread, and is not inherited by forked children. Therefore acceptance must verify the supported recorder does not leave helper processes. [Linux parent-death signal documentation](https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html)

Do not use global recorder kills or signal unrelated `wf-recorder` instances.

The supervisor’s stop timers must remain runnable while policy queries, filesystem work or database writes are blocked. Signal first when necessary; durable bookkeeping follows without weakening the discard latch.

### 7.2 Graceful stop

Only these triggers request save:

- requested duration reached;
- explicit owner stop.

Send `SIGINT` to the owned group. Allow three seconds for exit and file finalization. Then send group `SIGKILL` if any owned recorder process remains.

A graceful result is eligible for import only when:

- the recorder exited successfully;
- no escalation was required;
- structural/media validation passes;
- no discard trigger intervened;
- the final import checks pass.

Do not treat signal delivery as proof that pixels stopped immediately.

### 7.3 Discard stop

For a discard trigger, send `SIGKILL` without the three-second grace. Reap the child and finish/cancel output-draining tasks before removing its files.

A valid-looking MP4 does not override a discard decision.

### 7.4 Trigger table

The following bounds concern detection and signal issuance on a responsive station. Reaping and durable cleanup can take longer during kernel or storage failure.

| Trigger | Detection bound | Action |
|---|---:|---|
| Duration | Monotonic duration timer | SIGINT; SIGKILL three seconds later if needed |
| Owner stop received | Immediate control dispatch | SIGINT; same grace |
| Local `allow_desktop` or capture permission off; monitor allowlist excludes target | At most one second | Kill and discard |
| Lock, unknown lock state, monitor removal or relevant monitor geometry/transform change | Event-driven where available; otherwise one-second probe schedule plus one-second probe deadline | Kill and discard within two seconds |
| Desktop/compositor epoch changes | Event or local check, at most one second after observation | Kill and discard |
| Explicit target-link closure | Immediate link notification | Kill and discard |
| Silent link partition, deck crash or unresponsive deck | Current lease expiry, at most six seconds after the last valid request began | Kill and discard |
| Deck switch off or initiating owner login revoked/expired | Push stop when observed; lease expiry is the fallback bound | Kill and discard |
| Coordinator revoke, expiry, binding/generation change, caller-policy loss, caller disconnect or applicable fleet-control loss | Push stop when observed; lease expiry is the fallback bound | Kill and discard |
| Invalid/denied lease response | On response | Kill and discard, except valid owner-stop `save` |
| Lease timeout | Independent six-second expiry | Kill and discard |
| Output ceiling or write failure | Kernel enforcement/child failure; size check at most one second | Kill and discard |
| Free-space reserve lost | At most one-second resource check | Kill and discard |
| Recorder exits unexpectedly, even with exit code zero | Child exit notification | Discard |
| Station orderly shutdown or operation-supervisor cancellation | Immediate local notification | Kill and discard |
| Station crash/spawning-thread death | Kernel parent-death protection | Recorder killed; startup cleanup |
| Artifact tombstone | Deletion notification and final transaction guard | Kill/discard if uncommitted; ordinary deletion if committed |
| Total operation deadline | Deadline timer | Kill/discard unless publication is already underway |

A monitor with the same name after hotplug or compositor restart does not resume the old recording.

Failure to obtain fresh lock/resource information is a safety failure, not permission to retain an old answer.

The owner stop route records intent before its RPC. An online attempt has a five-second RPC deadline; if delivery fails, the next valid lease answer carries stop intent. If communication has failed entirely, lease expiry causes discard instead of saving.

### 7.5 Commit boundary

Continue checking policy through recording, graceful shutdown and finalization.

Immediately before import:

1. Verify there is no discard latch.
2. Obtain fresh remote import authority.
3. Refresh lock/monitor state and local settings.
4. Begin the bounded import.

A failure observed during copying cancels before publication. The final publication transaction checks local operation disposition and tombstones.

As in P5a/P6, an already-started atomic filesystem/DB publication may finish after a concurrent revocation. Retain its durable result. Do not promise instantaneous revocation across the remote-check/commit interval.

Deletion is stronger: both station and deck publication transactions enforce their permanent tombstones.

## 8. Limits, reservations and quota

All proposed numeric choices in this section require **OD-3**. Content/dimension choices additionally require **OD-5**.

| Resource | Limit |
|---|---:|
| Requested duration | Integer 1–120 seconds |
| Video samples | At most 30 frames/second |
| Output format | H.264 in MP4; no audio |
| Output bytes | At most 268,435,456 |
| Monitor dimensions | Each dimension 2–8192 and even |
| Pixel product | At most 16,777,216 |
| Concurrent captures per station | One shared slot for screenshots and recordings |
| Station capture quota | 4,294,967,296 bytes |
| Free-space reserve | 2,147,483,648 bytes |
| Recording peak reservation | 536,870,912 bytes |
| Import/hash copy buffer | At most 65,536 bytes |
| Station artifact chunk | At most 716,800 bytes |
| Deck capture cache | Existing 268,435,456 logical bytes |
| Per-version cache eligibility | Existing 67,108,864-byte limit |

Reject an unsupported monitor size; do not silently crop or resize.

The peak recording reservation covers a maximum staging file plus the importer’s maximum destination copy. Before admission, require:

```text
available bytes >= 2 GiB + 512 MiB
accounted capture usage + 512 MiB <= 4 GiB
```

Account committed versions, staging/import allocations, unreaped garbage and outstanding reservations without counting the same reserved allocation twice.

At commit, atomically replace the import allowance with actual committed bytes and retain allowance for the staging copy. Release the remainder only after physical cleanup succeeds.

Do not evict or delete artifacts automatically to admit a recording.

Before each station-controlled import write, enforce the reserve as in P5a. During direct recorder writes, sample free space at least once per second. This is not a filesystem reservation and cannot prevent another same-UID process consuming space between checks.

Set child `RLIMIT_FSIZE` soft and hard limits to 268,435,456 bytes before recorder execution. This prevents file extension beyond the ceiling even between polls. A limit-triggered termination or write error fails the operation. [Linux resource-limit documentation](https://man7.org/linux/man-pages/man2/getrlimit.2.html)

A complete file exactly at the ceiling is acceptable only if it passes every other check.

## 9. Recorder, files and import

### 9.1 Recorder interface

`wf-recorder` is the only backend.

Use structured arguments equivalent to:

```text
wf-recorder
  -o <monitor>
  -r 30
  -D
  -c libx264
  -x yuv420p
  -b 0
  -p preset=veryfast
  -p crf=23
  -F fps=30,trim=duration=<seconds>
  -m mp4
  -y
  -f /proc/self/fd/3
```

The filter value is generated from the validated integer, never supplied by the caller.

The duration filter bounds media samples; the independent supervisor timer bounds the recorder lifecycle. Startup delay can make the saved clip shorter than requested.

Use `wf-recorder` 0.5.0 as the initial acceptance baseline. Test newer installed releases before adding them to the supported release matrix; do not claim compatibility from a version string alone.

Upstream provides the selected output, muxer, frame-rate, filter, codec, pixel-format and overwrite options. It opens the named output through libavformat; it does not provide P5a’s PNG stdout interface. The proposed retained-FD arrangement therefore requires a real acceptance test. [wf-recorder 0.5.0 command implementation](https://github.com/ammen99/wf-recorder/blob/v0.5.0/src/main.cpp), [output implementation](https://github.com/ammen99/wf-recorder/blob/v0.5.0/src/frame-writer.cpp)

Do not invent a muxer-option flag or assume a normal MP4 can be written to a non-seekable pipe.

### 9.2 Descriptor arrangement

Create:

```text
<station data>/captures/                         0700
<station data>/captures/<capture_id>/            0700
<station data>/captures/<capture_id>/capture.mp4 0600
```

Use P5a’s directory-relative, no-follow traversal and:

```text
O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC
```

Require effective-UID ownership, regular-file type and link count one.

Keep the parent descriptor open. In the child launch shim, duplicate that exact file onto FD 3 and deliberately inherit only that descriptor and the required standard streams.

The fixed `/proc/self/fd/3` argument lets the recorder open the retained inode as a seekable output. Never pass the original staging pathname to the recorder.

Use:

- existing executable-resolution rules;
- cleared environment plus the desktop `ChildEnv`;
- null stdin;
- drained, bounded stdout/stderr;
- at most 16 KiB retained stderr, then drain/discard;
- no output contents or stderr in responses/audit.

No fallback to the current directory, Pictures, a portal or another recorder is permitted.

If the retained-FD path or required recorder capability is unavailable, fail closed.

### 9.3 Finished-file validation

After successful graceful exit and reaping:

1. Check descriptor identity, owner, mode, type, link count and size.
2. Parse the MP4 from the same retained descriptor.
3. Require complete bounded boxes, compatible MP4 branding and complete movie/sample tables.
4. Require exactly one H.264 video track and no audio, subtitle or other media tracks.
5. Require self-contained data references and sample extents inside the file’s media data.
6. Validate dimensions, positive timebase, sample counts/timing, media duration and the frame-rate ceiling.
7. Reject fragmented or truncated structures outside the deliberately supported recorder profile.
8. Rewind and calculate SHA-256 with bounded buffers.
9. Repeat descriptor identity/size checks.
10. Import from that descriptor.

Bound parser depth to 16, box count to 100,000 and video sample count to `seconds × 30`. Use checked arithmetic throughout. Do not load `mdat` or the whole file into memory.

Structural validation does not claim complete H.264 decoding. Browser playback and real-recorder fixtures form part of acceptance. No `ffprobe`, transcoder or decoder dependency is added.

### 9.4 Import

Generalize P5a’s importer by trusted operation kind:

```text
screenshot → capture.png / image
recording  → capture.mp4 / video
```

The caller cannot select either filename or kind independently.

Preserve:

- retained-descriptor copying;
- byte/reserve checks;
- exclusive no-follow destination creation;
- per-artifact publication/deletion lock;
- permanent tombstone checks;
- complete cleanup ownership.

Destination:

```text
<station data>/artifacts/<artifact_id>/1/capture.mp4
```

Before the station commit, fsync the destination file and every newly created directory through the first existing ancestor, exactly as P5a requires.

A failed fsync cannot produce `committed`.

The imported copy’s size/hash must match the validated source. No path reopen may substitute a different staging file.

## 10. Deadlines

These are proposed under **OD-3**. Each is a total budget from entry to the named stage.

| Stage | Deadline |
|---|---:|
| Start authority | Existing ten seconds |
| Initial local context/monitor/lock admission | Five seconds |
| Individual local watchdog probe | One second |
| Continuation/import lease request | Five seconds |
| Continuation lease validity | Six seconds from local request start |
| Graceful recorder shutdown | Three seconds |
| Station recording operation | `seconds + 30` seconds from intake |
| Deck wait for station action | `seconds + 35` seconds |
| Deck HTTP handler, including indexing | `seconds + 40` seconds |
| Board start request, including response body | `seconds + 45` seconds |
| Online stop RPC | Five seconds |
| Deck stop handler | Ten seconds |
| Board stop request | Fifteen seconds |
| Station/deck/board status | Existing 5/10/15 seconds |
| Video station chunk request | Twenty seconds |
| Video response preparation | Twenty-five seconds |
| Video HTTP response lifetime, including backpressure | 180 seconds |
| Board video metadata-load timer | Thirty seconds |

The video response lifetime replaces P5a’s screenshot-file total for video only. Do not apply a 60-second screenshot fetch lifetime to a two-minute player without an explicit video rule.

The operation deadline stops new work and terminates an active recorder. An already-started fsync or publication transaction may finish later. Its owned task must record the actual outcome.

Dropping an HTTP or MCP waiter does not abandon supervision, cleanup or result bookkeeping.

No start call enters an automatic mutation outbox or transport retry loop.

## 11. Artifact and Range contract

### 11.1 Artifact identity and provenance

Each recording creates one artifact, version 1:

| Field | Value |
|---|---|
| Station | Captured station’s stable deck ID |
| Session | `null` |
| Kind | `video` |
| File | `capture.mp4` |
| Owner author | `owner` |
| Coordinator author | P6-derived coordinator author |
| Title | `Desktop recording · <station name>/<monitor>`, existing 200-character rule |
| Repository association | None |

Extend immutable capture provenance with `kind:"recording"`.

Retain P5a/P6’s capture ID, monitor, actual start time and authenticated caller fields. Store recording metadata from §4.2 in the version.

Public artifact publication cannot supply or forge capture provenance.

The `video` kind covers single-file MP4 artifacts. Multi-file artifacts retain existing classification behavior. Ordinary MP4 publication does not gain recording provenance or recording-tool eligibility.

### 11.2 Routes and authorization

Implement GET and HEAD on the existing raw route:

```text
/api/artifacts/{station}/{id}/{version}/raw/{path}
```

Apply the same byte-serving implementation to existing public share/view file routes. Do not introduce a video-specific bearer URL.

Before returning metadata, length, validators or bytes:

1. Authenticate using the route’s existing mechanism.
2. Resolve the permitted artifact/version/file.
3. Check tombstones.
4. Validate the requested range.

Unknown, deleted and inaccessible identities retain the route’s existing indistinguishable failure behavior.

Recording-enable gates govern capture and coordinator delivery. They do not revoke ordinary reading of an already saved artifact.

### 11.3 Range behavior

For the selected single MP4 file of length `N`, support one byte range:

```text
bytes=start-end
bytes=start-
bytes=-suffix_length
```

Use checked unsigned parsing. Limit the Range field to 256 bytes.

| Request | Result |
|---|---|
| GET without applicable Range | `200`, complete representation |
| Satisfiable single byte range | `206`, selected bytes |
| Valid but unsatisfiable byte range | `416`, `Content-Range: bytes */N` |
| Malformed numeric/single-range syntax | `400 invalid_range` |
| Valid multiple ranges | Ignore Range and serve `200`; no multipart implementation |
| Unknown range unit | Ignore Range and serve `200` |
| HEAD | Ignore Range; `200` full-representation headers, no body |

Clamp an explicit end beyond EOF. A zero suffix or start at/after EOF is unsatisfiable. Reject arithmetic overflow.

Successful GET responses include:

```text
Accept-Ranges: bytes
Content-Type: video/mp4
Content-Length: <actual response byte count>
X-Content-Type-Options: nosniff
Cache-Control: no-store
Referrer-Policy: no-referrer
```

For `206`, also include:

```text
Content-Range: bytes <start>-<end>/<N>
```

Disable compression/transformation on these representations.

Use a stable strong ETag derived from immutable version/file identity and verified metadata. An `If-Range` strong match permits the range; other `If-Range` values select the complete `200` representation. Evaluate applicable ordinary HTTP preconditions before Range.

These response semantics follow HTTP’s range and conditional-request model. [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html#name-range), [RFC 7233](https://www.rfc-editor.org/rfc/rfc7233.html)

### 11.4 Bounded station reads

For an uncached interval `[start,end]`:

- begin at `start`;
- request `length = min(716800, remaining)`;
- keep at most one station chunk outstanding per body;
- emit no bytes outside the selected interval;
- never fetch preceding bytes to discard them;
- never fill the whole cache merely to answer a range.

Validate every station answer:

- expected path, offset and full-file size;
- reported length equals decoded length;
- length does not exceed the requested length;
- EOF agrees with full-file position;
- no premature empty/short response;
- valid bounded base64.

Reject malformed responses before exposing their bytes.

If failure occurs before headers, return the fixed HTTP error. After headers, terminate the body with an error; do not append JSON to MP4 bytes.

### 11.5 Cache and deletion

Preserve the existing 64 MiB per-version eligibility and 256 MiB capture-cache budget.

- A range cache miss streams only the requested interval.
- Existing complete cache entries may serve ranges.
- Slice cached storage before materializing response bytes; do not load an entire large blob for a small seek.
- Do not cache arbitrary partial ranges as complete files.
- Above the cache eligibility limit, stream from the station.
- An uncached offline video is unavailable, even if its metadata remains indexed.

Before each station read and immediately before every emitted chunk:

- check deck tombstones;
- recheck the route’s current read authority, including share/view expiry or revocation where applicable.

The station also checks its tombstone for each file read. A deletion after headers cuts the body. Already delivered bytes cannot be recalled.

Cache, metadata, reconciliation and event writes retain P5a’s transaction-local tombstone guards.

## 12. Board viewer and Desktop panel

Under **OD-5/OD-6**:

- Each eligible monitor has **Record**, a required duration control defaulting to 30 seconds, and the existing Screenshot control.
- Show capability, tool availability and permission failures separately.
- While the station slot is occupied, disable new screenshot/recording starts.
- Show **Preparing recording**, **Recording**, **Stopping recording** or **Saving recording** from current station status.
- Offer **Stop recording** independently of start gates.
- Discover the active operation on opening/reloading the Desktop panel.
- Retain the capture ID across reloads.
- Poll every two seconds while awaiting completion, through the requested duration plus the completion allowance.
- After that, offer **Check result**. Never automatically repeat the POST.

Permanent disclosure:

> Records the whole selected monitor, including other applications, notifications and private information visible there. No audio is recorded. The recording is stored as an ordinary artifact until deleted.

The artifact viewer uses:

```html
<video controls preload="metadata"></video>
```

with a same-origin, version-pinned raw URL.

- No autoplay, loop, thumbnail extraction or video-to-image conversion.
- Add `media-src 'self'` to the board document CSP.
- Preserve existing HTML/SVG sandbox policies.
- Do not fetch the video into a Blob or base64 string.
- Native controls may seek through Range requests.
- A playback retry reloads bytes only.
- Expired view links may be refreshed through existing authorized flows.
- Unsupported browser decoding offers the ordinary download action.

Viewer provenance:

> Desktop recording · `<station>/<monitor>` · `<time>`

Show requested and saved duration where they differ.

The wall shows exactly:

> Open this artifact on your own device

Video remains non-displayable on the wall regardless of its single-file shape. The wall route must not start a video fetch.

## 13. P5b-2: coordinator recording

This section requires **OD-1 and OD-4**.

### 13.1 Tool and class

Add exactly:

```text
record_monitor
```

It belongs to the existing `capture` class. It does not imply `desktop`, `inspect`, `drive` or `input`.

No `record_demo`, `desktop_record` or other alias is accepted.

Schema:

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": ["request_id", "monitor", "seconds"],
  "properties": {
    "request_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "station": {
      "type": "string",
      "minLength": 1,
      "maxLength": 128,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "monitor": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9._-]+$"
    },
    "seconds": {
      "type": "integer",
      "minimum": 1,
      "maximum": 120
    }
  }
}
```

Omitted station means the authenticated caller station. Explicit remote targeting follows P6’s grant-station rules.

Extend P6’s dedicated `coordinator.desktop_call` union. Keep its acceptance receipt, normalized request hash, private relay permit, coordinator-aware authority and outbox exclusion.

### 13.2 Explicit recording allowance

Add durable grant fields:

```text
recording_cap
recording_used
recording_second_cap
recording_second_used
```

Existing grants and grants without explicit recording opt-in have both caps zero.

For a newly opted-in grant:

| Field | Default | Allowed range |
|---|---:|---:|
| `recording_cap` | 5 | 0–5000 |
| `recording_second_cap` | 600 | 0–600000 |

The board labels this permission **Allow monitor recordings** and requires the existing elevated owner grant flow.

At acceptance, atomically charge:

- one desktop action;
- one capture;
- one recording;
- the full requested seconds.

Generalize `capture_cap` accounting to screenshots plus explicitly permitted recordings. Existing screenshot-only grants retain their behavior because recording allowance is zero.

No refund follows early stop, dispatch failure, cancellation, deletion, timeout or ambiguous outcome. Renewal and restart do not reset counters.

Apply P6’s capture rate of two new captures per rolling minute to screenshots and recordings together, and retain all other P6 rate/concurrency limits. Recording holds its mutation slot until terminal bookkeeping completes.

Internal lease renewals do not consume tool-query budgets.

### 13.3 Authority and disclosure

Use all P6 live gates, including caller and target fleet-control policy, grant expiry, binding generation and target capture policy.

Before recorder execution, send the fixed P6-style notification:

> Fleet Deck is recording `<monitor>` for `<callsign>` for up to `<seconds>` seconds.

Substitutions come from validated records and the validated integer. There is no agent-supplied message or action. Notification failure refuses the recording. DND is not bypassed.

The notification does not replace board authorization or the continuation lease.

### 13.4 Agent result

Return the P6 caller-owned capture status, extended for a recording. A committed result contains only:

- request/capture identity;
- stable selected target;
- immutable artifact reference;
- monitor;
- byte count and hash;
- start time;
- requested and saved duration;
- fixed stop reason.

Never return:

- MP4 bytes or base64;
- image/frame/thumbnail blocks;
- video-derived screenshots;
- raw `DesktopInfo`;
- share/view tokens or capability URLs;
- filesystem paths, environment, stderr or encoder output.

`read_capture_image` continues to require a screenshot PNG. It cannot read a recording, even if that artifact is small.

There is no new MCP video reader or coordinator stop tool in P5b-2. The owner stop control and all automatic safety stops apply to coordinator recordings.

`capture_status` remains scoped to the same grant and caller, keyed by the P6 request digest. It does not accept a target or arbitrary capture ID.

Current authority is required to deliver status or artifact references. Internal reconciliation may index a completed artifact after grant loss without delivering it to that caller.

The provider receives tool text/reference metadata. This contract does not claim isolation from other same-UID access or alter DL-230’s ordinary artifact audience.

### 13.5 Catalog and deadlines

List `record_monitor` at provider-process start only when the recording-enabled grant and required feature negotiation permit it. Retain P6’s fixed-catalog lifecycle behavior.

Later downgrade or policy loss makes calls fail; it does not silently mutate the catalog.

Use §10’s duration-dependent deadlines. The caller MCP response deadline is `seconds + 45` seconds. Bypass generic coordinator deadlines that would cut off a valid recording.

Existing P6 screenshot-only grants do not acquire a recording tool through renewal.

## 14. Version behavior

1. P5a-only stations continue owner screenshots unchanged.
2. A station missing `desktop_recording.v1` never receives `screen_record`, recording control methods or lease traffic.
3. A missing recorder is an availability error, not a different backend.
4. Negotiated recording support is checked at admission, dispatch and continuation. Losing it aborts an active uncommitted recording.
5. An old deck does not enable the new board recording controls. Missing `/api/features` retains the existing old-deck behavior.
6. For peers without `artifacts_video.v1`, serialize video artifacts as `kind:"other"`.
7. Also omit optional recording provenance/metadata that those peers cannot parse. Do not send a new closed-enum value inside an otherwise downgraded artifact.
8. Apply that projection to lists, gets, publication results, events, snapshots and nested capture results where applicable.
9. Projection never rewrites stored kind, provenance or hashes.
10. A modern board requests video-aware JSON using:
   ```text
   X-FleetDeck-Features: artifacts_video.v1
   ```
   Without that declaration, authenticated artifact JSON uses the legacy projection. The feature header grants no authority.
11. Existing public share/view pages are generated by the current deck and retain their existing access mechanism.
12. Recording-specific status extensions are emitted only through recording-aware paths.
13. P5b-2 requires `capture_recording_mcp.v1`; recording grant fields are never sent to strict older peers.
14. Unknown coordinator context or recording provenance fails closed. Never strip coordinator authority to make a start compatible.
15. Generic older artifact readers may still download the MP4 through ordinary artifact routes when authorized; they receive no promise of an embedded player.

## 15. Audit

Reuse P5a/P6 capture auditing and correlate by deck-generated capture ID.

| Action | Required rows |
|---|---|
| `desktop.capture` | Accepted/prepared attempt, actual start when established, and terminal result or refusal |
| `desktop.capture_stop` | Owner stop intent and resolved outcome |
| `desktop.capture_status` | Existing lookup outcome |
| Coordinator grant create/update | Explicit recording opt-in and configured caps |
| Coordinator recording acceptance | Atomically charged counters and requested seconds |
| `artifacts.delete` | Existing ordinary deletion audit |

Record, when established:

```text
actor
stable target station ID
capture ID and operation kind
validated monitor
requested seconds
actual start/end and saved duration
fixed stop reason and result code
artifact reference
coordinator grant ID, binding generation and caller IDs
charged counters
```

A dispatch attempt is not evidence that pixels started. A missing terminal response is not evidence that no artifact exists.

Deduplicate durable start/terminal audit transitions during recovery. A later reconciliation may resolve an earlier unknown outcome.

Record automatic stop once when latched; do not create one audit row per lease or policy poll.

Do not record authority objects, login credentials, raw agent request IDs, share tokens, paths, arbitrary text, video bytes, thumbnails or stderr.

Ordinary video reads retain ordinary artifact read-audit policy. Do not add per-chunk audit rows.

## 16. Errors and exact texts

Preserve existing desktop/capture errors outside recording-specific paths. Extend the capture HTTP allowlist so these codes survive relay mapping.

| HTTP | Code | Exact text |
|---:|---|---|
| 400 | `invalid_desktop_action` | `The desktop action is not valid.` |
| 400 | `recording_duration` | `Recording duration must be a whole number from 1 to 120 seconds.` |
| 403 | `desktop_forbidden` | `The station refused the desktop action.` |
| 403 | `desktop_control_off` | `Desktop control is off on this deck.` |
| 404 | `desktop_not_found` | `The target of the desktop action was not found.` |
| 404 | `unknown_artifact` | `No such artifact.` |
| 409 | `busy` | `A capture is already running.` |
| 409 | `capture_id_conflict` | `This capture ID was used for another request.` |
| 409 | `screen_locked` | `The screen is locked or its lock state is unknown.` |
| 409 | `not_recording` | `This capture is not a recording.` |
| 409 | `capture_quota` | `This capture would exceed the station's 4 GiB capture limit. Delete some captures first.` |
| 413 | `recording_too_large` | `The recording exceeds 256 MiB.` |
| 422 | `recording_dimensions` | `The monitor exceeds the recording size limit or has unsupported dimensions.` |
| 507 | `disk_full` | `The station has too little free space for a capture.` |
| 409 | `recording_cancelled` | `The recording was stopped before it started.` |
| 409 | `recording_interrupted` | `The recording was interrupted before it was saved.` |
| 502 | `recording_failed` | `The recording tool failed.` |
| 502 | `recording_invalid` | `The recording file could not be verified.` |
| 502 | `recording_publish_failed` | `The recording could not be saved as an artifact.` |
| 501 | `desktop_unsupported` | `The recording tool wf-recorder is not installed.` |
| 501 | `desktop_unsupported` | `This station does not support monitor recordings.` |
| 409 | `station_unreachable` | `The station is not connected.` |
| 504 | `desktop_timeout` | `The station did not answer in time; the action may have run.` |
| 400 | `invalid_range` | `The byte range is not valid.` |
| 416 | `range_not_satisfiable` | `The requested byte range is not satisfiable.` |
| 502 | `station_error` | `The station returned invalid artifact data.` |
| 504 | `timeout` | `The video data did not finish loading in time.` |

Use the missing-tool text only when the executable is missing. Other unsupported recording capabilities use the second fixed text.

A `416` includes `Content-Range: bytes */N`; its JSON error body has its own correct Content-Type and Content-Length.

Automatic stops use fixed terminal codes:

- policy/grant loss: P6 `authority_lost` for coordinator delivery; owner status uses `recording_interrupted`;
- missing/uncertain authority, link loss, shutdown or crash: `recording_interrupted`;
- lock and missing target: existing fixed codes where definitively established;
- resource and validation failures: the corresponding table code.

Store the fixed stop reason separately for owner diagnostics/audit.

P5b-2 adds:

| Code | Exact text |
|---|---|
| `recording_not_allowed` | `This grant does not allow monitor recordings.` |
| `recording_cap_reached` | `This grant has used its recording limit.` |
| `recording_seconds_reached` | `This grant has used its recording time limit.` |

For recording admission, exhausted shared capture allowance uses `capture_cap_reached` with:

> This grant has used its capture limit.

Existing screenshot error text remains unchanged.

Normal status text:

- Unknown: **“The recording result is unknown. Check the artifact list before starting another recording.”**
- Station committed, indexing pending: **“The recording was saved on the station. The artifact list is still updating.”**
- Safety interruption: **“Recording stopped. No recording artifact was saved.”**
- Playback failure: **“The video could not be loaded. Try again or download the artifact.”**

Only show “No recording artifact was saved” after a definitive station failure. An unknown result does not support that statement.

## 17. Tests: the four proofs

### 17.1 Behavior

1. Owner start schema, required capture ID, duration boundaries, monitor grammar and unknown-field rejection.
2. Canonical hash fixtures covering kind, monitor, duration, target and coordinator context; unchanged legacy fixtures.
3. Recording and screenshot compete for one station slot; distinct simultaneous requests reject immediately.
4. Duration completion and owner stop produce one valid single-file artifact.
5. Stop before spawn produces no recorder and no artifact.
6. Stop from another owner tab/login and stop of a coordinator recording.
7. Fresh-tab active discovery, reload recovery and progress freshness.
8. Exact recorder argv, environment allowlist, null stdin and inherited descriptor set.
9. Real baseline recorder writes and finalizes through `/proc/self/fd/3`; no staging-path reopen.
10. Silent/static monitor produces valid bounded output; selected-monitor isolation, rotation and dimensions.
11. Correct provenance, actual size/hash, title, version and stable station identity.
12. Station commit and deck indexing atomicity; failed desktop refresh does not change success.
13. Range closed/open/suffix forms, EOF clamping, empty-file behavior, HEAD, ETag/If-Range, malformed and multiple ranges.
14. Cached and station-backed ranges return exactly the requested bytes and begin station reads at the requested offset.
15. Native board playback, seek, retry, download and no autoplay.
16. Wall card without a video fetch.
17. P5b-2 schema/catalog, remote targeting, fixed disclosure, result projection and budget charging.

### 17.2 Failure

18. Every stop trigger in §7 with controlled clocks, including its detection/signal bound.
19. Half-open link with no disconnect event; deck crash; delayed, replayed and wrong-sequence lease answers.
20. Block policy/DB/filesystem work while the independent lease watchdog expires.
21. Revoke every applicable setting/grant/binding at preparation, spawn, recording, SIGINT grace, validation, copying and publication boundaries.
22. Ten-second start authority expires during preparation; no recorder starts. Its normal later expiry does not stop a leased recording.
23. Station process death, spawning-thread death, normal cancellation and shutdown leave no surviving supported recorder.
24. SIGINT stalls, recorder ignores SIGINT, full stderr/stdout pipes, unexpected zero/nonzero exit and forced kill.
25. Output exact ceiling, attempted ceiling+1, quota admission, double-copy reservation, reserve loss and ENOSPC.
26. Symlink/hard-link refusal, changed inode, wrong owner/mode, wrong FD and malformed/truncated MP4.
27. Parser overflow/depth/count limits, invalid sample extents, external references, extra tracks and excessive sample duration/rate.
28. Crash before/after operation claim, spawn, stop intent, import copy, every fsync, station commit, cleanup and deck indexing.
29. Directory-durability fault hook proving a committed artifact cannot lose unsynced directory entries.
30. Restart removes uncommitted files, preserves committed files and retains accounting until cleanup.
31. Lost start/stop responses, browser disconnect, late commit and every deadline; no start resend or abandoned bookkeeping.
32. Deletion races with copy, commit, reconciliation, cache fill, metadata refresh, share/view creation and each emitted chunk.
33. Invalid station chunk path, size, offset, length, base64, EOF and premature short response; no invalid bytes emitted.
34. P5b-2 concurrent final-budget-slot races, duplicates charged once, no refunds and no reset after renewal/restart.

### 17.3 Exposure

35. Distinct sentinels for owner, caller, selected target, unrelated station and another grant; only authorized metadata appears.
36. Identical artifact IDs on different stations; every lookup/cache key includes stable station identity.
37. Guessed capture/request IDs do not expose foreign coordinator operations.
38. MP4/base64/frame/thumbnail sentinels never appear in MCP results, logs or audit.
39. `read_capture_image` rejects recording artifacts; ordinary publication cannot forge recording provenance.
40. Share/view expiry, owner-session revocation and deletion between chunks cut delivery.
41. Range length/validator errors do not reveal inaccessible artifact existence.
42. No arbitrary executable, argv, path, codec, URL, environment or PID input.
43. State the composed-pixel and same-UID limitations accurately; do not claim confidentiality the process model cannot provide.

### 17.4 Version

44. Old/new board, deck, caller and target matrix for every feature combination.
45. P5a-only target remains usable for screenshots; recording is refused before dispatch.
46. Existing P6 grant remains screenshot-only; opt-in requires an elevated owner flow.
47. Old peers receive `other` and parseable optional metadata projection across every response/event/snapshot path.
48. Downgrade or reconnect during recording aborts without restarting.
49. Coordinator context is never stripped; unknown recording context fails closed.
50. Legacy screenshot rows migrate as screenshot; old hashes, grants and status responses retain their fixtures.
51. Required MCP protocol versions encode the reference-only recording result within existing frame limits.
52. Opt-in Omarchy/Hyprland live smoke: owner recording, stop, deck/link loss, lock, seek, deletion, coordinator opt-in/revoke and unsupported recorder capability.

Run only targeted crate/web checks through the required heavy-command queue. The implementation coordinator runs `make check`.

## 18. Implementation and acceptance

No new third-party dependency, recorder backend, privileged helper or station-machine setup step.

Reuse existing Rust, SQLite, Tokio, rustix, hashing, HTTP and artifact facilities. Split modules by responsibility:

- recording operation orchestration;
- supervisor and child launch;
- leases and live policy;
- MP4 validation;
- generalized capture import/accounting;
- Range parsing and bounded streaming;
- board recording state/player;
- P5b-2 schemas, grants and projections;
- focused tests.

Every changed source module, including test modules, remains below 800 lines.

Before implementation:

1. Record OD-1 through OD-6.
2. Update the parent’s phase table and superseded recording names.
3. Update protocol feature/method documentation, parity and security checklist.
4. Obtain DESIGN REVIEW PASS for this complete contract.

Before enabling P5b-1:

1. Complete its four proofs and required checks.
2. Obtain PRE-PR review.
3. Complete the real-recorder retained-FD/finalization smoke.

Before enabling P5b-2:

1. Complete and accept P6.
2. Complete recording-specific authority, budget, exposure and version proofs.
3. Verify that existing grants remain screenshot-only.
4. Record live-smoke limitations explicitly.

This read-only proposal does not establish implementation or production acceptance.
