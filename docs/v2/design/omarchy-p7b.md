# P7b: coordinator pointer click and the `any_window` input scope

- **Status:** Proposed contract (architect, 2026-10-19), from the advisor answer `advisor-p7b-answer.md` against v2 HEAD `f2286e3e`. The architect accepts P7b-OD1 to P7b-OD6 as architect decisions (O12-24; the owner may change any of them). DESIGN REVIEW required before implementation; PRE-PR required before merge; live smoke required before operational reliance (both capabilities stay experimental and off by default).
- **Parent:** `omarchy-p7.md`.
- **Prerequisite:** accepted and built P7 (DL-245, DL-246).
- **Scope:** coordinator tool `pointer_click`, and the `any_window` input scope with the tool `window_list`.
- **Still deferred:** right and middle buttons, double click as one call, scrolling, dragging, held keys or buttons, ordinary-session grants.

This annex is proposed contract content for adversary review. It does not change the accepted status of P7. Both capabilities share this file. Their permissions, features and operational acceptance stay independent.

## 1. What is wrong with the first options

The advisor recommends option 1 for the click and option 2 for `any_window`, with handles. These points apply to those options.

1. **A click has two input stages.** `movecursor` is already an input effect. It needs authorization and destination checks before it runs. A check only after the movement is too late.
2. **Focus does not establish the click recipient.** `wlrctl` creates a virtual pointer and emits button press and release events without a window target. After the movement, verify the actual cursor position, the current geometry, the focus and the obstructions before the button starts. Device creation and event delivery still add races. Reference: [wlrctl pointer implementation](https://sources.debian.org/src/wlrctl/0.2.2-2/pointer.c/).
3. **Protocol presence is not enough compatibility evidence.** The inspected `wlrctl` version binds virtual-pointer manager version 2. It chooses a discovered seat, and it has no command-line seat selector. The installed version, the compositor instance and the seat behavior need verification. Reference: [wlrctl connection implementation](https://sources.debian.org/src/wlrctl/0.2.2-2/main.c/).
4. **`any_window` is a large expansion of authority.** It reaches existing authenticated applications, terminals and the visible windows of other agents. If the output has no titles, the disclosure by enumeration is smaller. Screenshots and application interaction can still reveal personal data.
5. **Ordinary windows have no P6 launch provenance.** Their handles need a separate, bounded registry with lifetime tracking. An address and a class alone do not show that a window is the same window after a close and reopen or after a lost event. The existing registry registers coordinator previews from station launches. Reference: [events.rs](/home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/crates/fleetdeck-station/src/desktop/events.rs:289).
6. **Input handles must not widen other tools.** A handle from `window_list` must never authorize `window_close`. It must never confer preview ownership. It must never become a raw-address escape hatch.
7. **The P7 sign-out is necessary but limited.** It protects the browser sessions of this deck at the recorded machine addresses. It does not protect the approval UI of another service, another deck, a network path that is not reported, or the authenticated applications of the owner.

The alternative that uses the focused window makes the target depend on ambient focus and gives no durable identity to validate. Reject it.

## 2. Owner decisions

The architect accepts each recommendation below as an architect decision (O12-24, 2026-10-19), as for P7 (O12-23). The owner may replace any of them.

| ID | Decision | Recommendation |
|---|---|---|
| **P7b-OD1** | Click scope. | Permit one left-button press and release pair at a checked, window-relative point. |
| **P7b-OD2** | Scope of input targets. | Permit `any_window` through explicitly listed, caller-bound handles. There is no implicit focused target. |
| **P7b-OD3** | What `window_list` returns. | Return geometry and opaque handles, without window titles, classes or focus history. |
| **P7b-OD4** | Grant and station opt-ins. | Require independent elevated grant opt-ins and independent station gates for clicks and for the broader scope. |
| **P7b-OD5** | Targeting guarantee. | Accept checked, best-effort targeting with the P7 sign-out and the expanded disclosures of this annex. |
| **P7b-OD6** | Order of live smoke and implementation. | Permit implementation before live smoke. Keep both capabilities experimental and off by default. |

**P7b-OD2 includes interactive terminal windows and signed-in browser windows.** The broader scope reaches them. P7b adds no command-execution interface. But text typed into a terminal can execute commands. A signed-in browser window can act with the logged-in accounts of the owner. If this authority is not acceptable, defer `any_window`. A class blacklist cannot reliably remove this authority.

Inherited decisions from P7 stay binding: coordinator grants only; grant stations bound the physical desktop targets; agents run as the owner UID; the wall is read-only; management is remote from the board; no desktop notification grants authority.

## 3. Grants, settings and migration

### 3.1 Grant fields

Extend the owner grant request and the forwarded coordinator specification:

```ts
{
  allow_input?: boolean;                 // existing; default false
  input_scope?: "own_windows" | "any_window";
  allow_click?: boolean;                 // new; default false

  input_cap?: number;                    // existing; board → deck only
  input_character_cap?: number;          // existing; board → deck only
}
```

Validation and lifecycle:

- Input still requires the explicit classes `desktop`, `capture` and `input`.
- `allow_click: true` requires `allow_input: true`.
- Enabled input with an omitted scope defaults to `own_windows`.
- An explicit scope or caps without input allowance stay invalid.
- Accept exactly `any_window`. Do not add an `any` alias.
- Existing grants keep their allowance, scope, limits and usage. They get `allow_click = false`.
- The click allowance and the scope are immutable authorization facts of that grant. To add either, start or resume with a new elevation and a newly established provider catalog.
- Renewal and "Change limits" cannot add either permission. They cannot reset the counters.
- New grants do not inherit P6 previews or listed-window handles.

### 3.2 Station settings

Add two station settings:

```text
allow_desktop_click = false
allow_desktop_input_any = false
```

Keep `allow_desktop_input` as the prerequisite. To enable either new setting, the owner needs elevation through the board. To disable, the owner needs none.

Every input action under the broader scope needs the broader-scope gate. This includes an action aimed at an owned preview. Do not fall back to a preview-only execution when that gate closes. A click needs the click gate in addition.

### 3.3 Migration (V38)

Use the next forward migration. This is **V38 at this HEAD, subject to allocation**. The V37 CHECK on `tokens.input_scope` permits only `own_windows`, so a change of the application enum is not enough. Reference: [coordinator_input_schema.rs](/home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/crates/fleetdeck-deck/src/store/coordinator_input_schema.rs:23).

The migration must:

- Rebuild `tokens` with the extended scope CHECK and with `allow_click` that defaults to zero.
- Keep every token field, constraint, index and relationship.
- Extend the operation tool CHECK with `pointer_click`.
- Extend the rate buckets for bounded `window_list` reads.
- Keep the operations, the request identities, the rejection identities, the recording fields, the counters, the exposure snapshots and the timestamps.
- Pass the foreign-key checks and the fresh-versus-migrated schema checks.

Do not edit V37. Do not repeat its session deletion. Listed-window handles stay in memory only.

## 4. Features and compatibility

Add two independent protocol features:

```text
desktop_click_mcp.v1
desktop_input_any.v1
```

Both depend on P7 and on its existing P6 and capture prerequisites.

| Enabled authority | Required additional features on the caller and on every grant target |
|---|---|
| Clicks in previews | `desktop_click_mcp.v1` |
| Broader keyboard and movement | `desktop_input_any.v1` |
| Broader clicks | Both |

A feature describes implemented protocol support. It does not attest that an executable is installed. It does not attest a successful live smoke.

Keep the provider catalog fixed at process start:

- `pointer_click` appears only with input allowance, click allowance and negotiated support.
- `window_list` appears only with `any_window` allowance and negotiated support.
- The existing input tools stay listed according to their enabled binding.
- Runtime settings, executable availability and lock state affect calls only.
- A downgrade causes an explicit refusal. It never strips the scope or the coordinator authority.

Derive each additional internal tool class locally. Public class parsing must reject an attempt to supply one directly.

## 5. Catalog and exact JSON schemas

Add exactly these tools, under the conditions of §4:

```text
pointer_click
window_list
```

`scroll` stays unavailable. Reuse the exact definitions `station`, `request_id`, `window_id`, `x` and `y` of P7 §4. Copy each referenced definition into the root of each published schema.

### 5.1 `pointer_click`

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": ["request_id", "window_id", "x", "y"],
  "properties": {
    "request_id": {"$ref": "#/$defs/request_id"},
    "station": {"$ref": "#/$defs/station"},
    "window_id": {"$ref": "#/$defs/window_id"},
    "x": {"type": "integer", "minimum": 0, "maximum": 32767},
    "y": {"type": "integer", "minimum": 0, "maximum": 32767}
  }
}
```

There is no button argument. The call makes exactly one left-button press and release pair. There is no count, duration, modifier, held-button state, scrolling or dragging.

An application can interpret two separate calls as a double click. Physical modifiers, the button state and the compositor bindings can also change the effect. Do not promise one application action.

### 5.2 `window_list`

```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "station": {"$ref": "#/$defs/station"}
  }
}
```

`window_list` is a protected read. It has no mutation `request_id`. It has no title filter, no address selector, no scope override and no pagination.

### 5.3 Existing input schemas

The existing input schemas stay unchanged. Under `any_window`, their `window_id` can identify either an eligible caller-owned preview or a handle that `window_list` issued.

All existing grammar, Unicode, byte, unknown-field and null-versus-omission rules stay binding.

## 6. What `window_list` may reveal

Return this closed projection:

```ts
{
  station: string;
  monitors: Array<{
    name: string;
    x: number;
    y: number;
    width: number;
    height: number;
  }>;
  windows: Array<{
    window_id: string;
    rect: {
      x: number;
      y: number;
      width: number;
      height: number;
    };
    monitors: string[];
    focused: boolean;
    expires_in_ms: number;
  }>;
}
```

The coordinates are global logical coordinates. The monitor geometry gives the reference that relates a window to a permitted monitor screenshot.

Limits and rules for v1:

- At most 64 windows, 64 monitors and 16 KiB of serialized output.
- Checked, bounded geometry with positive dimensions.
- Only mapped, non-hidden clients on workspaces that are now displayed, ordinary or special.
- Every monitor that an eligible window overlaps must be capture-permitted.
- Exclude the detected board and wall windows before handles are issued.
- Do not switch workspaces, raise windows or focus anything.
- Return no titles, initial titles, classes, application labels, URLs, PIDs, native addresses, workspace names, focus history or other grant identities.
- Return no counts and no exclusion reasons for omitted windows.
- An oversized result fails with `desktop_result_too_large`. Do not truncate.

The station can inspect titles and classes for a short time inside its barrier. They must not enter responses, durable records, audit or logs.

Geometry and focus are also desktop metadata. Disclose their release. Do not claim title confidentiality after the owner authorizes monitor screenshots and broader application interaction.

### 6.1 Protected read

Use the P6 protected-read machinery with a new read kind, gated by a feature:

```ts
{ kind: "input_windows" }
```

The one-use permit binds the full coordinator, the target, the link, the exact read object, the required scope and the deadline. Check the live input, broader-scope, capture, monitor and unlocked-state gates before the inspection and before the projection is returned. Keep the final deck delivery pass and the caller `Rechecked` behavior.

Apply the existing query limits, and in addition:

- 10 list reads per rolling 60 seconds, for each grant and caller session.
- One list read in flight for each target station. A second read gets `busy` at once.
- The existing protected-read deadlines and the deck-wide capacity.

Persist the rate timestamps. A list read uses no input, character or desktop-mutation budget. It sends no disclosure notification. It causes no board sign-out.

## 7. Listed-window handle contract

Create a separate input-target registry. A listed handle has 128 random bits, encoded as the 32 lowercase hexadecimal characters of P7.

Bind each handle to:

```text
deck fingerprint
grant ID
caller station/session
binding ID and generation
target/compositor identity
event-stream epoch
station-local window incarnation
native address and recorded class
monotonic expiry
```

Rules:

- The maximum lifetime is 60 seconds from issuance. A new listing does not extend it.
- At most 64 handles for each binding and target, and 1,024 on the whole station.
- Reuse an unexpired handle only for the same bound window incarnation.
- When the capacity is full, fail at once. Never evict the live handles of another caller to make room.
- Invalidate a handle on close, observed address reuse, identity mismatch, event gap, reconnect, compositor change, target restart or change of the binding generation.
- After a renewal that changes the binding generation, a new listing is required. The P6 preview retention is unchanged.
- Track the open and close lifetimes of ordinary windows, including windows that the enumeration observes first.
- Detect registry changes while a listing is collected. Fail. Do not sample again and again until the observations pass.
- Never recover an invalid handle by a search for similar metadata.

Event observation is not atomic with the compositor state. Document the remaining race between event lag and address reuse.

### 7.1 Input eligibility only

An input handle confers **input eligibility only**. P6 `window_close` continues to resolve only through the owned-preview registry. A listing of the visible preview of another coordinator does not reveal its P6 handle. It does not give close authority.

## 8. Barrier and focus rules

For both scopes:

- Apply every board-class, title, known-origin and trusted-provenance rule of P7.
- A detected board match overrides input eligibility.
- Validate before focusing, and again in the final observations.
- Require a known unlocked state and a current, permitted monitor coverage.
- Unknown, expired and inaccessible handles get identical responses.
- A foreign active window produces a generic focus loss, without its identity.

For listed windows:

- Do not fabricate preview provenance.
- Consult trusted wall and window records where they exist.
- Treat the absence of board evidence as incomplete detection.
- Focus exactly the address of the registry, once, after revalidation.
- Do not move windows, switch workspaces or substitute a focused window.

For pointer actions:

- Keep the window-relative logical coordinates of P7, the checked conversion and no clamping.
- Require the destination on an enabled, permitted monitor.
- Refuse an overlap with another visible mapped client.
- Refuse an overlap with **`top` and `overlay`** layer surfaces.
- Keep the accepted treatment of `background` and `bottom`.
- Malformed layer, visibility, geometry or monitor data fails closed.

Menus and popups that client enumeration does not show are not independent targets. Rectangle checks do not prove which subsurface receives the event.

## 9. Wire and authority changes

### 9.1 Action variants

Keep all existing P7 action bytes. Add closed variants for:

```text
input_pointer_click

