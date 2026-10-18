# P5a: owner screenshots and artifact deletion (revision 11)

- **Status:** built on v2 (DL-233); PRE-PR review: round 1 found 3 MAJOR defects, fixed in `59699f69` and `e23c0c16`; round 2 PASS (P5a-18). Live smoke of `grim` pending. Revision 11 (2026-10-18), accepted by the owner with the round 3 fix `aced66e8` (O11-4).
- **Parent:** [omarchy.md](./omarchy.md) (§6.5A, §6.5B, §11, §13). This annex overrides §6.3, §6.4, §6.5, §6.5A and §7 where they differ. DL-230 items 1-4 stay binding and unchanged.
- **Sibling:** [omarchy-p8.md](./omarchy-p8.md) (provisioning).
- **Scope of P5a:** one owner action, a monitor screenshot from the board Desktop panel, and the owner deletion of any artifact. Recordings (P5b) and the MCP tools (P6) are not in P5a.
- **Code references:** each `file:line` points to the code at `c00eea8f`.

## 1. Scope and phase disposition

The split alone does not close the findings A1-A11. It removes recording and MCP work from the review. P5a still needs a durable capture result, deletion barriers, bounded binary output, and complete timeout behavior.

Phases:

- **P5a:** the owner monitor screenshot from the Desktop panel, and the owner deletion of any artifact.
- **P5b:** recordings, the `video` kind, `Range` playback and the recording stop. It has its own later DESIGN REVIEW.
- **P6:** the MCP tools, the `capture` class and the MCP image return. P6 depends on P3 and P5a, not on recordings.

Disposition of the findings:

| Finding | Disposition | Effect of the split |
| --- | --- | --- |
| A1 | closed (agent part: deferred to P6) | Closed by the DL-230 policy. Agent implementation and disclosure belong to P6. |
| A2 | closed | Monitor-only scope stays closed. Keep the composed-pixel disclosure and the lock admission rule. |
| A3 | closed | Closed by the DL-230 ordinary-artifact policy. |
| A4 | P5a contract | Still open. Implement general artifact deletion in P5a (section 3). |
| A5 | deferred to P5b | P5b has its own design review. Do not label it fixed. |
| A6 | P5a contract | The screenshot part stays open. The contract in section 4 closes the design gaps. |
| A7 | P5a contract | The screenshot part stays open. The contract in section 5 closes the design gaps. |
| A8 | P5a contract | The screenshot part stays open. The contract in section 6 closes the design gaps. |
| A9 | P5a contract | The screenshot part stays open. The contract in section 7 closes the design gaps. |
| A10 | deferred to P5b | Video on the board. |
| A11 | video and MCP image parts: deferred to P5b and P6; screenshot capability negotiation: P5a contract | Capability negotiation of the screenshot stays in P5a (section 2). |
| B1-B6 | P8 | See [omarchy-p8.md](./omarchy-p8.md). |

What the advisor found wrong with "one screenshot action" alone:

1. A timeout can occur after the station commit. The P3b authority prevents reuse on one connection epoch. It does not give a durable result lookup. Its consumed-ID registry is in memory (`crates/fleetdeck-station/src/desktop/authority.rs:154`).
2. An owner capture has no session publication event. The station emits `artifact.published` only when `session_id` exists. The deck must index owner captures explicitly and recover missed results (`crates/fleetdeck-station/src/store/artifacts.rs:425`).
3. Deletion is a general artifact change. It must cover ordinary publication, cache fills, metadata refresh, event replay, shares, public views and wall delivery. A capture that is only a screenshot does not reduce this requirement.
4. Capture settings are not implemented at `c00eea8f`. `StationConfig` has `allow_desktop`, but neither `allow_desktop_capture` nor `capture_monitors`. P5a must add their complete config, settings, elevation and live-policy wiring (`crates/fleetdeck-core/src/config.rs:235`).
5. The superseded text must change globally: §§6.3-6.5A, 7, 9-9A, 11-14 of `omarchy.md`.

## 2. Common wire contract

### 2.1 Features

- `desktop_capture.v1`: screenshot action and durable result lookup.
- `artifacts_delete.v1`: station artifact deletion.
- No `video` kind and no MCP tool in P5a.

### 2.2 The action

Keep the existing HTTP action route and the flat link parameters:

```http
POST /api/stations/{station}/desktop/act
X-FleetDeck-Capture-Id: <ULID>
Content-Type: application/json

{"action":"screen_capture","monitor":"DP-1"}
```

