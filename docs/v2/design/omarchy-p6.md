# P6: coordinator desktop tools and monitor screenshots

- **Status:** proposed contract (2026-10-20, from the advisor answer `advisor-p6-answer.md`; revised for DESIGN REVIEW round 1, decision O12-2). The architect accepts the recommendations OD-1 to OD-7 (decision O12-1; the owner may change any of them). Adversary DESIGN REVIEW: round 1 FAIL (fixed, O12-2), round 2 one MAJOR (fixed, O12-3), round 3 PASS (2026-10-20). Accepted for implementation; PRE-PR review required before merge.
- **Parent:** [omarchy.md](./omarchy.md), §§7.1–7.3, 7.5, 11 and 13.
- **Prerequisites:** built P3 desktop relay and dispatch authority; built P5a screenshot, durable result, import, deletion and artifact paths.
- **Authority model:** ADR-006 coordinator bindings and lifecycle, extended through ADR-008 grants.
- **Scope:** coordinator MCP tools for desktop information, notifications, previews, caller-owned window closing, existing wall directives, monitor screenshots and bounded screenshot image reads.
- **Excluded:** recordings and `record_demo` (P5b); all input synthesis (P7); new grants for ordinary sessions; terminal handover; wall pairing, opening, closing or rebinding.

This annex supersedes the affected draft P6 rules of the parent once accepted. It does not change the owner HTTP authorization model.

## 1. Owner decisions

These are proposed decisions, not previously accepted instructions. Implementation must use the accepted choices, and update this annex before its DESIGN REVIEW.

| ID | Decision needed | Recommendation |
|---|---|---|
| **OD-1** | Parent §7.2 permits ordinary sessions to receive desktop classes. This brief specifies coordinator sessions. | P6 supports coordinator bindings only. Ordinary sessions’ `own` tools gain no authority. |
| **OD-2** | Parent §7.2 introduces a separate `desktops` list. Option A reuses grant target stations. | Use ADR-008 `stations`, labelled “Grant stations” on the board. Do not add `desktops`. Explain that this one list bounds every class in that grant. |
| **OD-3** | Whether desktop tools participate in the existing fleet-control kill switches. | Classify `desktop` and `capture` as fleet-control classes. Require `fleet_control`, caller `allow_fleet_control`, target `allow_fleet_control`, and the applicable desktop gates. |
| **OD-4** | The drafted `terminal_open` cannot reuse the existing handover unchanged. | Defer it. P6 does not advertise or accept `terminal_open`; retain owner handover unchanged. |
| **OD-5** | Meaning of remote capture and permission to read a saved image. | Permit pixels from the explicitly selected grant station. A capture grant may read only captures made by that same grant and caller. Current capture gates also govern subsequent MCP image reads. |
| **OD-6** | Numeric action, capture, read and image budgets. | Accept §8, including a 700 KiB (716,800-byte) MCP PNG limit without resizing (decision O12-2: one existing artifact chunk, so the image crosses each link hop in one frame). A larger screenshot remains an ordinary owner-viewable artifact. |
| **OD-7** | Meaning of “own task” for coordinator wall references. | Use the caller and its spawn lineage, plus captures belonging to the same grant and caller. Do not let fleet scope alone authorize arbitrary wall references. |

The remaining protocol details implement these choices; they are not separate requests for elevation or per-action approval.

Inherited owner decisions remain binding:

- All settings, grant creation, elevation, renewal and revocation are available from the board.
- No station-machine step is required.
- Capture needs no per-screenshot confirmation.
- Screenshots are ordinary artifacts under the accepted P5a policy.
- Agents run as the owner’s UID. These tools do not isolate them from other same-UID processes.

## 2. Grants and settings

### 2.1 Classes

Add the exact serialized values `desktop` and `capture` to both `GrantClass` and `ToolClass`.

Neither implies the other, `inspect`, `manage`, `answer`, `spawn`, `drive` or `input`.

| Class | Tools |
|---|---|
| `desktop` | `desktop_info`, `wall_show`, `notify`, `preview_open`, `window_close`, `desktop_action_status` |
| `capture` | `desktop_info`, `screenshot`, `capture_status`, `read_capture_image` |

`desktop_info` requires **either** class. Other tools require their listed class.

Extend the active-mutating-grant exclusion of ADR-008 to include both new classes. Another coordinator cannot drive, manage or answer for a session holding an active desktop or capture grant.

Physical desktop targeting uses grant stations regardless of `scope = lineage | fleet`. Session and artifact references in `wall_show` additionally follow §4.2.

### 2.2 Creation and lifecycle

Only the owner creates a grant through the existing board start or resume flow, with fresh elevation in the same request. Members, share guests, API tokens, displays and coordinators cannot create or widen one.

The creation request includes:

- Classes and stable target station IDs.
- `desktop_cap`.
- `capture_cap` when `capture` is present.
- `image_byte_cap` when `capture` is present.
- Existing idle and absolute expiry fields.

Defaults and maxima remain ADR-006’s:

- Idle expiry: default 30 minutes; maximum 24 hours.
- Absolute expiry: default 12 hours; maximum seven days.
- Idle renewal follows the existing generation handshake.
- Renewal never resets usage counters or operation history.
- Revoked grants are terminal. Resume never restores an old grant.

Adding classes or changing target stations requires a newly authorized grant/start or resume; P6 adds no in-process privilege upgrade. Cap increases use an elevated owner board action; decreases and revocation require no elevation.

Creation validates the current features of the caller and every target (§10). Settings may be off at creation: permission and operational readiness remain separate.

### 2.3 Live gates

Following **OD-3**, every P6 call requires:

1. Deck `fleet_control`.
2. Caller station `allow_fleet_control`.
3. Target station `allow_fleet_control`.
4. Deck `desktop_control`.
5. Target station `allow_desktop`.
6. For capture-class calls: target `allow_desktop_capture`.
7. For a capture’s monitor: the live `capture_monitors` rule.

An empty `capture_monitors` allows all monitors, including later attachments. New screenshots additionally require that the monitor exists now.

Caller station `allow_desktop` and `allow_desktop_capture` are not prerequisites for operating another station. A headless coordinator station is supported.

No station setting is treated as live merely because its publication has reached the deck. The deck obtains a fresh target-policy answer; the target independently reads its own settings at execution or delivery. Caller policy uses the live caller-policy round trip.

The board retains the existing elevated flows for enabling dangerous settings. No new setting is introduced.

## 3. MCP catalog and exact input schemas

Keep the existing endpoint, credential authentication, Host/Origin checks, protocol versions and lifecycle watermark.

The catalog is fixed at provider-process start. Class membership determines the tools listed; switches determine whether calls succeed. A later disconnect, downgrade or switch change does not silently change the catalog.

P6 adds exactly these nine names:

```text
desktop_info
wall_show
notify
preview_open
window_close
desktop_action_status
screenshot
capture_status
read_capture_image
```

No aliases are accepted. In particular, `desktop_screenshot`, `desktop_record`, `record_demo`, `terminal_open` and input tools are absent.

The following JSON is the normative schema bundle. For tool `N`, its `inputSchema` is `schemas[N]` with the bundle’s `$defs` copied into that schema’s root. References then resolve locally. This expansion is mechanical.