input_any_key_press
input_any_type_text
input_any_pointer_move
input_any_pointer_click
```

The broader variants carry the same relevant validated arguments, including the opaque handle. Every input call under an `any_window` grant uses a broader variant, also when it targets its own preview. This prevents an old execution path from accepting a broader authority by accident.

Keep the P6 action-hash structure. Hash the complete typed action and the coordinator context. Never replace a hashed handle with an address. Keep the exact normalized payload hashing and the shared request identity.

### 9.2 Policy purposes

Extend the live policy with these purposes, gated by features:

```text
input_click
input_any
input_any_click
```

Their closed answers contain the existing input-policy fields and the applicable new station gates. Keep the bytes of the existing `purpose: "input"`.

Admission, the execute checks and the delivery checks must verify the click allowance and the scope that the action needs. A check only at grant creation is not enough.

## 10. Station protocol

This section extends P7 §7.3.

### 10.1 Keyboard and movement under `any_window`

Keep the three explicit execute checks of P7 and its final sequence. Substitute the correct target resolver and the correct gates.

### 10.2 Click

Keep steps 1 to 13 of P7 §7.3: the durable claim, the disclosure check, the focus check and the preparation after focus. Prepare the fixed click executable before the movement.

Then execute:

1. **Movement authorization:** do the third explicit execute check of P7.
2. Read the final active-window state. Revalidate the target, the barrier, the local gates, the coverage and the deadlines.
3. Dispatch one prepared `movecursor`, with no unrelated await before it.
4. **Button authorization:** do a fourth explicit execute check.
5. Collect a bounded final observation bundle: active window, cursor position, clients, monitors, layers and lock state.
6. Require the same target identity and an unchanged target rectangle since the movement. Compute the requested point again, and all destination checks.
7. Require that the observed cursor position equals the requested global point.
8. Check again the synchronous gates, the registry and epoch validity, the current coverage, the cancellation and the original start deadline.
9. Spawn the prepared `wlrctl pointer click left` invocation, with no other await before it.
10. Supervise. Kill and reap on a detected loss. Do the final observations and persist the outcome. Never replay.

`hyprctl cursorpos` reports global layout coordinates. Reference: [Hyprland command documentation](https://wiki.hypr.land/0.44.0/Configuring/Using-hyprctl/).

### 10.3 Final bundle limits

For the new final click bundle:

- At most 250 ms in total.
- The authority answer is at most 500 ms old when the button process starts.
- Every observation that authorizes the button is at most 100 ms old, measured conservatively from the dispatch of its query.
- No sampling loop, no corrective movement and no retry.

For the click branch, this changes the P7 rule that the final active-window read is the only remaining asynchronous observation. The final bounded bundle replaces that single read. There is still no await between the final synchronous checks and the spawn.

Keep the original ten-second execution-start lifetime. Both the movement and the button invocation must start within it. An earlier disclosure, focus or movement does not extend it.

The present executor has one final authorization followed by one input dispatch. A click needs one more stage. Do not append another command to that branch. Reference: [exec.rs](/home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/crates/fleetdeck-station/src/desktop/input/exec.rs:350).

## 11. Execution, accounting and outcomes

### 11.1 Argv

Use only:

```text
hyprctl dispatch movecursor <checked-x> <checked-y>
wlrctl pointer click left
```

There is no shell, no alternate backend, no relative `wlrctl` movement, no fallback helper and no argv that the caller controls.

Keep the cleared child environment, the bounded output, the owned process supervision and the cleanup. Require verified correspondence between the Hyprland IPC and the Wayland compositor and seat that `wlrctl` uses. An ambiguous configuration refuses.

### 11.2 Deadlines

| Stage | Limit |
|---|---:|
| Movement | Existing 1 second |
| Click invocation | 1 second |
| Kill and reap | Existing 2 seconds |
| Complete station handler | Existing 30 seconds |
| Deck wait / handler / MCP response | Existing 35 / 40 / 45 seconds |

Do not extend these deadlines to make room for the new checks.

### 11.3 Charges

Charge one accepted click as:

- One desktop mutation.
- One input action.
- Zero typed characters.
- One existing input-rate entry.

Its internal movement is not a separate accepted operation and has no separate charge. Keep all existing limits, claims, duplicate and conflict handling, permanent charges and recovery behavior.

### 11.4 Outcomes

A click can move the pointer and then refuse the button invocation. A button invocation can complete in part. Neither outcome permits an automatic retry.

Keep `done` with `{}`. It means that the invocation and the required observations ended without a detected failure. It does not mean that the application accepted the intended action. A click that closes its own window on purpose, or that changes the focus, can give an interrupted or ambiguous result.

Interruption stays observational. Stop the owned child. Do not send a compensating button release, Escape or undo. Do not restore the focus or the cursor. Verify the button-release behavior of the backend during the live smoke.

### 11.5 Audit

Add the audit action `desktop.pointer_click`. Its length is zero. Audit the validated target handles and the fixed outcomes under the P7 rules. Never audit coordinates or native metadata.

The audit row `desktop.window_list` contains only the authorized query identity, the result count and the fixed outcome. It does not contain the returned list.

## 12. Board sign-out reuse

Reuse P7 §7.6 exactly as accepted and built (DL-245, DL-246).

Every input mutation that executes newly, in either scope and including a click, goes through the existing attempted transaction and the machine-address sign-out of P7 **before disclosure, focusing or movement**.

Keep:

- The durable canonical address snapshots.
- The deletion of owner and member sessions.
- The revocation and acknowledgment of pending and live board and shell sockets.
- The transactional sign-in exclusion and the socket admission checks of DL-246.
- The permanent invalidity of deleted sessions.
- The startup recovery before the listeners accept work.
- The exposure for the complete operation.
- The built terminal timing: `finished_at + 5 s`, then the 60-second tail. The fallback for an unfinished operation stays 45 seconds.
- No dispatch if a required session or socket cleanup fails.

Duplicates and status reads do not repeat the sign-out. `window_list` does not invoke it.

Keep the residual of the proxy or VPN and the disclosure of the owner lock-out. The broader scope does not make the sign-out by address complete.

## 13. Codes and exact owner texts

Keep the existing framing and the existing code meanings. Add these codes:

| Code | Exact text |
|---|---|
| `input_click_not_allowed` | `This grant does not allow pointer clicks.` |
| `input_scope_not_allowed` | `This grant does not allow input in other application windows.` |
| `input_click_disabled` | `Agent pointer clicks are disabled on the target station.` |
| `input_any_disabled` | `Agent input in other application windows is disabled on the target station.` |
| `input_click_tool_missing` | `The pointer click tool wlrctl is not installed on the target station.` |
| `input_click_tool_unsupported` | `The target desktop cannot use the configured pointer click tool.` |

Emit a code of a setting only after the trusted target directly checks that setting. Emit "unsupported" only from verified compatibility evidence. Do not interpret arbitrary stderr.

For the broader action variants, use fixed generic texts with the existing codes:

| Code | P7b broader-scope text |
|---|---|
| `desktop_window_not_found` | `No input window available to this grant matches that handle.` |
| `input_destination_unavailable` | `The pointer destination is not available inside the target window.` |
| `input_focus_unknown` | `The target window's input focus could not be verified. No further input was started; the pointer may already have moved.` |
| `focus_changed` | `The target window no longer has focus. Input stopped; some input may already have been sent.` |