```json
{
  "action": "screen_capture",
  "monitor": "DP-1",
  "authority": "<existing DesktopAuthority object>"
}
```

- The header `X-FleetDeck-Capture-Id` is required for `screen_capture` only.
- The board generates one canonical 26-character ULID before it sends. The deck uses it as `authority.action_id`.
- Add `screen_capture` to both closed action enums and to `target_monitor`. The latter knows only the existing screen, workspace, wall and browser actions (`crates/fleetdeck-proto/src/desktop.rs:122`, `crates/fleetdeck-link/src/desktop_authority.rs:17`).

Authorization is, all at once:

- owner login;
- Admin scope;
- CSRF;
- `desktop_control`;
- `allow_desktop`;
- `allow_desktop_capture`;
- the live monitor allowlist.

Capture itself needs no elevation prompt. Turning capture on needs the existing elevated settings flow. Members, tokens, displays and coordinators cannot invoke this action in P5a.

### 2.3 The result

Add optional fields to the existing result:

```ts
type DesktopActResult = {
  address: string | null;
  info: DesktopInfo;
  capture?: {
    capture_id: string;
    published: ArtifactPublished;
  };
  info_fresh?: boolean; // absent means true
};
```

For a screenshot success, `address` is `null` and `capture` is required. Other actions keep their current result. The current result has only `address` and `info`. A separate `DesktopCaptureResult` would break that integration (`crates/fleetdeck-proto/src/desktop.rs:218`).

### 2.4 Result lookup

Add an owner and Admin result lookup:

```http
GET /api/stations/{station}/desktop/captures/{capture_id}
```

Its link method is `desktop.capture_status`. It is gated by `desktop_capture.v1`. Its params are:

```json
{"capture_id":"<ULID>"}
```

Both return this tagged union:

```ts
type CaptureStatus =
  | { state: "pending"; capture_id: string }
  | { state: "unknown"; capture_id: string }
  | { state: "committed"; capture_id: string; published: ArtifactPublished }
  | { state: "failed"; capture_id: string; error: string; message: string }
  | { state: "deleted"; capture_id: string };
```

`unknown` means that no durable station result is available. It never authorizes a resend.

### 2.5 Features endpoint and capability

- Add the owner-only `GET /api/features`. It returns `{"features":[...]}`. A missing route means an old deck.
- Add the optional field `features: string[]`, default `[]`, to `DesktopInfo`. The deck returns the intersection with the features that the current station link negotiated.
- Enable the screenshot UI only when both the deck and the station support `desktop_capture.v1` and `tools.grim` is true. Keep readiness and authorization separate.

### 2.6 Implementation boundary

- Use the existing `rustix`, Tokio, ring, serde/JSON, TOML, SQLite and local-socket facilities. No new third-party library is required. `grim` stays the specified external tool.
- Put capture, deletion, provisioning, recovery and tests in separate modules. Keep each changed source file below 800 lines.

## 3. A4: artifact deletion (P5a contract)

The reported race exists. The cache fill awaits station reads, then does an unconditional cache upsert. Share insertion and metadata insertion also lack a tombstone guard (`crates/fleetdeck-deck/src/http/artifact_stream.rs:110`, `crates/fleetdeck-deck/src/store/artifacts.rs:395`).

Rules:

- `DELETE /api/artifacts/{station}/{id}` needs owner login, Admin, CSRF and fresh elevation.
- The identity is the stable station ID of the deck plus the artifact ID. It is never a station name or a supplied path.
- A deck tombstone is **permanent**. Station acknowledgement does not remove it. Station removal and a later re-enrollment must not cascade-delete it.
- One deck transaction inserts the tombstone, removes the artifact, index, version, cache, share and view rows, and creates the station deletion job.
- Every artifact write checks the tombstone **inside its write transaction**. This includes `record_artifact`, `note_artifact`, `put_artifact_versions`, `put_artifact_file`, share and view creation, and capture reconciliation.
- Every artifact read excludes tombstones before it returns metadata or bytes. Cover list, get, raw, `/a/{token}/`, public shares, display artifacts and coordinator reads.
- Cached and station-backed bodies yield chunks of at most 700 KiB. Each path checks deletion immediately before it yields each chunk. Bytes that were already yielded cannot be recalled.
- Do not keep an unchecked full cached response body as one large chunk.

### 3.1 The station method

```text
method: artifacts.delete
scope: Admin
feature: artifacts_delete.v1
params: {"artifact_id":"<id>"}
result: {"artifact_id":"<id>","state":"deleted"}
```