```json
{
  "$defs": {
    "id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 128,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "request_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "display_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "monitor": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9._-]+$"
    },
    "window_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "session_ref": {
      "type": "object",
      "additionalProperties": false,
      "required": ["station_id", "session_id"],
      "properties": {
        "station_id": { "$ref": "#/$defs/id" },
        "session_id": { "$ref": "#/$defs/id" }
      }
    },
    "artifact_ref": {
      "type": "object",
      "additionalProperties": false,
      "required": ["station_id", "artifact_id", "version"],
      "properties": {
        "station_id": { "$ref": "#/$defs/id" },
        "artifact_id": { "$ref": "#/$defs/id" },
        "version": {
          "type": "integer",
          "minimum": 1,
          "maximum": 4294967295
        }
      }
    },
    "wall_content": {
      "oneOf": [
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "session"],
          "properties": {
            "kind": { "const": "session" },
            "session": { "$ref": "#/$defs/session_ref" }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "artifact"],
          "properties": {
            "kind": { "const": "artifact" },
            "artifact": { "$ref": "#/$defs/artifact_ref" }
          }
        }
      ]
    }
  },
  "schemas": {
    "desktop_info": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "station": { "$ref": "#/$defs/id" }
      }
    },
    "wall_show": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "display_id", "content"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "display_id": { "$ref": "#/$defs/display_id" },
        "content": { "$ref": "#/$defs/wall_content" },
        "pin": { "type": "boolean", "default": false }
      }
    },
    "notify": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "title", "body"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "title": {
          "type": "string",
          "minLength": 1,
          "maxLength": 80
        },
        "body": {
          "type": "string",
          "maxLength": 300
        },
        "urgency": {
          "type": "string",
          "enum": ["low", "normal"],
          "default": "normal"
        }
      }
    },
    "preview_open": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "url"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "url": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2048
        },
        "monitor": { "$ref": "#/$defs/monitor" }
      }
    },
    "window_close": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "window_id": { "$ref": "#/$defs/window_id" }
      }
    },
    "desktop_action_status": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" }
      }
    },
    "screenshot": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "monitor"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "monitor": { "$ref": "#/$defs/monitor" }
      }
    },
    "capture_status": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" }
      }
    },
    "read_capture_image": {
      "type": "object",
      "additionalProperties": false,
      "required": ["artifact"],
      "properties": {
        "artifact": { "$ref": "#/$defs/artifact_ref" }
      }
    }
  }
}
```

Additional validation is mandatory on both trusted implementation sides:

- Omitted `station` means the authenticated caller’s station. Normalize defaults before computing the payload hash.
- IDs identify existing records; a valid string is not authority.
- JSON Schema lengths are Unicode scalar counts. Apply byte limits separately where specified.
- Reject unknown fields at every object level.
- Do not accept caller identity, grant, binding, generation, authority, executable, argv, environment, pathname, arbitrary artifact filename or raw window address from tool arguments.
- Never interpolate a schema error containing supplied text into an agent-visible error or audit row.
- MCP mutations require `request_id`. The caller must retain it and reuse it when checking or repeating the same request.

## 4. Tool behavior and returned data

### 4.1 `desktop_info`

Read fresh information from exactly one authorized target.

Return only:

```ts
type CoordinatorDesktopInfo = {
  station: string;
  kind: "omarchy" | "hyprland";
  monitors: Array<{
    name: string;
    width: number;
    height: number;
    scale_pct: number;
    focused: boolean;
    dpms_on: boolean | null;
    capture_allowed: boolean;
  }>;
  locked: boolean | null;
  capabilities: {
    notify: boolean;
    preview: boolean;
    screenshot: boolean;
  };
  walls: Array<{ display_id: string; monitor: string }>;
};
```

Limits: at most 64 monitors, 64 wall entries and 16 KiB serialized output. Exceeding a limit fails with `desktop_result_too_large`; do not silently return an incomplete discovery result.

A capture-only caller receives only monitors permitted for capture and `walls: []`. Capabilities requiring the absent `desktop` class are false.

Never return monitor descriptions or serials, window lists, titles, PIDs, addresses, wall contents, display ceilings, URLs, desktop environment variables or raw `DesktopInfo`.

### 4.2 `wall_show`

This tool changes a directive on an **existing, owner-paired display**. It does not launch a browser or create, replace or widen a display binding.

Require:

- Display exists, is not revoked and is currently bound to the selected target station.
- Selected target is a grant station.
- Every referenced station is a grant station and lies inside the display ceiling.
- Session references name the caller or a session with this grant in its spawn lineage.
- Ordinary artifact references belong to such a session.
- A sessionless capture artifact belongs to this grant and caller.
- Artifact version is explicit, exists and is not tombstoned.

With **OD-7**, these rules apply even under fleet scope.

Map `session` to the existing `focus` directive and `artifact` to the existing `artifact` directive. Construct the filter from the referenced station only; the agent cannot supply a filter, batch, URL or another directive mode.

Activate `DirectiveSender::Coordinator` through an authenticated coordinator entry point. Store the grant identity and binding generation in the coordinator directive provenance. Updating only the currently refused enum arm is insufficient.

Before every directive or associated content delivery:

1. Recheck display login, binding and ceiling.
2. Recheck grant, generation, expiry, reference ownership and tombstones.
3. Obtain current caller and target policy.
4. Repeat deck-local checks after those awaits.
5. Emit only if all checks still pass.

Invalidation removes the affected stored coordinator directive through the existing generation mechanism. An uncertain remote policy gives the effective `mission` directive for that response; it never falls back to an owner payload.

Existing display projection, sandbox, CSP and artifact deletion checks remain mandatory. An agent receives only the resulting directive generation, never display credentials or rendered wall data.

### 4.3 `notify`

Use the built typed `Notify` action and fixed `notify-send` argv with `--` terminating options.

- Urgency is `low` or `normal`; `critical` is refused.
- Strip markup with the existing helper.
- Reject control characters after stripping.
- Prefix the title with the authenticated generated callsign and `": "` (the `caller_label` of §5.4). The deck builds the final prefixed and truncated title before it hashes the `Notify` action, so the target receives and the hash covers the final text (decision O12-4).
- Truncate the final prefixed title to 80 scalars and body to 300 scalars.
- Reject an empty final title.
- No click action, board route, executable or notification replacement ID is accepted.

Return `{}` on confirmed success. Do not invent a notification ID where the adapter does not return one.

### 4.4 `preview_open`

Map to built `BrowserOpen`; derive its opener from the authenticated caller.

Apply the existing URL validation:

- HTTP or HTTPS only.
- At most 2048 bytes.
- Printable, non-space ASCII.
- Existing host, bracketed IPv6 and port grammar.
- No user information or alternate URL scheme.

Add the coordinator restriction from the parent (decision O12-4: the deck checks the URL grammar; the target checks the host list with its live tailnet status at execution, and refuses with `invalid_params`): the initial host must be exactly `localhost`, `127.0.0.1`, `[::1]`, or the selected target station’s authenticated advertised tailnet hostname. Hostname comparison is case-insensitive; suffix matches and caller-supplied aliases do not qualify. No DNS lookup grants permission.

The URL is interpreted on the **target** station. Do not rewrite a caller-local URL into another station’s origin.