Keep the legacy P7 texts for the legacy action variants. A refusal after movement for a click in an own preview must also not claim that no input effect occurred.

Use this P7b text for an unknown outcome:

> The input outcome is unknown. Inspect the target application before issuing another input request. Do not retry automatically.

Keep `input_board_forbidden`, the warnings for failure, timeout and interruption, the budget codes, `screen_locked`, the authority codes and the generic enumeration failures.

For owner requests, keep the built strict-decoder behavior. An unknown scope value gives `400 invalid_body`. A recognized scope without negotiated support gives `unsupported`. A scope field of a tool stays `invalid_params`.

Generalize the board sign-out texts:

> You were signed out because an agent used desktop input on the station where this board is open.

> An agent is using desktop input on this station. Sign in again in a minute.

Keep the code and status behavior of `signed_out_for_input` and `sign_in_blocked_for_input`.

## 14. Board texts and controls

Keep the existing input opt-in. Add an option that is independent and unchecked:

> **Allow pointer clicks**

Disclosure:

> Allows this agent to move the pointer and send a left click in its permitted windows. A click can submit forms or approve actions. The pointer or receiving surface can change after Fleet Deck checks it.

Scope selector:

> **This agent’s preview windows**
> **Other application windows on the grant stations**

When the owner selects the broader scope, a separate unchecked acknowledgment is required:

> **Allow input in other application windows**

Red disclosure:

> This agent can list window positions, view permitted screenshots, and type, press keys and move the pointer in existing applications, including terminals and signed-in browser windows. If clicks are enabled, it can click there too. It can read personal information and act using your logged-in accounts. Fleet Deck refuses detected board and wall windows, but detection and targeting are incomplete. Board sign-out protects this deck only at the machine addresses it records. Agents run as your OS user.

Station labels:

> **Allow agent clicks**
> **Allow agent input in other application windows**

Both disclosures state three facts: the setting requires an explicitly authorized grant, a disable requests an interruption, and input that is already sent cannot be undone.

Grant rows show the scope, the click allowance, the existing usage and limits, the experimental status, the target readiness, and the revoke and kill switches. There is no display of titles or of input-payload activity.

Use these notifications on the target, with no action:

> Fleet Deck is clicking for `<callsign>` in an agent preview.

For the broader scope:

> Fleet Deck is sending keyboard input for `<callsign>` to an application window.
> Fleet Deck is moving the pointer for `<callsign>` in an application window.
> Fleet Deck is clicking for `<callsign>` in an application window.

A notification failure prevents focus and input. A notification grants no authority.

## 15. Doctor line

Add a `desktop-click` doctor line. It reports the availability and version of the executable, and the explicit status of compatibility and smoke. The doctor must never click as a readiness probe.