Station deletion first commits a permanent local tombstone and a cleanup job. All station publication and import commits check that tombstone. A per-artifact lock covers the final file move and the DB commit, and deletion uses the same lock. Thus a publisher cannot move bytes into the artifact tree after the deletion cleanup.

Cleanup removes the complete version tree of the artifact through directory-relative, no-follow operations. It then removes the metadata and marks the cleanup job complete. Missing files and an already deleted identity are success. Send `deleted` only after the cleanup completes. On restart, finish the pending cleanup before serving artifact reads.

### 3.2 HTTP results

| Condition | Result |
| --- | --- |
| First delete; station cleanup completes | `200 {"state":"deleted"}` |
| Station offline, unsupported, or cleanup pending | `202 {"state":"pending","reason":"offline" \| "unsupported" \| "retry"}` |
| Repeated delete | Same current state; no new identity or job |
| Unknown identity with no tombstone | `404 unknown_artifact`, `"No such artifact."` |
| Any ordinary read after a tombstone | `404 unknown_artifact`, `"No such artifact."` |

- Add the owner and Admin route `GET /api/artifacts/{station}/{id}/deletion` with the same state body.
- On every link-up, try the pending deletion first. The later retry delays are 1, 2, 4, 8, 16, 32, then 60 seconds.
- Unsupported peers stay pending until they are upgraded. Do not report a physical deletion.
- Deadlines: 5 seconds for an online deletion attempt, 10 seconds for the deck handler, 15 seconds for the board fetch. After the deck transaction commits, a timeout leaves the deletion pending.

### 3.3 UI text

- Button: "Delete artifact"
- Confirmation: "Delete this artifact and all its versions? Downloaded copies and provider conversations will remain."
- Pending: "Hidden on this deck. Waiting for station deletion."
- Unsupported: "Hidden on this deck. Upgrade the station to delete its copy."

### 3.4 Audit and meaning

Audit `artifacts.delete` with the identity and the result code only. "Deleted" means removal from the active Fleet Deck stores. It does not promise secure erasure of database pages or backups.

### 3.5 Tests (A4)

- Pause the cache fill, the publication commit, the metadata refresh, the share creation and the event replay across a deletion. None may restore rows or bytes.
- Test a repeated delete.
- Test a station restart and a deck restart at each cleanup boundary.
- Test offline and unsupported peers.
- Test a deletion during every delivery path.

## 4. A6: screenshot budgets (P5a contract)

The current artifact staging copies without a byte cap. The 1 MiB cap of the desktop runner applies to text stdout, not to capture files (`crates/fleetdeck-station/src/artifacts/stage.rs:75`, `crates/fleetdeck-station/src/desktop/run.rs:23`).

| Resource | P5a limit and enforcement |
| --- | --- |
| Concurrent capture | One per station; reject immediately rather than queue |
| Output | PNG, at most **33,554,432 bytes** |
| Dimensions | Each dimension 1-8192; product at most **16,777,216 pixels** |
| Station capture quota | **4,294,967,296 bytes** |
| Reservation | Reserve the full 33,554,432 bytes before spawning |
| Free-space reserve | Require available bytes >= **2,147,483,648 + 33,554,432** before spawning |
| Copy buffer | At most 65,536 bytes |
| Capture cache on deck | At most **268,435,456 logical bytes**, with LRU eviction |

- Quota accounting includes committed capture-version bytes, capture staging files, unreaped capture garbage and active reservations.
- Release an unused reservation only after the commit or a completed cleanup.
- Count all generated capture versions until their physical station copy is removed.
- Ordinary artifact limits stay unchanged.

Write rules:

- Before each file write, reject any buffer that would cross the output cap.
- Read one additional byte to tell an exact-cap EOF from an overflow. Never write that extra byte.
- Check the available space before each write. Fail on reserve loss or `ENOSPC`.
- Other processes can use the disk between checks. The reserve is an admission and check rule, not a claim of a filesystem reservation.
- Validate the reported monitor dimensions before spawning and the PNG dimensions before import. Do not silently resize. Do not retry at a smaller size.

Cache rules:

- Deck capture-cache insertion and eviction use one transaction. It is also subject to the A4 tombstone guard.
- If the capture cannot be cached, leave the artifact valid and serve it from the station.
- The cache limit counts stored payload bytes, not the SQLite file size.

### 4.1 Station codes

Preserve these codes through the capture-specific HTTP mapping:

| HTTP | Code | Exact text |
| ---: | --- | --- |
| 409 | `busy` | `A capture is already running.` |
| 507 | `disk_full` | `The station has too little free space for a capture.` |
| 409 | `capture_quota` | `This capture would exceed the station's 4 GiB capture limit. Delete some captures first.` |
| 413 | `capture_too_large` | `The screenshot exceeds 32 MiB.` |
| 422 | `capture_dimensions` | `The monitor exceeds the screenshot size limit.` |

The existing relay maps unknown wire errors to the generic `desktop_failed`. It must gain this allowlisted capture mapping (`crates/fleetdeck-deck/src/link/desktop_authority.rs:187`).

### 4.2 Tests (A6)

- Exact cap and cap+1.
- A quota where only a full reservation would cross the limit.
- Incompressible pixels.
- Dimension and product boundaries.
- Simultaneous requests.
- Reserve loss during output.
- Disk-full cleanup.
- Restart with abandoned reservations.
- Cache eviction without artifact deletion.

## 5. A7: screenshot process and files (P5a contract)

Do not pass a staging pathname to `grim`. Use its binary stdout:

```text
grim -t png -s 1 -l 6 -o <monitor> -
```

The final `-` makes stdout the image destination. This avoids the default Pictures or current-directory fallback. It also avoids that the tool reopens a precreated file. The upstream manual documents this interface: [grim manual](https://github.com/emersion/grim/blob/master/grim.1.scd).

- Use grim **1.5.0** as the supported P5a release baseline. Arch packages it now as `1.5.0-2`. The package revision is not part of the capture protocol.
- Release acceptance must test that version.
- A runtime capability failure is an error. It is never a different capture backend.
- Do not invent `grim --version`. The documented interface has no version option. See the [Arch package](https://archlinux.org/packages/extra/x86_64/grim/).

### 5.1 The binary runner

Add a binary runner. Do not reuse the text runner:

```rust
Runner::capture_png(
    monitor: &str,
    output: &mut File,
    byte_limit: u64,
    deadline: Instant,
) -> Result<CapturedPng, Fail>
```

Rules:

- Resolve the executable with the existing desktop PATH rules.
- Use structured argv, a cleared environment plus the desktop `ChildEnv`, null stdin, and piped stdout and stderr.
- Stream stdout to the retained output descriptor. Never convert it to text. Never log it.
- Keep at most 16 KiB of stderr, then drain and discard it. Never return stderr to the board.
- Give `grim` 10 seconds, within the station action deadline.
- On timeout, cancellation, output overflow or write failure: signal the owned process group with `SIGKILL`, reap the child, and finish or cancel the pipe tasks before the staging file is unlinked.
- Missing tool: `unsupported`, "The screenshot tool grim is not installed."
- Nonzero exit, bad PNG structure or malformed output: `capture_failed`, "The screenshot tool failed."
- Never fall back to a recorder, a portal, window geometry, a workspace file or the Pictures directory.

The current runner does not set up a process group. Its detached `spawn` keeps children alive on purpose (`crates/fleetdeck-station/src/desktop/run.rs:83`).

Reuse the existing hidden `shim-exec` mechanism for `setsid`, the Linux parent-death `SIGKILL`, and the parent check after registration. Launch and supervise it from a dedicated OS thread that stays alive until the child is reaped, because Linux parent-death signals follow the spawning thread. The supported grim runs directly and starts no helper processes. Normal cancellation kills the complete owned group (`crates/fleetdeck-providers/src/host/shim/server_proc.rs:69`, `crates/fleetdeck-providers/src/host/shim/server.rs:3`).

### 5.2 Staging layout

```text
<station data>/captures/                         0700
<station data>/captures/<capture_id>/            0700
<station data>/captures/<capture_id>/capture.png 0600
```

- Open the directory components without following symlinks.
- Create the leaf with `O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC`, mode 0600.
- Require the effective UID, a regular file type and a link count of one.

### 5.3 After child exit and stdout EOF

1. Verify the PNG signature, IHDR, bounded chunk lengths, chunk CRCs and the terminal IEND.
2. Check the byte and dimension limits.
3. Rewind the same retained descriptor and calculate the SHA-256.
4. Recheck the descriptor identity, size, mode, owner and link count.
5. Import from that descriptor. Do not reopen the file by its original path.

Use `import_capture(File, CaptureMetadata)`, not `import_capture(path, …)`. The existing staging already uses directory-relative no-follow reads and SHA-256. Reuse those primitives. No PNG decoder dependency is needed for this structural check.

### 5.4 Lock and monitor checks

- Before pixels are requested, refresh the monitor and lock state. Require `locked == Some(false)`.
- Check them again before import. Discard the output if the screen is locked, unknown or missing.
- Keep the existing authority admission. Add the final start check immediately before `grim`. An earlier probe must not make the start deadline ineffective.
- At station startup, finish the capture recovery before the station accepts another capture. Remove uncommitted staging and the artifact directories with no committed version row. Keep committed artifacts.

### 5.5 Tests (A7)

- Exact argv.
- Binary bytes.
- No fallback path.
- Symlink and hard-link refusal.
- Changed inode.
- Malformed PNG.
- Stalled stdout.
- Full stderr pipe.
- Nonzero exit.
- Cancellation with a child group.
- Station death.
- Startup cleanup.

## 6. A8: import, publication and result (P5a contract)

Normal publication calls both deck indexing functions explicitly. It is not enough to return the station artifact ID (`crates/fleetdeck-deck/src/http/artifact_routes.rs:241`).

### 6.1 Capture values

| Field | Value |
| --- | --- |
| Artifact ID | Station-generated ULID, allocated before the final file move |
| Artifact station | Captured station; the deck replaces the station-side name with its stable station ID |
| `session_id` | `null` |
| `author` | `owner` |
| `title` | `Desktop capture · <station name>/<monitor>`, cut with the existing 200-character title rule |
| `kind` | `image` |
| Version | `1`; every capture creates a new artifact |
| Files | `["capture.png"]` |
| Size | Actual PNG byte count |
| SHA-256 | Lowercase hex SHA-256 of the PNG |
| Repository association | None |

Add the optional immutable `capture` metadata to `ArtifactVersion`:

```ts
type CaptureProvenance = {
  capture_id: string;
  station_id: string;
  monitor: string;
  kind: "screenshot";
  caller: "owner";
  caller_station: null;
  at: number; // station Unix milliseconds at capture start
};
```

Derive all fields inside the station and deck capture path. Accept none from public artifact publication parameters. The existing `ArtifactVersion` has no provenance field (`crates/fleetdeck-proto/src/artifacts.rs:144`).

### 6.2 Durable records

- **Deck:** commit `(station, capture_id, action hash, send state)` before sending. Change the send state to "send attempted" before the network call. After a crash in that interval, treat the capture as unknown. Never send it again.
- **Station:** commit a capture-operation row before it starts pixels. Key it by the authenticated deck fingerprint plus the capture ID. Store the action hash and the operation state.
- The station artifact, version and provenance commit and the `committed` result of the operation are one DB transaction.
- Sync the PNG and the destination directory before the station DB commit. The import layout is `<station data>/artifacts/<artifact_id>/1/capture.png`. Before the station DB commit, in this order: `fsync` the PNG file; `fsync` the version directory `1/`; `fsync` the artifact directory `<artifact_id>/`; `fsync` `<station data>/artifacts/` (it gets the new `<artifact_id>` entry); and `fsync` `<station data>/` too when this import created `artifacts/`. Each directory that the import created or renamed into is synced, up to the first ancestor that existed before the import. A failed `fsync` is a failed import (`capture_publish_failed`, before the commit); the operation is never `committed` after a failed sync. (Round 3 finding A8, `adv-omarchy-p5-p8-r3-report.md`.)
- Test: a store fault hook that drops the directory entries that were not synced, run after the DB commit, shows that no committed capture can lose its PNG; a kill test alone does not prove this.
- Persist the artifact ID before the file move. Recovery can then identify and remove a move whose DB commit never completed.
- A station restart marks an interrupted, uncommitted capture as failed. It never captures again.
- Keep a permanent compact record of the capture ID, the hash, the terminal code and the artifact reference. Do not keep full desktop snapshots in these records.

The existing store moves staged files before the DB commit, so a crash can leave a final directory without a row. P5a must cover this boundary explicitly (`crates/fleetdeck-station/src/store/artifacts.rs:440`).

At the deck, store the artifact index, the version and provenance, and the capture completion atomically. Refactor the existing indexing operations to share that transaction. Do not call their current independent transactions and then claim atomic completion.

### 6.3 Recovery

Recovery runs on deck start, on station link-up, and every 30 seconds while a capture result is unresolved. It calls only `desktop.capture_status`. A committed station result is enough to repair the deck indexing. A tombstone takes precedence and yields `deleted`.

### 6.4 Failure results

Before the station commit, an import failure removes the staging and returns:

```text
capture_publish_failed
The screenshot could not be saved as an artifact.
```

After the station commit, a deck indexing failure must **not** claim that the screenshot failed, and must not delete it. Return `202` with the capture ID and the pending state. UI text: "The screenshot was saved on the station. The artifact list is still updating."

Do not let a failed post-capture desktop refresh turn a committed artifact into an action failure. Return the last valid `info` with `info_fresh:false`. This is a capture-specific exception to the current post-action refresh (`crates/fleetdeck-station/src/desktop/mod.rs:265`).

### 6.5 Tests (A8)

- A crash before and after the file move.
- A crash before and after the station commit.
- A crash before and after the deck commit.
- A lost success response.
- A failed info refresh.
- The same ID with a different monitor (conflict).
- A deletion during reconciliation.
- Each request produces at most one artifact.

## 7. A9: names and deadlines (P5a contract)

Canonical P5a names:

- Action: `screen_capture`
- Action transport: `desktop.act`
- Result lookup: `desktop.capture_status`
- HTTP paths: those in this annex
- No P5a MCP name

`capture.screenshot`, `capture.record`, `screen_record`, `screenshot`, `record_demo`, `desktop_screenshot` and `desktop_record` are inactive historical proposals in the affected sections of `omarchy.md`. P5b and P6 must define their own reviewed names.

### 7.1 Deadlines

| Hop or stage | Deadline |
| --- | ---: |
| Authority start | Existing 10-second authority and clock/epoch rules |
| Pre-capture context, monitor and lock admission | 5 seconds total |
| `grim` | 10 seconds |
| Station screenshot operation | 30 seconds from intake |
| Deck wait for station action | 35 seconds |
| Deck action HTTP handler, including indexing | 40 seconds |
| Board screenshot fetch, including body consumption | 45 seconds |
| Station result lookup | 5 seconds |
| Deck result lookup | 10 seconds |
| Board result lookup | 15 seconds |

The current station and deck short limits are already 30 and 35 seconds. The board uses a 20-second settings transport for desktop calls. It must get a separate capture transport (`crates/fleetdeck-station/src/desktop/deadline.rs:12`, `crates/fleetdeck-deck/src/link/desktop_authority.rs:34`, `web/src/runtime/RuntimeApp.tsx:245`).

The 30-second deadline stops new capture work. A file sync or a DB commit that is already in progress may finish after the timeout. The owned operation records its final result. Dropping an HTTP waiter must not abandon that bookkeeping. Do not claim a hard deadline for uninterruptible filesystem I/O.

### 7.2 No second screenshot

- Neither a browser disconnect nor a deck timeout starts another screenshot.
- Keep capture outside every automatic mutation outbox.
- The board keeps the capture ID across a reload, polls the status every two seconds for 60 seconds, and then offers "Check result". It never repeats the POST automatically.

### 7.3 Errors and texts

- Timeout: `504 desktop_timeout`, "The station did not answer in time; the action may have run."
- Unknown lookup: "The screenshot result is unknown. Check the artifact list before taking another screenshot."
- Same ID with different parameters: `409 capture_id_conflict`, "This capture ID was used for another request."
- Locked or unknown lock: `409 screen_locked`, "The screen is locked or its lock state is unknown."
- Missing monitor: `404 desktop_not_found`, the existing target-not-found text.
- Gate or authority refusal: the existing `403 desktop_forbidden` mapping and fixed text.

### 7.4 Viewing the screenshot

Do not fetch PNG bytes inside the action result. Keep the 700 KiB `artifacts.file` chunks. Add a **60-second total** capture-file fetch limit at the deck, with the existing **20-second per-station-request** limit, and a **65-second** board image-load timer. On expiry, cancel the fetch and the image request, and show "The screenshot could not be loaded. Try again." Loading bytes may be retried. Taking the screenshot may not.

### 7.5 Tests (A9)

- A browser wait that exceeds the old 20 seconds.
- Station, deck and board timeout boundaries.
- A delayed DB completion.
- A reload with a pending ID.
- No mutation-outbox entry.
- No repeated capture after a reconnect.
- Bounded image loading.

## 8. Review boundary

Round 3 reviews P5a and P8 against these complete contracts. It records the P5b and P6 findings as deferred. It keeps the release-dependent P8 acceptance explicit ([omarchy-p8.md](./omarchy-p8.md) section 5).