Use the existing fresh Chromium profile and unique window class. Browser redirects and page networking are not a network sandbox; the grant disclosure states that opening a preview runs web content as the owner’s UID.

Return a station-generated opaque `window_id`. Never return the compositor address, PID, browser profile or URL.

### 4.5 `window_close`

Accept only the opaque `window_id` returned for a preview.

The target station keeps a preview record for each coordinator preview. It binds:

```text
deck fingerprint
grant ID
caller station ID
caller session ID
window ID (the opaque handle; 128 random bits, lowercase hex, 32 characters)
existing compositor signature, address and class identity
```

The existing window identity is the compositor signature, the address and the class. It has no PID: `desktop/events.rs` never uses one, and P6 adds none.

The wire action is a new feature-gated variant of `DesktopAction` (needs `desktop_mcp.v1`; a coordinator authority only):

```ts
{ action: "preview_close"; window_id: string }
```

The deck never learns or sends the compositor address. The `action_hash` of §5.4 covers the serialized `preview_close` object with its `window_id`, so the handle is the logical identity of the action from admission to the final dispatch. The legacy `WindowClose { address }` action and its owner bytes stay unchanged; a coordinator authority with the legacy wire action `window_close` is refused. The MCP tool keeps the name `window_close`; the deck maps it to `preview_close`.

After admission and the `execute` authority check, the target resolves the handle in its own preview records. The record must belong to the same grant (or its renewal) and the caller station and session of the coordinator context. A new grant does not inherit it.

The preview records live on the target station, so a deck restart does not change them. A target restart, a compositor reconnect, an event gap or a compositor change clears them with the existing record-clearing behavior; a later close then gives `desktop_window_not_found`.

Immediately before closing, retain the built fresh compositor identity check on the resolved signature, address and class. Reconnects, event gaps and compositor changes retain their existing record-clearing behavior.

Never close an owner-opened window, another coordinator’s preview or a wall window through this tool. Unknown and foreign handles produce the same error.

### 4.6 `screenshot`

Capture exactly one monitor through the P5a `screen_capture` operation.

- No window, region, workspace, output pathname, format, scaling or quality arguments.
- No automatic second capture.
- Return the durable capture state and, when committed, the artifact reference.
- Never include image bytes or `DesktopActResult.info`.

Preserve P5a’s PNG, process, filesystem, quota, lock, dimension, fsync, import, deletion and recovery rules.

Before `grim`, send a fixed station notification:

> Fleet Deck is taking a screenshot of `<monitor>` for `<callsign>`.

Both substitutions come from validated target/caller records. It has no action and no agent-supplied text. Notification failure refuses the new capture with `desktop_failed`; DND is not disabled or bypassed. Audit and board activity provide the additional visible record.

After pixels are obtained, recheck authority and local capture policy before import. If authority is lost before import starts, discard the uncommitted output and record `authority_lost`. A commit already underway may finish and must retain its durable result.

### 4.7 Capture provenance

Extend `CaptureCaller` with `coordinator`.

For a coordinator capture, preserve:

```ts
{
  capture_id: string;
  station_id: string;       // captured station's stable deck ID
  monitor: string;
  kind: "screenshot";
  caller: "coordinator";
  caller_station: string;   // coordinator's stable station ID
  caller_session: string;
  caller_grant_id: string;
  at: number;              // actual station capture-start time
}
```

The three new caller fields are optional in the shared type and omitted for existing owner captures. Owner `caller_station` remains `null`.

Additionally store binding generation in the private capture-operation record.

All provenance is derived from authenticated execution context. Public artifact publication cannot set or replace it. Durable operation, artifact and version commits preserve the relationship atomically.

Keep:

- Artifact station = captured station.
- `session_id = null`, including remote captures.
- Kind `image`, version `1`, file `capture.png`.
- No repository association.
- Author derived as `coordinator:<caller station>/<caller session>`.
- P5a title and size/hash rules.

Deck indexing and recovery must preserve coordinator provenance. They must not reconstruct a missing or malformed coordinator record as an owner capture. Invalid provenance prevents coordinator delivery and is reported to the owner as an indexing failure.

### 4.8 Status tools

`desktop_action_status` reaches only this caller’s non-capture operations. `capture_status` reaches only this caller’s capture operations.

Lookup is by this grant and a hash of `request_id`; no target station is supplied or searched by the agent.

Non-capture states:

```ts
type DesktopActionStatus =
  | { request_id: string; state: "unknown" }
  | {
      request_id: string;
      operation_id: string;
      station: string;
      state: "pending" | "unknown";
    }
  | {
      request_id: string;
      operation_id: string;
      station: string;
      state: "done";
      result: {} | { window_id: string } | { generation: number };
    }
  | {
      request_id: string;
      operation_id: string;
      station: string;
      state: "failed";
      error: string;
      message: string;
    };
```

Capture states:

```ts
type CoordinatorCaptureStatus =
  | { request_id: string; state: "unknown" }
  | {
      request_id: string;
      capture_id: string;
      station: string;
      state: "pending" | "unknown" | "deleted";
    }
  | {
      request_id: string;
      capture_id: string;
      station: string;
      state: "committed";
      artifact: {
        station_id: string;
        artifact_id: string;
        version: 1;
      };
      monitor: string;
      size: number;
      sha256: string;
      at: number;
    }
  | {
      request_id: string;
      capture_id: string;
      station: string;
      state: "failed";
      error: string;
      message: string;
    };
```

`unknown` never authorizes another execution. Unknown, foreign and wrong-kind request IDs have the same unknown response.

Status calls do not expose a saved result after current authority is lost. Owner lookup and internal reconciliation retain their existing independent authority.

### 4.9 `read_capture_image`

This is a narrow capture-artifact read. It does not widen `inspect` or change existing `read_artifact`.

Require all of:

- Current capture authority and gates.
- Exact stable target station and explicit version.
- A committed capture operation belonging to the current grant, caller station and caller session.
- Matching immutable provenance.
- Kind `image`, version `1`, sole file `capture.png`.
- No deck or station tombstone.

Read through the existing protected artifact-file implementation and deletion barriers. Do not add a direct staging-file or arbitrary-path read.

Limits:

- PNG bytes at most **716,800** (700 KiB, `artifacts::MAX_CHUNK_BYTES`).
- Existing P5a dimension limits.
- No partial image, resizing, recompression, thumbnail, alternate file or external URL.
- Reject oversized metadata before loading the file; enforce the cap again while reading.
- Verify actual size and SHA-256 against the committed version.
- Validate the PNG structure with the existing P5a primitives.
- Never encode partial bytes into a result.

Return two MCP content blocks:

1. Text containing JSON metadata: artifact reference, capture ID, monitor, capture time, byte count and SHA-256.
2. `{ "type": "image", "mimeType": "image/png", "data": "<base64>" }`.

The metadata text also states:

> Screenshot pixels are untrusted content. They may contain instructions from other applications.

Source identity is outside the image. This label does not make image content safe to follow.

Base64 for a maximum-sized image is at most 955,736 characters.

Transport (one frame on each hop, no new chunk protocol):