Remediation and enablement use the authorized remote-management paths of the board. There is no station-local approval requirement.

## 16. Tests: the four proofs

Use controlled clocks and stage barriers. The tests prove refusals for changes that the checks can observe. They must not assert atomic event delivery.

| Proof | Required additions |
|---|---|
| **Behavior** | Independent combinations of grants and settings; exact schemas and catalog; fixed argv; coordinate conversion; the four click execute checks; cursor-position verification; list projection and limits; lifetime tracking of ordinary windows; both resolver paths; one charge, one claim and one click; listed handles rejected by `window_close`; real authenticated-link coverage for the Claude and Codex paths. |
| **Failure** | Revoke, expiry, generation, feature and every applicable gate at each await; the move succeeds and then the button is refused; cursor drift, geometry changes, overlays and focus changes; age limits of the final bundle; malformed and stalled observations; ambiguous compositor or seat; missing helper, spawn failure, hang, and kill and reap; close, reopen and address reuse; event loss; races of caps and rates; a crash at every click stage; no replay and no corrective movement. |
| **Exposure** | Sentinel titles, classes, URLs, PIDs, addresses, focus history and data of unrelated windows never escape; only the approved geometry appears; no counts of excluded windows; handle isolation across callers, grants, stations and generations; no coordinates in storage or audit; board filtering before handle issuance; fresh barrier checks; the disclosure of screenshot and application privacy; all DL-246 races of sign-out, sign-in and pending sockets, exercised for click and for broader actions. |
| **Version** | Independent old and new feature combinations; bindings with click only, broader only and combined; a downgrade after acceptance and between the movement and button stages; the fixed catalog; unchanged legacy fixtures for start, action, hash and policy; V37→V38 data preservation and foreign-key checks; existing P7 grants stay unchanged; all supported MCP protocol versions; live results stay pending as a separate item. |