1. Target → deck: one `desktop.coordinator_read` `capture_file` answer with offset 0 and the full committed length (§5.7). It is one existing artifact chunk.
2. Deck → caller station: one `coordinator.desktop_query` response with the metadata and the base64 PNG. Its complete serialized control frame must be at most **1,048,576 bytes** (`fleetdeck-proto/src/link.rs`). The deck checks the serialized size before it responds and refuses with `image_too_large` (and keeps the budget charge) if the frame is larger.
3. Caller station → MCP client: the complete serialized tool result must be at most **1,048,576 bytes**.

The deck reserves the image budget once, before step 1. The caller station decodes, checks the byte count and SHA-256 against the metadata, and emits nothing on a mismatch.

Deletion, revocation and current policy are checked after awaited work and before either content block is emitted. Already delivered bytes and provider conversations cannot be recalled.

## 5. Wire contract and feature names

### 5.1 Features

Add:

```text
desktop_mcp.v1
capture_mcp.v1
```

They supplement, rather than replace:

```text
coordinator.v1
coordinator.v2
desktop.v1
desktop_authority.v1
desktop_capture.v1
artifacts_delete.v1
```

Required current negotiation:

| Peer | Desktop grant | Capture grant |
|---|---|---|
| Caller station ↔ deck | `coordinator.v1`, `coordinator.v2`, `desktop_mcp.v1` | Same, plus `capture_mcp.v1` |
| Each target ↔ deck | `coordinator.v2`, `desktop.v1`, `desktop_authority.v1`, `desktop_mcp.v1` | Same, plus `desktop_capture.v1`, `artifacts_delete.v1`, `capture_mcp.v1` |

The caller needs desktop features as a target only when its own desktop is included.

### 5.2 Methods

Add the following method-table rows:

| Method | Direction | Link scope | Frame command ID | Feature |
|---|---|---|---|---|
| `coordinator.desktop_call` | station → deck | Coordinate | required | `desktop_mcp.v1` |
| `coordinator.desktop_query` | station → deck | Coordinate | absent | `desktop_mcp.v1` |
| `desktop.authority_check` | station → deck | Coordinate | absent | `desktop_mcp.v1` |
| `desktop.policy_now` | deck → station | Read | absent | `desktop_mcp.v1` |
| `desktop.coordinator_read` | deck → station | Read | absent | `desktop_mcp.v1` |

Capture variants additionally require `capture_mcp.v1` in handlers.

Continue using `desktop.act` for actual notifications, previews, preview closes (`preview_close`, §4.5) and screenshots, with its existing Drive scope and `desktop_authority.v1` requirement.

`wall_show` commits a deck directive through the shared directive validation path; it does not invoke `desktop.act`.

### 5.3 Coordinator envelopes

`coordinator.desktop_call` uses a dedicated closed envelope:

```ts
{
  command_id: string;
  target_incarnation: string;
  retry_until: number;
  caller: string;
  binding: CoordinatorBinding;
  call:
    | { tool: "wall_show"; arguments: WallShowArguments }
    | { tool: "notify"; arguments: NotifyArguments }
    | { tool: "preview_open"; arguments: PreviewOpenArguments }
    | { tool: "window_close"; arguments: WindowCloseArguments }
    | { tool: "screenshot"; arguments: ScreenshotArguments };
}
```

Arguments use §3’s schemas, with defaults normalized by the trusted forwarder. Caller and binding are injected from the credential binding.

- Frame and envelope command IDs must match.
- `retry_until` is at most arrival plus ten seconds.
- This is a mutation with a durable acceptance/rejection record.
- It is explicitly excluded from both coordinator and station mutation outboxes.
- No transport layer automatically retries it.

`coordinator.desktop_query` uses:

```ts
{
  query_id: string;
  target_incarnation: string;
  deadline: number;
  caller: string;
  binding: CoordinatorBinding;
  query:
    | { tool: "desktop_info"; arguments: DesktopInfoArguments }
    | { tool: "desktop_action_status"; arguments: StatusArguments }
    | { tool: "capture_status"; arguments: StatusArguments }
    | { tool: "read_capture_image"; arguments: ImageArguments };
}
```

Queries have no mutation receipt. Their maximum deadlines are specified in §9; they do not inherit the existing generic 30-second query ceiling.

### 5.4 Coordinator action authority

Keep the existing authority lifetime, single-use, skew and epoch rules.

Add an optional `coordinator` field to `DesktopAuthority`, omitted for owner and wake actions:

```ts
{
  grant_id: string;
  binding_id: string;
  generation: number;
  caller_station: string;
  caller_session: string;
  caller_label: string;
}
```

`caller_label` (decision O12-4) is the deck's generated callsign of the caller session when it is 1 to 32 characters from `[a-z0-9-]`, otherwise the caller session ID. The target refuses a label longer than 64 characters or with a character outside `[A-Za-z0-9_-]` as malformed. The target uses it only for the fixed screenshot disclosure of §4.6. The hash covers it.

The window handle of a close is in the typed action (`preview_close`, §4.5), not in this context.

For coordinator actions, `action_hash` is the SHA-256 of canonical compact JSON with these fields:

```json
{
  "action": "<serialized typed DesktopAction object>",
  "coordinator": "<complete coordinator context object>",
  "monitor": "<existing target_monitor result, or null>",
  "station": "<target node fingerprint>"
}
```

Use the existing canonical ordering rules. Owner and wake canonical bytes remain byte-for-byte unchanged.

Removing or changing coordinator context must invalidate the hash. No coordinator authority is sent without `desktop_mcp.v1`; capture also requires `capture_mcp.v1`.

The relay accepts a coordinator actor only together with a private checked permit created by deck authorization. A bare grant ID supplied to a relay helper cannot authorize execution.

### 5.5 Live target policy

`desktop.policy_now` takes:

```ts
{
  purpose: "desktop" | "capture";
  monitor: string | null;
}
```

Return:

```ts
{
  allow_fleet_control: boolean;
  allow_desktop: boolean;
  allow_desktop_capture: boolean;
  monitor_allowed: boolean;
}
```

Values come from the target’s live policy hub. `monitor_allowed` is true for a null monitor, otherwise follows the current allowlist. This method does not return a desktop snapshot.

The handler rejects capture purpose without `capture_mcp.v1`.

### 5.6 Dispatch authority check

`desktop.authority_check` takes:

```ts
{
  operation_id: string;
  action_hash: string;
  phase: "execute" | "import" | "read";
  read?: ProtectedRead;   // required for phase "read", absent otherwise
}
```

`ProtectedRead` is the closed `read` union of §5.7. For phase `read`, `operation_id` is the `read_id`, `action_hash` is the `read_hash` of §5.7, and `read` is the read object that the target is about to run, exactly as the target parsed it. A `read` field with phase `execute` or `import`, or a missing `read` with phase `read`, is malformed and fails closed. For phases `execute` and `import`, they are the action operation ID and the §5.4 action hash.

The authenticated requesting station is the target. It cannot supply or substitute that identity.

The deck resolves the operation or temporary read permit, validates the target, hash, phase and current authority, asks the caller station for live policy, repeats deck-local checks, and returns:

```ts
{
  valid: boolean;
  reason: null | "authority_lost" | "authority_unknown";
}
```

For caller policy, add a P6 extension to the existing `coordinator.policy_now` request containing the expected `CoordinatorBinding`. Send that extension only with `desktop_mcp.v1`.

Its P6 answer includes:

```ts
{
  fleet_control: boolean;
  bound: boolean;
  binding_matches: boolean;
}
```

`binding_matches` compares the full expected binding with the station’s current binding. Existing ADR-008 requests and answers remain unchanged.

The five-second authority-check deadline includes the caller’s three-second policy deadline. Missing, malformed, unsupported or disconnected answers fail closed.

### 5.7 Protected reads

`desktop.coordinator_read` takes a closed request:

```ts
{
  read_id: string;
  read:
    | { kind: "info" }
    | { kind: "action_status"; operation_id: string }
    | { kind: "capture_status"; capture_id: string }
    | {
        kind: "capture_file";
        artifact_id: string;
        version: 1;
        offset: 0;
        length: number;   // the full committed byte count, at most 716,800
      };
  read_hash: string;
}
```

One request is one read and one chunk. A `capture_file` read has offset 0 and the full committed length. The target refuses any other offset, and a length that is not the committed byte count.

**Read-permit hash.** `read_hash` is the lowercase hex SHA-256 of canonical compact JSON (the existing canonical ordering rules of §5.4) with these fields:

```json
{
  "coordinator": "<complete coordinator context object of §5.4>",
  "kind": "read",
  "link": "<the deck's current link ID of the target>",
  "read": "<the serialized read object above>",
  "read_id": "<read_id>",
  "station": "<target node fingerprint>"
}
```

The deck sends `read_hash`; the target does not recompute it. The target passes it unchanged to `desktop.authority_check` with `operation_id = read_id`, `phase = "read"` and the `read` object that it is about to run. The target runs only that same parsed object after a `valid: true` answer.

Before the deck consumes the permit, it checks all of these, in this order:

1. A permit with this `read_id` exists, is not used and is not expired.
2. The authenticated requesting station is the permit's target station.
3. The link on which the check arrived is the link stored in the permit (the link that carried the read).
4. The received `read` object, serialized with the canonical rules, is byte-equal to the permit's read object.
5. The hash recomputed from the permit, that station and that link equals the received `action_hash`.

Any failure gives `valid: false` with `authority_lost` and consumes the permit, so a changed read cannot be tried again with the same `read_id`. The `"kind": "read"` field keeps a read hash apart from every action hash.

`read_id` is 128 random bits in lowercase hex (32 characters), unique per read. A permit authorizes exactly one `authority_check` with phase `read` and one answer; a second use is refused.

The deck registers a bounded temporary read permit before sending it. The permit binds:

- Full caller binding and required class.
- Target station and current link.
- Exact read kind and resource.
- Applicable capture monitor.
- Deadline.
- For image reads, fixed `capture.png`, version, hash and byte ceiling.

The target checks that permit through `desktop.authority_check`, then its own live gates immediately before returning data.

Reuse the existing desktop information, capture-operation lookup and artifact-file implementation internally. Do not fork their filesystem or tombstone logic.

The capture-file answer is at most **700 KiB** (716,800 bytes). Validate the returned offset (0), the decoded length (equal to the committed byte count), EOF and the SHA-256. A response larger than requested or inconsistent with the version fails before any byte is kept.

Temporary read permits expire with their query, are removed on completion or cancellation, and cannot authorize mutations.

## 6. Authorization order and execution boundary

### 6.1 Station MCP intake

In order:

1. Existing connection, header, body, Host, Origin and content-type checks.
2. Existing credential authentication.
3. Binding active and native identity known.
4. Tool belongs to the fixed granted catalog.
5. Strict argument validation.
6. Live caller `allow_fleet_control`.
7. Current required features and lifecycle watermark.
8. Existing endpoint concurrency/rate limits.
9. Forward once through the P6 path.

Never dispatch a desktop program on the caller station as a shortcut.

### 6.2 Deck decision

In order:

1. Validate method, envelope, frame command ID, incarnation and deadline.
2. Authenticate paired caller station and Coordinate scope.
3. Check grant belongs to caller station/session; validate full binding, generation, active state, idle expiry and absolute expiry.
4. Check deck `fleet_control` and `desktop_control`.
5. Check required class and target membership **before target/resource lookup**.
6. Check current caller and target features.
7. Resolve operation/resource, ownership, display ceiling and tombstones as applicable.
8. Obtain live caller policy and live target policy.
9. In one final decision transaction, repeat steps 3–7, check duplicate request identity, reserve budgets, and commit the operation/receipt.
10. Immediately before sending or committing a directive, repeat deck-local authority and feature checks.

A duplicate returns current status only after current authorization succeeds. It never preserves authority across revocation.

No database transaction is held across a network await.

### 6.3 Target execution

For `desktop.act`, retain existing authority admission and single-use consumption before desktop inspection.

Then:

1. Validate coordinator context and required features.
2. Check live target gates.
3. Complete bounded preparation and acquire necessary locks.
4. Obtain `desktop.authority_check`.
5. After that await, reread local settings and revalidate resource ownership.
6. Recheck action hash, epoch, authority deadline and existing monotonic lifetime.
7. Start the authorized side effect without another unrelated asynchronous preparation step.

The final check belongs at the actual side effect: browser spawn, notification send, compositor write or `grim` spawn. A check performed only before directory preparation or another awaited operation is insufficient.

Multi-step actions recheck before later externally visible writes. Ownership-based cleanup may finish after cancellation, but must not start new user-requested work.

For screenshots, repeat remote authority and local capture checks before import, outside the atomic commit itself.

### 6.4 Limits of revocation

Revocation stops new decisions and effects whose final dispatch check has not completed. There remains a bounded interval between the remote answer and the local side effect.

Document these intervals honestly:

- Caller-policy sampling to final deck answer: within the five-second check deadline.
- Final deck answer to target effect: local dispatch work; no unrelated await or queue.
- A local process or atomic filesystem/DB operation already started may finish.

Revocation does not close an already opened preview, erase an artifact, undo a delivered notification or remove a provider’s received image. The owner can close previews and delete artifacts through existing board controls.

## 7. Idempotency, durable state and recovery

### 7.1 Request identity

Use:

```text
(grant_id, SHA-256(request_id))
```

as the coordinator request key.

The payload hash covers normalized tool name, selected target and complete validated arguments except `request_id`.

- Same key and same payload: return current recorded state.
- Same key and different payload: `request_id_conflict`.
- Different IDs are distinct requested actions.

Do not log the raw agent-selected request ID. It may contain sensitive text despite its grammar.

### 7.2 Mutation record

Before sending, commit:

- Deck-generated operation ULID.
- Request-key digest and payload hash.
- Full caller/binding provenance.
- Target station and required class.
- Action kind and authority hash.
- Validated resource identifiers.
- Budget charges.
- State and fixed result/error fields.

Do not retain URL or notification text in the durable operation record. The live task holds the typed payload; restart does not reconstruct and dispatch it.

States are:

```text
prepared → attempted → done | failed | unknown
```

Commit `attempted` before calling the link. A crash after that commit never causes an automatic send.

A prepared operation abandoned by process loss becomes failed without dispatch. An attempted operation with no definitive result is unknown and is reconciled only by lookup.

### 7.3 Target records

For coordinator desktop mutations, add a compact durable target operation record keyed by deck fingerprint and operation ULID.

Persist the claim before the user-visible effect. Never execute that operation again after restart or reconnect. A lost non-capture completion may remain unknown.

Capture operations reuse P5a’s durable operation table and atomic artifact commit. Extend its hash and provenance checks rather than introducing a second capture ledger.

Retain compact terminal identities so receipt pruning cannot permit another effect.

### 7.4 Capture recovery

Reuse P5a:

- Recovery at deck start and target link-up.
- Every 30 seconds while unresolved.
- Lookup only; never another screenshot.
- Permanent deletion tombstones win.

Internal reconciliation may finish indexing after grant revocation. It does not deliver data to a revoked caller.

A capture saved on the station but not yet indexed returns `pending`, not screenshot failure. A failed desktop refresh cannot change a committed capture into failure.

Client cancellation stops waiting and future unstarted work; it must not abandon durable capture bookkeeping.

## 8. Budgets and admission limits

All figures in this section require **OD-6**.

### 8.1 Durable per-grant budgets

| Field | Default | Allowed range | Charge |
|---|---:|---:|---|
| `desktop_cap` / `desktop_used` | 300 | 0–5000 | Every newly accepted mutation, including screenshots and wall directives |
| `capture_cap` / `capture_used` | 20 | 0–5000 | Every newly accepted screenshot; also consumes one desktop action |
| `image_byte_cap` / `image_byte_used` | 67,108,864 bytes | 0–1,073,741,824, in 1 MiB units | Reserve and permanently charge 716,800 bytes (700 KiB) once before each image fetch |
| Distinct well-formed mutation attempts | 10,000 | Fixed | New request keys that reach the authorized decision stage, accepted or rejected |

The final row bounds durable request-key/rejection storage. Once reached, only known-key status/replay checks remain possible; a new grant requires the owner.

Counters are lifetime values, committed atomically with acceptance. They survive restart, renewal and receipt pruning. They are not refunded for dispatch failure, cancellation, ambiguous results or deletion.

Image reads charge their full reservation even when the PNG is smaller or delivery fails. Known oversized artifacts are rejected before reservation.

### 8.2 Rates and concurrency

| Resource | Limit |
|---|---:|
| New mutations per grant | 30 in any rolling 60 seconds |
| `notify` per caller session, across grants | 6 in any rolling 60 seconds |
| New screenshots per grant | 2 in any rolling 60 seconds |
| Metadata/status queries per grant | 60 in any rolling 60 seconds |
| Image fetches per grant | 6 in any rolling 60 seconds |
| Mutations in flight per grant | 2 |
| Image fetches in flight per grant | 1 |
| P6 calls/read permits in flight on the deck | 64 |
| Image assemblies in flight on the deck | 4 |
| Captures running per target station | Existing P5a limit: 1 |

Apply the existing MCP limits as well: eight concurrent requests and twenty requests per second per binding.

Reject immediately when a slot is unavailable; do not create an unbounded queue.

Persist charged rolling-window timestamps with acceptance. Restart or renewal must not reset the applicable window. Use monotonic elapsed time in-process and conservative persisted accounting across restart; backward clock movement must not create new capacity.

Automatic capture disclosure notifications do not consume the user `notify` rate; they cannot be requested independently.

### 8.3 P5a limits

Keep unchanged:

- PNG ≤ 33,554,432 bytes.
- Each dimension 1–8192.
- Product ≤ 16,777,216 pixels.
- Station capture quota 4 GiB.
- Full 32 MiB reservation before spawn.
- Free space ≥ 2 GiB plus reservation.
- Copy buffers ≤ 65,536 bytes.
- Deck capture cache ≤ 256 MiB logical payload.

The smaller MCP image limit affects delivery only. It never causes a new capture or silent resize.

## 9. Deadlines

All deadlines are total budgets from entry to the named stage, not fresh budgets after every await.

| Stage | Deadline |
|---|---:|
| Caller-policy request | 3 s |
| Target-policy request | 3 s |
| Target → deck authority check, including caller policy | 5 s |
| Authority execution start | Existing 10 s, epoch and skew rules |
| Screenshot context/monitor/lock admission | Existing 5 s |
| `grim` | Existing 10 s |
| Station screenshot | Existing 30 s |
| Deck wait for screenshot | Existing 35 s |
| Deck screenshot handler/indexing | Existing 40 s |
| Caller MCP screenshot response | 45 s |
| Station preview action | Existing 90 s |
| Deck preview wait | 95 s |
| Deck preview handler | 100 s |
| Caller MCP preview response | 105 s |
| Other station desktop actions | Existing 30 s |
| Deck wait for those actions | 35 s |
| Deck handler | 40 s |
| Caller MCP response | 45 s |
| Fresh desktop information: station/deck/MCP | 30 / 40 / 45 s |
| Capture result lookup: station/deck/MCP | 5 / 10 / 15 s |
| Other action status: deck/MCP | 10 / 15 s |
| One artifact-file request | 20 s |
| Complete image-file acquisition | 60 s |
| Deck image query, including final authorization | 65 s |
| Caller MCP image response, including encoding | 70 s |

Authority is issued after the deck’s awaited intake checks. Preparation and dispatch checks still consume its ten-second start lifetime; expiry refuses execution.

The P6 forwarder must bypass the generic coordinator query timeout and outbox paths where they conflict with this table.

Timeout never initiates another mutation. A filesystem sync or atomic commit already underway may finish after the waiter expires; the owned operation records the result.

## 10. Version behavior

- Old grants acquire no new classes or image-reading authority.
- Grants without either new class serialize without P6 fields; retain byte fixtures for existing v1 and v2 start commands.
- P6 grant creation fails with stored `unsupported` if the caller or any target lacks a required feature or a current link.
- Unknown classes fail closed on deck and station.
- Optional P6 authority/provenance fields are sent only to negotiated peers.
- Owner and wake action hashes and messages remain unchanged.
- Existing owner captures retain their old wire representation.
- A reconnect or downgrade is checked on every call, send, dispatch and read. Never strip coordinator authority to use a legacy owner action.
- Already listed tools remain listed after downgrade and fail explicitly.
- An older deck does not accept the new methods; the new caller reports `desktop_unsupported` and does not execute locally.
- An older caller station cannot receive a P6 grant.
- A station supporting P5a but lacking `capture_mcp.v1` remains available for owner screenshots only.
- Unknown coordinator provenance is never interpreted as owner provenance.
- Image-provider compatibility is proved by opt-in Claude and Codex smoke tests for the already supported MCP protocol versions. No new MCP protocol version is introduced.

Advertise the two P6 feature strings in the owner features endpoint when implemented. Readiness uses current negotiation, never a version-number guess.

## 11. Errors and exact texts

Authentication, HTTP admission and JSON-RPC framing retain ADR-006 behavior.

For a recognized tool execution error, return `isError: true` with one text block containing JSON:

```json
{
  "error": "<code>",
  "message": "<fixed text>"
}
```

Where known and authorized, also include the deck-generated operation ID or capture ID. Never include raw wire messages, stderr, URLs, supplied text or paths.

Pending/unknown durable capture states are normal status results, with the explanatory text below. They are not claims that no screenshot occurred.

### 11.1 P6 errors