## 17. Live smoke

The live smoke runs on dev1 and uses a remotely initiated, owner-authorized flow. It needs a separate recorded acceptance for each of: preview clicks, broader keyboard and movement, and broader clicks.

Verify:

- The exact versions of Omarchy, Hyprland, Chromium and `wlrctl`.
- The protocol version, the correspondence of compositor and Wayland, the seat selection and the refusal of ambiguity.
- Execution without root, `uinput`, a daemon or an interaction for permission on the station.
- Whether the creation of the virtual pointer keeps the checked cursor position.
- The real delivery of press and release, the button cleanup after a termination, and the interpretation of a double click by the application.
- Physical pointer movement, held buttons and modifiers, pointer grabs and focus that follows the pointer.
- Scaling, rotation, negative origins, multiple monitors, special workspaces and windows that span capture permissions.
- Native dialogs, popups, overlapping clients, the refusal for top and overlay layers, and the acceptance for background and bottom layers.
- The latency of the final bundle and all original execution deadlines.
- Listing of ordinary windows, screenshots, handle expiry, close and reopen, and compositor reconnect.
- The refusal of detected board and wall windows, the acknowledged false negatives for ordinary browser windows, and the existing sign-out by machine address.
- Cancellation, lock, revoke, all kill switches, notification failure and DND.
- Claude and Codex discovery, duplicates, status and handling of an unknown outcome.