| Code | Exact text |
|---|---|
| `not_bound` | `This coordinator is not bound to a provider session.` |
| `not_granted` | `This grant does not allow this desktop operation.` |
| `binding_stale` | `The coordinator binding has changed. Start a new authorized request.` |
| `elevation_required` | `Owner elevation is required for desktop or capture authority.` |
| `unsupported` | `The selected peers do not support this desktop grant.` |
| `incarnation_mismatch` | `The deck identity has changed. Reconnect before trying again.` |
| `expired` | `The desktop request expired before it was accepted.` |
| `invalid_params` | `The desktop tool arguments are not valid.` |
| `request_id_conflict` | `This request ID was used for another desktop request.` |
| `desktop_cap_reached` | `This grant has used its desktop action limit.` |
| `capture_cap_reached` | `This grant has used its screenshot limit.` |
| `image_budget_reached` | `This grant has used its screenshot image-read budget.` |
| `request_budget_reached` | `This grant has used its desktop request limit.` |
| `rate_limited` | `This grant's desktop rate limit has been reached. Try again later.` |
| `busy` | `The desktop operation cannot start because its capacity limit is in use.` |
| `authority_lost` | `Desktop authority was withdrawn before this operation could continue.` |
| `authority_unknown` | `Desktop authority could not be checked. No further desktop work was started.` |
| `fleet_control_off` | `Fleet control is off on this deck.` |
| `desktop_result_too_large` | `The desktop information exceeds the tool response limit.` |
| `image_too_large` | `The screenshot exceeds the 700 KiB MCP image limit. Open the artifact on the board.` |
| `image_invalid` | `The screenshot artifact could not be verified.` |
| `unknown_artifact` | `No such capture artifact is available to this grant.` |
| `desktop_window_not_found` | `No window opened by this grant matches that handle.` |
| `desktop_display_not_found` | `No wall available to this grant matches that display.` |
| `desktop_reference_not_granted` | `This grant cannot show that reference on the wall.` |
| `desktop_read_timeout` | `The desktop data did not finish loading in time.` |

The capacity `busy` text above is for non-capture admission. Preserve the existing capture-specific `busy` text below.

Unknown, foreign, tombstoned and otherwise inaccessible artifact identities use the same `unknown_artifact` response. Unknown and foreign window/display handles likewise share their respective response.

### 11.2 Existing relay errors

Preserve:

| Code | Exact text |
|---|---|
| `desktop_unsupported` | `This station's build has no desktop control.` |
| `desktop_forbidden` | `The station refused the desktop action.` |
| `invalid_desktop_action` | `The desktop action is not valid.` |
| `desktop_not_found` | `The target of the desktop action was not found.` |
| `desktop_conflict` | `The desktop action conflicts with the current state.` |
| `desktop_failed` | `The desktop action failed.` |
| `desktop_overloaded` | `The station is busy; try again later.` |
| `desktop_timeout` | `The station did not answer in time; the action may have run.` |
| `station_unreachable` | `The station is not connected.` |
| `desktop_control_off` | `Desktop control is off on this deck.` |

### 11.3 Existing capture errors

Preserve the allowlisted P5a codes and texts:

| Code | Exact text |
|---|---|
| `busy` | `A capture is already running.` |
| `disk_full` | `The station has too little free space for a capture.` |
| `capture_quota` | `This capture would exceed the station's 4 GiB capture limit. Delete some captures first.` |
| `capture_too_large` | `The screenshot exceeds 32 MiB.` |
| `capture_dimensions` | `The monitor exceeds the screenshot size limit.` |
| `capture_failed` | `The screenshot tool failed.` |
| `capture_publish_failed` | `The screenshot could not be saved as an artifact.` |
| `screen_locked` | `The screen is locked or its lock state is unknown.` |
| `capture_id_conflict` | `This capture ID was used for another request.` |
| `capture_interrupted` | `The screenshot was interrupted before it was saved.` |

Missing `grim`: `desktop_unsupported`, with the capture-specific fixed text:

> The screenshot tool grim is not installed.

Unknown capture status includes:

> The screenshot result is unknown. Check the artifact list before taking another screenshot.

Pending indexing includes:

> The screenshot was saved on the station. The artifact list is still updating.

A station `forbidden` is never explained as a specific setting value unless that setting was directly checked by the trusted handler.

## 12. Audit and owner visibility

Use deck-generated identifiers and fixed codes only.

| Audit action | Rows |
|---|---|
| `coordinator.grant_create` | Existing creation row, including selected classes, target IDs and configured caps |
| `coordinator.grant_revoke` / existing renewal action | Existing lifecycle rows |
| `desktop.info` | One P6 query outcome |
| `desktop.notify` | Attempt and terminal outcome |
| `desktop.preview_open` | Attempt and terminal outcome |
| `desktop.window_close` | Attempt and terminal outcome |
| `display.directive` | Coordinator directive acceptance/refusal; accepted row includes generation and referenced IDs |
| `desktop.capture` | Start, end or refusal, correlated by capture ID |
| `desktop.action_status` | Status-query outcome |
| `desktop.capture_status` | Capture-status-query outcome |
| `desktop.capture_read` | Image-read attempt and outcome |

Each row contains, when established:

```text
actor = coordinator
grant ID
binding generation
caller station/session IDs
target station ID
operation or capture ID
validated monitor/display/window/artifact identifiers, as applicable
stage
result code
charged counters or byte reservation, as applicable
```

Never include credentials, credential digests, authority objects, native provider IDs, raw request IDs, URLs, notification text, screenshots, base64, arbitrary titles, paths or stderr.

For a refused unresolved identifier, record the code rather than copying that supplied identifier. Do not log an entire request or response for diagnosis.

Acceptance/refusal audit records commit with the relevant decision. An operation cannot start if its required decision record cannot be committed. Completion records are durable and idempotent by operation and stage.

The board shows:

- Desktop/capture grant chips and grant stations.
- Expiry, action usage, screenshot usage and image budget.
- Recent operations and unresolved outcomes.
- Revoke and existing kill switches.
- Artifact links through owner routes.

Capture grant disclosure:

> This agent can capture the whole selected monitor, including other applications, notifications and private information visible there. Screenshots are stored as artifacts. Images the agent reads are sent to its provider conversation. Deleting an artifact does not remove downloaded copies or provider conversations.

Preview grant disclosure states that opening web content can make network requests.

No approval or setting change is delegated to a desktop notification.

## 13. Exposure contract

A P6 result may contain only:

- The selected target’s projected desktop information.
- A caller-owned operation’s state and fixed code.
- A window handle for this caller’s preview.
- A directive generation.
- This grant/caller’s capture reference and allowlisted metadata.
- The single authorized PNG through `read_capture_image`.

It must never contain:

- Another request’s result because IDs collided across stations or grants.
- Another caller’s capture, including an owner capture.
- Another station’s artifact returned by an artifact-ID-only lookup.
- Raw desktop snapshots attached to mutation or capture results.
- Window titles, process lists, compositor addresses or environment values.
- Display credentials, pairing tokens, owner cookies, share links or capability URLs.
- Arbitrary file bytes or an arbitrary artifact filename.
- Raw adapter errors, command output or screenshot bytes in logs/audit.

Under **OD-5**, “another station” means a station other than the explicitly authorized selected target. Remote capture is intentional disclosure of that target’s composed pixels.

A monitor screenshot cannot exclude another application’s visible data on that monitor. Do not claim session-level pixel isolation or secret detection.

## 14. Implementation boundaries

No new third-party dependency.

Reuse existing serde/JSON, SQLite, ring, Tokio, HTTP, base64, rustix, desktop runner, screenshot validation, retained-descriptor import and artifact-file facilities.

Split implementation into focused modules for:

- MCP schemas and result encoding.
- Deck authorization and operation storage.
- Relay permits and dispatch checks.
- Protected reads and image assembly.
- Coordinator directive authorization.
- Capture provenance and recovery.
- Tests.

Every changed source module remains below 800 lines. Refactor existing large modules before extending them.

Required integration changes include:

- Grant and tool classes, elevated board fields, counters and migrations.
- New method/feature tables and dedicated forwarder path.
- Coordinator-aware action hashing and target dispatch.
- Durable non-capture operation claims.
- Window ownership by station/session/grant.
- Directive write authorization **and live delivery authorization**.
- Capture caller provenance throughout station commit, deck indexing and recovery.
- Bounded MCP image content generation.
- Error allowlists and redaction.
- Parent phase table, ADR cross-references, `PARITY.md` and `SECURITY-CHECKLIST.md`.

P6 must not silently depend on recordings, input tools, a station-local setup command or a new desktop capture backend.

## 15. Tests: the four proofs

### 15.1 Behavior

1. **Catalog/schema matrix:** each class alone, both classes, neither, pending binding, renewal and switches off. Validate every schema, defaults and unknown-field rejection.
2. **Grant creation:** owner elevation, target station list, caps and expiry; no transitive creation or ordinary `own` binding authority.
3. **Targeting:** omitted station means caller; explicit authorized remote station operates there; headless caller can operate a remote desktop.
4. **Notifications:** exact argv, option-looking title/body, markup removal, prefix and final scalar limits; no critical urgency or click action.
5. **Preview:** initial-host allowlist, IPv6, ports, exact tailnet hostname, fresh profile and returned opaque handle.
6. **Window ownership:** same session ID on two stations, different grants on one station, renewed binding, unknown handle, substituted handle (another caller's `window_id` in a valid authority gives `desktop_window_not_found`), close after a deck restart (succeeds), compositor restart, event gap and reused address; a coordinator authority with the legacy `window_close` is refused.
7. **Wall:** own/lineage session and artifact references; same-grant capture; ceiling narrowing; no binding change, URL, batch or unrelated reference.
8. **Screenshot:** one selected monitor, fixed disclosure notification, P5a process/import behavior and correct remote caller provenance.
9. **Image:** an exact-cap PNG (716,800 bytes) succeeds through both real link hops (target → deck → caller station) and the MCP result; cap+1 fails with `image_too_large`; metadata and image blocks match the one committed artifact; oversized delivery does not invalidate the artifact.
10. **Accounting:** concurrent final-budget-slot races; duplicates charge once; screenshots charge both counters; renewal, restart, pruning and deletion do not refund usage.

### 15.2 Failure

11. Revoke, expire, renew generation or turn either deck switch off at: intake; after remote policy; after acceptance; before send; during target preparation; before the effect; before screenshot import; during image assembly; before MCP emission.
12. Hold back caller policy publication while turning caller `allow_fleet_control` off. The live round trip refuses execution despite unchanged deck projections.
13. Turn target `allow_fleet_control`, `allow_desktop`, capture permission or monitor allowlist off after preparation; the final local read refuses work.
14. Caller/target disconnect, timeout, malformed policy answer and authority-check overload all fail closed.
15. Change the grant or deck switch during the caller-policy await. The second deck-local check refuses it.
16. Expire the ten-second token while waiting on preparation or locks. No user-requested effect starts.
17. Crash at prepared/attempted/send/effect/completion boundaries. No automatic resend; target durable claims prevent another execution.
18. Reuse a request ID with different URL, monitor, title, target, display or reference. Return conflict without effect.
19. Preserve all P5a crash, fsync, disk-full, lock-state, lost-response and delayed-commit proofs for coordinator provenance.
20. Delete during reconciliation, metadata read, cache hit, station file read and final image emission. No resurrection or unchecked delivery.
21. Exercise every deadline with controlled clocks and blocked stages. No timeout path starts another screenshot or queues a mutation.
22. Exercise rate and capacity boundaries without unbounded waiting tasks; restart and backward-clock tests do not replenish budgets.

### 15.3 Exposure

23. Use distinct sentinel data for caller, selected target, unrelated station, owner capture and another coordinator capture. Only expressly allowed fields/bytes appear.
24. Give identical artifact IDs on two stations. Every lookup and cache key includes the stable station ID.
25. Guess foreign request IDs, capture IDs, window handles, displays and artifact IDs. Responses do not reveal their existence.
26. Put sentinel secrets in raw `DesktopInfo`, wall addresses, monitor descriptions, notification text, URL, stderr and native identity. They never appear in P6 responses, audit or logs.
27. Test malformed/oversized chunk responses, base64 expansion, offsets, EOF, hash mismatch, wrong MIME/kind/version/file and substituted provenance. No partial image reaches MCP. Read-permit fixtures: a fixed byte fixture of the `read_hash` input and output; the authority check refuses a changed read kind, resource, offset, length, link, station or coordinator context, and a second use of a `read_id`.
28. Test revocation and deletion immediately before response emission, including cache-hit paths.
29. Restore a coordinator directive after revoke, generation change, task/lineage change, ceiling change and target-policy loss. No stale content is delivered.
30. Ordinary artifact publication cannot forge capture provenance or gain `read_capture_image` eligibility.
31. Input, recording, terminal and owner-only desktop actions remain unavailable through P6.
32. Same-UID limitations and composed-pixel disclosure are stated accurately; tests do not claim isolation the process model cannot provide.

### 15.4 Version

33. Feature combinations across old/new caller, deck and target; P5a-only target; downgrade after creation, acceptance and preparation.
34. Byte fixtures prove unchanged legacy grant specs, owner/wake hashes, owner capture provenance and non-P6 policy requests.
35. Unknown classes/context/provenance fail closed; coordinator authority is never stripped for compatibility.
36. Fixed catalog survives reconnect and renewal; unsupported calls return the specified error.
37. Each of the three supported MCP protocol versions (`2025-11-25`, `2025-06-18`, `2025-03-26`; `coordinator/jsonrpc.rs`) encodes text and image results correctly within the total response limit.
38. Opt-in Claude and Codex smoke: authenticated discovery, remote notification/preview, screenshot artifact, image read, revoke and denial of the next action. All enablement and approvals occur from the board.

Run targeted checks through:

```sh
umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh cargo check -p <changed-crate>
```

Run relevant targeted nextest and web tests through the same queue. The implementation coordinator runs `make check`. No build or test was executed for this read-only proposal.

## 16. Acceptance

Before implementation:

1. Record the accepted OD-1 through OD-7 choices.
2. Update the parent’s P6 scope and superseded tool names.
3. Obtain adversary DESIGN REVIEW PASS for this complete contract.

Before merge:

1. Complete the four proofs and required project checks.
2. Obtain adversary PRE-PR review.
3. Record remaining live-smoke limitations without claiming production acceptance.

The focused authority/exposure review used the repository’s [security-audit skill](../../../.agents/skills/security-audit/SKILL.md).