Use dummy applications and dummy approvals. A passing smoke records the supported configurations and the observations. It does not remove the targeting races.

## 18. What changes from P7

| P7 rule | P7b treatment |
|---|---|
| Header: buttons and `any` deferred | Superseded for these separately authorized capabilities. The other deferred gestures stay deferred. |
| §3: only `own_windows` | Add `any_window`, the click allowance and independent station gates. |
| §4: never list or accept `pointer_click` | An exception only under the new click contract and feature. `scroll` stays unavailable. |
| §5.3 pointer semantics | Kept. A click adds the verification of the actual position after the movement. |
| §7.1 preview-only resolution | Kept for the preview scope. A separate input-only registry serves the broader scope. |
| §7.2 barrier | Kept. Applied before the listing and throughout the broader execution. Missing launch provenance is an explicit limitation. |
| §7.3 three execute checks | Kept for keyboard and movement. A click needs four, and the bounded final observation bundle. |
| §7.4 one injection invocation | A click is one accepted operation. It has one movement dispatch and one button invocation, each checked independently. |
| §7.6 sign-out | Reused with the DL-246 fixes and the built timing. The texts are generalized. |
| §14 helper presence never enables click | Kept. Explicit allowance and independent negotiation stay mandatory. |
| §15 click spike deferred | Becomes the operational-acceptance smoke of P7b. |

These P7 items stay: the grammar, the sensitive stdin, the authorization foundations, the permanent accounting, the claim and recovery rules, no replay, the single station input slot, the deadlines, the remote-first management and the same-UID limitations.

## 19. Build task split and acceptance gates

Use the same build split as P7:

| Task | Scope |
|---|---|
| **F** | Foundation and migration. |
| **S1** | Caller, catalog and policy. |
| **K1** | Deck admission and execution. |
| **K2** | Sign-out integration and regression proof. |
| **S2** | Target registry and click execution. |
| **W** | Board. |
| **P** | The four proofs. |
| **D** | Documentation. |

Gates:

1. Before implementation: adversary DESIGN REVIEW PASS.
2. Before merge: adversary PRE-PR PASS.
3. Before operational reliance: the live smoke of §17. Both capabilities stay experimental and off by default until then.
