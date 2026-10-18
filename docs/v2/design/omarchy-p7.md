# P7: coordinator keyboard input and pointer movement

- **Status:** proposed contract (architect, 2026-10-18), from the advisor answer `advisor-p7-answer.md` against v2 HEAD `0eeeb149`. The architect accepts the recommendations P7-OD1 to P7-OD7 as architect decisions (O12-23; the owner may change any of them) and adds the board sign-out of §7.6 (revision 2: DESIGN REVIEW round 1 FAIL with 3 BLOCKER on the first barrier and 1 MAJOR on the `wtype` keymap file; revision 3: round 2 FAIL with 2 BLOCKER on the fence and its unfence; reports `adv-omarchy-p7-design-r1-report.md`, `adv-omarchy-p7-design-r2-report.md`). DESIGN REVIEW required before implementation; PRE-PR required before merge; live smoke required before operational reliance (the feature stays experimental and off by default until then).
- **Parent:** `omarchy.md` §§7.4, 7.5, 11, 13 and 14.
- **Prerequisite:** accepted and built P6, `omarchy-p6.md`, including subsequent authority fixes recorded in DL-242.
- **Scope:** coordinator tools `key_press`, `type_text` and `pointer_move`, targeting caller-owned P6 preview windows through opaque `window_id` handles.
- **Deferred:** `any` scope, pointer buttons, scrolling, dragging, held keys, terminal input and ordinary-session grants.
- **Constraints:** no new library dependency, privileged helper, `uinput`, `ydotoold`, shell command interface or station-local approval step.

Once accepted, this annex replaces the parent’s P7 scope, input protocol and input tests. P6 remains authoritative except for the explicit extensions below.

## 1. Recommendation and defects in option A

Choose **A**, with experimental status until the live spike.

A uses the existing authority and preview-ownership paths and permits useful implementation and deterministic testing on the development machine. B expands the uncertain boundary before the narrow version has been exercised. C unnecessarily delays schema, accounting, authorization, board and fake-desktop work.

A nevertheless needs these corrections:

1. **A focus check cannot bind input to a window.** `wtype` receives no P6 handle or Hyprland address. Checking focus and sending input are separate operations.
2. **The race lasts beyond process spawn.** Focus may change while text is being emitted. A successful exit does not prove the intended application received or accepted every character.
3. **No click does not mean no approval.** Return, Space and keyboard navigation can activate controls.
4. **Window ownership does not identify current content.** A P6 preview has an isolated browser profile and a unique class, but may navigate, redirect, open dialogs or change its title.
5. **Title and class detection cannot identify every board window.** Hyprland does not provide an authoritative browser URL through `activewindow`. A missing board title is not evidence that the page is safe.
6. **Pointer movement is a global effect.** A focused preview does not establish which surface is under a destination point. Movement can also affect focus; Hyprland documents configurable focus-following-pointer behavior. [Hyprland input configuration](https://wiki.hypr.land/Configuring/Basics/Variables/)
7. **P6’s first-start check is insufficient.** `Start::begin` caches its first successful check. P7 must explicitly perform another execute check before injection, after disclosure and focusing.
8. **Text transport needs separate handling.** The existing desktop runner has null stdin. Typed text must use a private pipe, not argv, an environment variable or a temporary text file.
9. **P6 accounting and recovery must extend to input.** Input requires durable claims, duplicate handling, shared limits and uncertain-result behavior.

The implementation must distinguish:

- **Enforced checks:** grant authority, station membership, live gates, recorded preview ownership and refusal of detected board windows.
- **Best-effort observation:** current focus, pointer destination and detection of subsequent changes.
- **Unproved behavior:** the receiving surface and application effect of every synthetic event.

P7 must not describe these tools as an isolation boundary against agents running as the owner’s UID.

## 2. Owner decisions

The architect accepts each recommendation below as an architect decision (O12-23, 2026-10-18), as for P6 (O12-1) and P5b (O12-6). The owner may replace any of them. P7-OD1 is accepted only together with the board sign-out of §7.6, which does not depend on window detection.

| ID | Owner decision needed | Recommendation |
|---|---|---|
| **P7-OD1** | Whether the residual focus race and incomplete board detection are acceptable. DL-197 requires board refusal. | Accept `own_windows` as checked, best-effort targeting, with the disclosures below. Known board windows remain prohibited. If DL-197 requires an absolute guarantee against board input, A cannot meet it with this backend; defer execution under C pending a stronger mechanism. |
| **P7-OD2** | The parent requires the spike before DESIGN REVIEW and code. | Permit contract review and implementation before the live spike, with production reliance withheld. Amend §§11 and 13 accordingly. |
| **P7-OD3** | Grant prerequisites and elevated opt-in. | Require `input`, `desktop` and `capture` in the same coordinator grant, plus explicit input opt-in. Keep all enablement on the board. |
| **P7-OD4** | Initial keyboard grammar and pointer coordinates. | Accept §5: conservative keys, Ctrl/Shift modifiers, no embedded text controls, window-relative logical coordinates. |
| **P7-OD5** | Numeric limits. | Accept §9, including permanent charges and shared P6 limits. |
| **P7-OD6** | Whether each call needs another owner approval. | No per-call approval. Require a fixed target-station disclosure for every newly executed call. |
| **P7-OD7** | Execution granularity and interruption limits. | One bounded `wtype` invocation per key/text call. Authority is checked immediately before that invocation, not atomically before each generated key event. An invocation may produce partial input before interruption. |

The remaining protocol rules implement these decisions.

Inherited decisions remain binding:

- Coordinator grants only, following O12-1.
- Grant stations bound physical desktop targets.
- Agents run as the owner’s UID.
- The wall remains read-only.
- Settings, elevation, grant creation, renewal, revocation and preview closing are available remotely from the board.
- No desktop notification grants authority.

## 3. Grants, settings and migration

### 3.1 Public class and explicit allowance

Add serialized grant class:

```text
input
```

It is a fleet-control class and participates in the existing active-mutating-grant exclusion.

It does not imply another class. Under **P7-OD3**, an enabled P7 grant must explicitly contain all three:

```text
desktop
capture
input
```

The capture prerequisite preserves the parent’s requirement that an input-capable agent can obtain screenshots. It does not prove the agent viewed a screenshot, understood it or can read every screenshot within P6’s image-size limit.

Add owner grant-request fields:

```ts
{
  allow_input?: boolean;          // default false
  input_scope?: "own_windows";
  input_cap?: number;
  input_character_cap?: number;
}
```

Validation:

- `allow_input: true` requires all three classes.
- With `allow_input: true`, omitted scope normalizes to `own_windows`.
- `any` and every other scope are refused.
- Explicit scope or caps without `allow_input: true` are invalid.
- Without opt-in, effective input caps are zero and no input tools are listed.
- Existing grants acquire no input allowance.
- Cap zero means no new accepted calls; it does not mean unlimited.
- Adding input authority requires a new elevated owner grant/start or resume. Renewal does not add it.
- A new grant does not inherit another grant’s previews.

Store:

```text
allow_input
input_scope
input_cap
input_used
input_character_cap
input_character_used
```

The allowance and scope are durable authorization facts. Positive caps alone do not establish allowance.

### 3.2 Caller start specification

Extend `CoordinatorSpec` with:

```ts
{
  allow_input?: boolean;
  input_scope?: "own_windows";
  input_cap?: number;                 // board → deck only
  input_character_cap?: number;       // board → deck only
}
```

The deck validates and removes caps before forwarding the start command.

Send `allow_input: true` and `input_scope: "own_windows"` only to a caller link negotiating `desktop_input_mcp.v1`. Omit disabled fields.

Add station-internal `ToolClass::InputEnabled`. The station derives it only from:

- public class `input`;
- classes `desktop` and `capture`;
- `allow_input: true`;
- scope `own_windows`;
- required start-time feature negotiation.

`ToolClass::parse` must never accept `input_enabled` as a deck-supplied class. Naming it directly fails closed.

The input tools require both `Input` and `InputEnabled`.

### 3.3 Live gates

Every input execution requires:

1. Active grant, matching binding and generation.
2. Neither absolute nor idle expiry reached.
3. Target station belongs to the grant.
4. Required classes and explicit allowance.
5. Deck `fleet_control`.
6. Deck `desktop_control`.
7. Caller `allow_fleet_control`, obtained through P6’s live caller-policy round trip.
8. Target `allow_fleet_control`.
9. Target `allow_desktop`.
10. Target `allow_desktop_input`.
11. Target `allow_desktop_capture`.
12. Current required features.
13. Known unlocked screen.
14. Target preview’s current monitor coverage permitted by `capture_monitors`.

Implement the parent’s planned `allow_desktop_input` setting with default `false` throughout configuration, policy, remote settings, board projection and tests. Do not assume P6 already implemented it.

Enabling it requires elevated owner authorization through the board. Disabling it requires no elevation.

No separate deck input switch is added: `desktop_control` is the deck-wide input kill switch.

A headless caller may operate an authorized remote desktop. Caller `allow_desktop` and `allow_desktop_input` are not required for that remote operation.

Settings publications are projections, not live authorization. The deck obtains fresh policy; the target independently reads local policy at execution.

### 3.4 Board text

Under **P7-OD3**, the unchecked grant option is:

> **Allow keyboard input and pointer movement in this agent’s previews**

Display this red disclosure beside it:

> This agent can type, press keys and move the pointer in preview windows opened for it on the grant stations. Keyboard input can activate buttons and submit forms. Focus can change after Fleet Deck checks it, so input may reach another window, including an owner board window. Board detection is incomplete. Agents run as your OS user. Pointer clicks and unrestricted window input are unavailable. This feature is experimental until its live smoke passes.

Retain P6’s screenshot and preview-networking disclosures.

The station setting is labelled:

> **Allow agent desktop input**

Its disclosure is:

> Allows explicitly authorized coordinator grants to use keyboard input and pointer movement on this station. Turning this off stops new input and requests interruption of active input; already sent input cannot be undone.

The grant form shows scope as fixed text:

> **Scope: this caller’s preview windows**

There is no `any` selector.

### 3.5 Migration

At this HEAD, V36 is the latest deck migration. Add **V37**, unless another accepted migration takes that number before implementation. Do not edit V34 or V36.

Add these token columns:

```sql
ALTER TABLE tokens ADD COLUMN allow_input INTEGER NOT NULL DEFAULT 0
  CHECK (allow_input IN (0, 1));

ALTER TABLE tokens ADD COLUMN input_scope TEXT
  CHECK (input_scope IS NULL OR input_scope = 'own_windows');

ALTER TABLE tokens ADD COLUMN input_cap INTEGER NOT NULL DEFAULT 0
  CHECK (input_cap BETWEEN 0 AND 5000);

ALTER TABLE tokens ADD COLUMN input_used INTEGER NOT NULL DEFAULT 0
  CHECK (input_used >= 0);

ALTER TABLE tokens ADD COLUMN input_character_cap INTEGER NOT NULL DEFAULT 0
  CHECK (input_character_cap BETWEEN 0 AND 1000000);

ALTER TABLE tokens ADD COLUMN input_character_used INTEGER NOT NULL DEFAULT 0
  CHECK (input_character_used >= 0);
```

In the same forward migration:

- Rebuild `coordinator_desktop_ops` to extend its checked tool list with `key_press`, `type_text`, `pointer_move`.
- Extend its checked class list with `input`.
- Add nullable `input_length`, constrained to 0–2000.
- Preserve every existing column, value, unique constraint and index, including recording `seconds`.
- Rebuild `coordinator_desktop_rate` to add bucket `input`.
- Preserve existing rate entries.
- Preserve rejected-request identities and all counters.

New operation rows use `input_length` as follows:

| Tool | Length |
|---|---:|
| `key_press` | 1 |
| `type_text` | Unicode scalar count |
| `pointer_move` | 0 |

Legacy rows retain null `input_length`.

Existing grants migrate to disabled input, null scope and zero caps/usage. No legacy class list is rewritten.

Extend the station’s durable desktop-claim kind validation if necessary. Use its next migration only if its schema requires one.

## 4. Catalog and exact JSON schemas

Add exactly:

```text
key_press
type_text
pointer_move
```

**Do not list or accept `pointer_click` or `scroll`.** An unlisted tool follows existing unknown-tool behavior. There is no click stub that can later acquire an implementation without another contract.

Continue exposing P6’s `desktop_action_status` through the required `desktop` class.

The catalog is fixed at provider-process start. Include the three tools only when the input-enabled binding and required features exist then.

Settings, missing executables, lock state and temporary desktop availability affect calls, not the fixed catalog. Later reconnect, renewal or downgrade does not add or remove listed tools.

This is the normative schema bundle. Copy `$defs` into each tool schema’s root when publishing it, as in P6.

```json
{
  "$defs": {
    "station": {
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
    "window_id": {
      "type": "string",
      "minLength": 32,
      "maxLength": 32,
      "pattern": "^[0-9a-f]{32}$"
    },
    "chord": {
      "type": "string",
      "minLength": 1,
      "maxLength": 32,
      "pattern": "^(Ctrl\\+)?(Shift\\+)?([a-z0-9]|Return|Tab|Escape|BackSpace|Delete|Insert|Home|End|PageUp|PageDown|Left|Right|Up|Down|Space)$"
    }
  },
  "schemas": {
    "key_press": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id", "chord"],
      "properties": {
        "request_id": {"$ref": "#/$defs/request_id"},
        "station": {"$ref": "#/$defs/station"},
        "window_id": {"$ref": "#/$defs/window_id"},
        "chord": {"$ref": "#/$defs/chord"}
      }
    },
    "type_text": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id", "text"],
      "properties": {
        "request_id": {"$ref": "#/$defs/request_id"},
        "station": {"$ref": "#/$defs/station"},
        "window_id": {"$ref": "#/$defs/window_id"},
        "text": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2000,
          "pattern": "^[^\\u0000-\\u001F\\u007F-\\u009F\\u2028\\u2029]+$"
        }
      }
    },
    "pointer_move": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id", "x", "y"],
      "properties": {
        "request_id": {"$ref": "#/$defs/request_id"},
        "station": {"$ref": "#/$defs/station"},
        "window_id": {"$ref": "#/$defs/window_id"},
        "x": {
          "type": "integer",
          "minimum": 0,
          "maximum": 32767
        },
        "y": {
          "type": "integer",
          "minimum": 0,
          "maximum": 32767
        }
      }
    }
  }
}
```

Both trusted sides additionally validate:

- Omitted station normalizes to the authenticated caller’s station.
- Unicode lengths count scalar values, not UTF-16 code units or grapheme clusters.
- Text is at most 8,000 UTF-8 bytes.
- Lone surrogate encodings and malformed JSON/UTF-8 are invalid.
- Unknown fields are refused at every level.
- Null is not omission.
- No caller identity, grant, generation, address, executable, argv, environment, monitor, delay, repeat count or scope override is accepted.
- Errors never interpolate supplied values.

A syntactically valid handle establishes no authority.

## 5. Keyboard grammar and pointer semantics

This section requires **P7-OD4**.

### 5.1 Keys and modifiers

The schema’s key set is exhaustive.

Modifiers are `Ctrl` and `Shift`, each at most once, in that order. They cannot appear alone.

Examples:

```text
a
Shift+a
Ctrl+a
Ctrl+Shift+a
Return
Shift+Tab
Ctrl+Home
```

Reject:

```text
CTRL+A
Control+a
Shift+Ctrl+a
Ctrl+Ctrl+a
Alt+Tab
Super+Return
F5
Ctrl++
a,b
```

No aliases, arbitrary XKB names, numeric keycodes, modifier locking, key-down/up tools, sequences or repeat options are accepted.

Map `Space` to XKB `space`; other named keys map to their corresponding allowlisted XKB names.

Build fixed argv:

1. `-M ctrl` if requested.
2. `-M shift` if requested.
3. `-k <allowlisted keysym>`.
4. Release modifiers in reverse order with `-m`.

The operation requests one press/release chord. It does not promise that application or compositor shortcuts remain inside the preview. Excluding Alt and Super reduces common window-management shortcuts; it does not eliminate global bindings.

### 5.2 Text

Accept 1–2,000 Unicode scalar values, at most 8,000 UTF-8 bytes.

Reject:

- C0 controls U+0000–U+001F.
- DEL and C1 controls U+007F–U+009F.
- U+2028 and U+2029.

Thus newline, carriage return, tab, Escape, NUL and backspace are invalid inside `text`. Use separate `key_press` calls for Return or Tab.

Preserve accepted text exactly. Do not trim, normalize, substitute characters or append Return.

Run fixed executable `wtype` with argv:

```text
-
```

Write the validated UTF-8 bytes to a private stdin pipe and close it. Do not use a clipboard.

`wtype` supports stdin and emits virtual-keyboard events using its own keymap. Its interface provides neither a P6-window target nor confirmation of application text insertion. Exact behavior must be verified against the installed version. [wtype source](https://github.com/atx/wtype/blob/master/main.c)

**The `wtype` keymap file (architect decision, answer to DESIGN REVIEW round 1 MAJOR):** upstream `wtype` builds a keymap for the characters it types and passes it to the compositor through a file that it creates with `mkstemp` in `/tmp`, unlinks at once, and then writes through its descriptor. The keymap lists the distinct characters, so the file content is derived from the typed text. The architect accepts this as the one exception to the rule of §8.1: the file has no name once unlinked (before any data is written), lives only while `wtype` runs, and is readable only through the descriptors of `wtype` and the compositor, which processes of the same UID can already reach. The same UID can also read the text from the memory of the station and of `wtype`, so the exception adds no new reader. Fleet Deck writes no typed text to any file, argv, environment or log itself. The live smoke records the installed `wtype` version.

No fallback to argv is permitted if stdin support is unavailable.

### 5.3 Pointer coordinates

`x` and `y` are integer **logical compositor coordinates relative to the current preview rectangle’s top-left**, using the rectangle reported by Hyprland.

They are not screenshot pixels, physical monitor pixels, percentages or global coordinates.

At execution:

```text
0 ≤ x < current logical width
0 ≤ y < current logical height

global_x = current rectangle x + x
global_y = current rectangle y + y
```

Use checked arithmetic. Do not clamp an out-of-bounds point.

Dispatch fixed argv equivalent to:

```text
hyprctl dispatch movecursor <global_x> <global_y>
```

Hyprland documents `focuswindow` window selectors and `movecursor x y`; the installed station version is the live-spike compatibility target. [Hyprland dispatchers](https://wiki.hypr.land/0.54.0/Configuring/Dispatchers/)

Additional pointer checks:

- Preview must be mapped, visible and not hidden.
- Destination must lie on a live, enabled, permitted monitor.
- Refuse a destination overlapping a different mapped client or any reported layer surface. Conservative false refusals are acceptable.
- Missing or malformed geometry, layer data or monitor mapping fails closed.
- The destination checks are repeated after focusing.
- No motion path, animation, button or automatic click follows.

These checks reduce accidental movement over another surface. They do not provide an atomic hit-test-and-move guarantee.

## 6. Wire contract and feature negotiation

### 6.1 Feature

Add:

```text
desktop_input_mcp.v1
```

Advertise it in implemented feature tables and the owner features endpoint.

It means support for this contract’s protocol. It does not attest that `wtype` is installed or that the live smoke passed.

Required negotiation:

| Link | Required features |
|---|---|
| Caller ↔ deck | P6 desktop/capture caller requirements, plus `desktop_input_mcp.v1` |
| Each target ↔ deck | P6 desktop/capture target requirements, plus `desktop_input_mcp.v1` |

Every input-enabled target must satisfy the existing capture requirements, including `capture_mcp.v1`, `desktop_capture.v1` and `artifacts_delete.v1`.

### 6.2 Calls and actions

Extend the private `coordinator.desktop_call` union:

```ts
| { tool: "key_press"; arguments: KeyPressArguments }
| { tool: "type_text"; arguments: TypeTextArguments }
| { tool: "pointer_move"; arguments: PointerMoveArguments }
```

Use P6’s envelope, command-ID matching, ten-second acceptance deadline, durable acceptance and outbox exclusions. Forward once.

Extend `DesktopAction` with these closed variants:

```ts
type InputAction =
  | {
      action: "input_key_press";
      window_id: string;
      chord: string;
    }
  | {
      action: "input_type_text";
      window_id: string;
      text: string;
    }
  | {
      action: "input_pointer_move";
      window_id: string;
      x: number;
      y: number;
    };
```

Reject these variants without:

- coordinator authority;
- `desktop_input_mcp.v1`;
- valid P7 arguments.

P7 creates no owner HTTP input-execution route and no raw `input.*` escape hatch. Owner management routes remain owner-only, with Admin authorization and elevation where applicable.

### 6.3 Hashes and claims

Use P6’s coordinator-aware action hash unchanged in structure. It covers:

- Complete typed input action.
- Complete coordinator context.
- Target fingerprint.
- Existing monitor field, null for these actions.

The logical target is `window_id`. Never replace it in the hashed action with a compositor address.

P6’s normalized request payload hash covers station, tool and all arguments except `request_id`, including exact accepted text.

Required hashes remain private operation metadata. Do not copy text, chords, payload hashes or authority hashes into audit rows.

The target durably claims the operation before disclosure, focusing or injection. The claim binds operation identity and action hash. A changed payload under the same operation is refused.

### 6.4 Policy extension

Extend `desktop.policy_now` with:

```ts
{
  purpose: "input";
  monitor: string | null;
}
```

For this purpose only, its closed answer is:

```ts
{
  allow_fleet_control: boolean;
  allow_desktop: boolean;
  allow_desktop_capture: boolean;
  allow_desktop_input: boolean;
  monitor_allowed: boolean;
}
```

Send and accept this shape only with `desktop_input_mcp.v1`.

The P6 `desktop` and `capture` request/answer bytes remain unchanged.

The initial deck policy query may use a null monitor because preview geometry belongs to the target. The target must enforce actual current monitor coverage before input. A null-monitor answer is not evidence that a resolved monitor is permitted.

Continue using:

```text
desktop.authority_check
phase = execute
```

No new authority phase is needed. Input operations cannot use `import`. Protected status reads retain P6’s read permits and checks.

## 7. Target identity, board barrier and execution protocol

### 7.1 Preview ownership

Resolve the handle exclusively through the target’s P6 preview registry.

Require an exact match on:

```text
deck fingerprint
grant ID
caller station ID
caller session ID
opaque window_id
compositor signature
recorded address
recorded class
```

Retain P6’s signature/address/class identity. Do not silently introduce PID-based ownership.

Renewal retains grant identity and may retain its previews. A new grant does not.

Target restart, compositor change, event gap, reconnect or preview eviction invalidates the handle according to P6’s registry rules.

Never rediscover an invalid handle from a similar class or title.

Unknown, foreign, owner-opened and otherwise inaccessible handles return the same `desktop_window_not_found`.

### 7.2 Board-window barrier

Apply this barrier to the resolved target before focusing and to the final active-window snapshot.

Refuse when any available evidence identifies Fleet Deck:

- A station record identifies the window as `Kind::Wall`.
- Current or initial class equals `fleetdeck-wall`, or begins `fleetdeck-wall-`.
- Current or initial class equals `fleetdeck-board`, or begins `fleetdeck-board-`.
- Current or initial title contains `fleet deck` or `fleetdeck`, ASCII case-insensitively.
- A title contains a known deck-origin host as a complete hostname token.
- Trusted launch provenance identifies the initial URL as a known deck origin, regardless of route.

Known origins come from trusted deck/link configuration and observed authenticated origins, never agent arguments. Match normalized hostnames; do not use arbitrary substring matches such as treating `deck.example.evil` as `deck.example`.

Retain only the minimal board-origin classification in preview provenance; do not add URL logging.

A board match overrides preview ownership. A preview that becomes identifiable as a board loses input eligibility.

For an active foreign window, return `focus_changed`; do not disclose that the foreign window was a board. `input_board_forbidden` is available only after ownership of the requested handle is established.

Limitations:

- A title is attacker-controlled content.
- Hyprland’s window metadata is not an authoritative URL source.
- Initial launch provenance cannot observe later navigation.
- Ordinary browser classes do not identify their current origin.
- A false negative remains possible.
- Fresh preview profiles reduce accidental reuse of owner cookies; they do not establish an application-security boundary.

**P7-OD1 expressly covers these limitations.** Do not claim that this detector proves every board window is excluded.

### 7.3 Station protocol

For each newly dispatched operation:

1. Validate authority, coordinator context, feature, hash and action grammar before inspecting desktop resources.
2. Read live local gates.
3. Acquire the station-wide P7 input slot with immediate refusal if occupied. Do not queue.
4. Resolve and verify the owned preview.
5. Obtain bounded current compositor, client, monitor and lock information.
6. Require known unlocked state and permitted monitor coverage.
7. Apply the board barrier and pointer checks.
8. Prepare fixed commands, pipes and cancellation observers. No input program starts yet.
9. Persist the target claim and required attempt record.
10. Perform a fresh execute authority check, reread local gates, and send the fixed disclosure notification.
11. Perform another fresh execute check immediately before `focuswindow address:<resolved address>`. Revalidate the registry record after the await.
12. Focus exactly that address. Do not switch to a guessed window, move the window, unlock the screen or retry focus.
13. Complete final bounded preparation and snapshots, including pointer geometry where applicable.
14. Perform a fresh execute authority check for the injection.
15. After its answer, issue the final `hyprctl activewindow -j` read. This is the only remaining asynchronous observation before injection.
16. Require that the active signature/address/class matches the same recorded preview. Apply the board barrier again.
17. Reread synchronous local gates, link/epoch state, registry validity and authority start deadlines. Check the age limits below.
18. Spawn the prepared `wtype` invocation or dispatch the single pointer movement without another queue, remote call or unrelated preparation await.
19. Observe completion and cancellation; perform a final focus/identity observation for outcome classification.
20. Persist the terminal or uncertain result. Never resend or resume the operation.

Use an explicit execute recheck, such as the existing `check_phase(Execute, …)` path. Calling `Start::begin` again is insufficient.

Disclosure, focusing and injection are separate externally visible steps. Refusal before injection does not imply that no notification or focus change occurred.

### 7.4 Race and guarantees

The final sequence is:

```text
remote authority answer
    → bounded active-window read
    → synchronous local checks
    → injection start
    → generated events
```

It cannot make authority, focus and event delivery atomic.

Requirements:

- Final active-window read completes within 250 ms.
- Remote authority answer is at most 500 ms old at injection start.
- Successful focus snapshot is at most 100 ms old at injection start.
- Exceeding a bound refuses injection.
- No repeated snapshot loop attempts to obtain a passing result.

These bounds constrain station-side delay. They do not bound compositor scheduling or eliminate the race.

For this contract, **one injection is one `wtype` invocation or one `movecursor` dispatch**. It is not each constituent key event.

`wtype` cannot guarantee:

- delivery to the window checked by another process;
- application acceptance or exact resulting text;
- suppression of compositor/global shortcuts;
- absence of concurrent physical input;
- atomic completion or rollback.

A post-check can detect some changes after input. It cannot prove no intervening focus change occurred.

The parent test “a focus change between the focus step and the injection is refused” becomes:

> A change visible to the final check is refused before injection. Changes after that observation may be detected and interrupted, but delivery to another surface cannot be ruled out.

### 7.5 Interruption

Observe preview close/invalidation, compositor event loss, focus changes, local policy changes and link loss while an input process is active.

On detected loss:

- Stop supplying stdin.
- Kill and reap the owned input child.
- Record the fixed outcome.
- Do not send synthetic Escape, BackSpace, undo or retry text.
- Do not restore the previous focus or pointer automatically.

Use existing focus events plus bounded polling while `wtype` runs. Poll at most once per 100 ms, permit only one outstanding poll, and stop on malformed data or observation failure.

Owner preview close must not wait behind the input slot. Closing invalidates/cancels active input through the shared preview identity.

P7 introduces no continuation lease. A remote gate change after the final execute check may remain unobserved until notification, disconnect or process completion. Under **P7-OD7**, the bounded invocation may already have sent input and may finish before cancellation is observed.

### 7.6 Board sign-out on the target machine (architect addition, revision 4)

The window checks of §7.2 cannot prove that keystrokes never reach a board window (a focus change after the final check, a preview that shows the board, keyboard navigation). Synthetic input reaches only the desktop of the target station, so the browser sessions at risk are those that run on that machine. History: revision 1 refused decisions during a 5 s window (round 1: a delayed request, a restart and a composer `!` command pass it); revision 2 fenced the session and let an elevation unfence it (round 2: a request sent while fenced becomes valid again after the unfence; approver members were not covered). Revision 3 signs such sessions out: a deleted session can never make a request valid again, and the human signs in again with their own credentials (round 3: a session without a recorded address and a sign-in during the tail escape the sign-out; a shell socket that is still opening misses the sweep). Revision 4 closes both (the owner approved a 4th round, 2026-10-18).

**Browser sessions.** Both kinds of board browser session: the owner login sessions and the member sessions (approver and other members). The wall and display principals are not browser board sessions with mutations and are not affected.

**Address form.** Every address is kept and compared in canonical form: an IPv4-mapped IPv6 address becomes its IPv4 address; IPv6 in its compressed lowercase form; no port, no zone.

**Machine addresses of a target.** For each target station with `desktop_input_mcp.v1`, the set of:
- the peer address of the station's current link;
- the `host_addresses` that the station reports in its `desktop.info` answer: at most 16 addresses of its up interfaces, loopback and link-local excluded (ignored from a station without the feature; capped at 16);
- when the target is the deck's own local station (the station of `fleetdeck up`, the same machine): every loopback address and every address of the deck's own interfaces.

**Durable snapshot.** In the transaction that marks an input operation `attempted`, the deck stores the machine-address set of its target in the new operation column `exposure_addrs` (a JSON array of canonical strings, at most 40 entries). Exposure and sign-out use this snapshot, never the live link state, so a disconnect or a deck restart cannot change it.

**Exposure.** An operation is *exposing* from its `attempted` commit until 5 s after its terminal result (`finished_at`), and, for an uncertain result, until `attempted_at` + the input deadline of §10 + 5 s. A deck restart rebuilds the exposing operations from the durable rows before the board API accepts a request (the recovery of §11 runs before the HTTP listener starts).

**Last address of a session.** The deck records `last_addr` (canonical) and `last_addr_seen_at` on the session row (owner login session and member session) when the session is created (the sign-in request's address) and at each authenticated board request and socket upgrade, at most once per 10 s per session. The address is the immediate TCP peer of the board connection. Migration V37 deletes every existing owner login session and member session, so that no session without an address exists (every browser signs in once after the upgrade).

**Sign-out.**
1. In the `attempted` transaction (before the dispatch), the deck deletes every owner login session and every member session whose `last_addr` is in the snapshot (whatever its age), and every session that has a live or pending socket (below) from an address in the snapshot. The dispatch waits until the socket step below has ended every relay of a deleted session.
2. While any operation is exposing, and for 60 s after its exposure ends, an authenticated board request or socket upgrade from an address in that operation's snapshot deletes its session before the request is handled and answers 401 `signed_out_for_input`. A sign-in (owner password, owner code, member credentials, display or device link) from such an address in that time is refused before any session is created, with 403 `sign_in_blocked_for_input` (`An agent is typing on this station. Sign in again in a minute.`); no cookie or CSRF token is issued.
3. Deleted sessions stay deleted: the cookie and the CSRF token of a deleted session are refused for good (the existing session lookup), so a request that an agent caused and that arrives late is refused whatever the time.
- Audit: one row `board.signed_out_for_input` per deleted session, with the session kind and the operation ID, never an address list or typed text.

**Board.** On 401 `signed_out_for_input` (or a closed WebSocket with that reason), the board shows the sign-in page with the line `You were signed out because an agent typed on the station where this board is open.` Pending items are not repeated by themselves after the new sign-in.

**Owner lock-out (accepted):** while an agent types on a station, a board on that same machine cannot act; the owner revokes, stops or closes from another device (or after the 60 s tail). A deck behind a proxy sees every board with the proxy's address, so every board is signed out during input (fail closed); `operations.md` states this and recommends the direct tailnet address.

**Residual (P7-OD1):** a browser on the target machine that reaches the deck through an address not in the snapshot (a proxy or VPN that the station does not report, a NAT shared with another machine is the opposite case and signs out too much) keeps its session. `operations.md` will say: do not keep a board signed in on a station while an agent types on it. The sign-out protects decisions that go through this deck only.

**Implementation decisions (from the code map, `p7-code-map.md`):**
- **Startup order:** today the deck binds the board HTTP server and starts the link node before `captures_prepared_to_unknown` and `desktop_ops_recover` (`crates/fleetdeck-deck/src/serve.rs`). P7 moves those two store calls to right after `DeckStore::open`, before `DeckNode::start_with` and `BoardServer::bind_with`. They are store-only and need neither. So the exposure rows are final before any board request or station link is accepted.
- **Migration V37 (deck):** `ALTER TABLE coordinator_desktop_ops ADD COLUMN attempted_at INTEGER` and `ADD COLUMN finished_at INTEGER` and `ADD COLUMN exposure_addrs TEXT` (all nullable; the table has only `created_at` and `updated_at`, and the recovery overwrites `updated_at`); `desktop_op_attempted` sets `attempted_at`; `desktop_op_finish` and `desktop_ops_recover` set `finished_at` only when it is NULL; `ALTER TABLE login_sessions ADD COLUMN last_addr TEXT` and `ADD COLUMN last_addr_seen_at INTEGER`; the same two columns on the member session table. The input class and the input caps of §3 are in the same V37. `DECK_LATEST` becomes 37. No earlier migration changes.
- **Where the rule runs:** in the board guard (`crates/fleetdeck-deck/src/http/guard.rs`, the `Ok(Some(principal))` arm, where the method, the principal and the `PeerAddr` extension are all known). It runs for every authenticated `/api/*` request, mutating or not, including the socket upgrades (`/api/ws`, `.../shells/{shell}/attach`): rule 2 deletes the session of any authenticated request from an exposed address. The address of a request is `PeerAddr` (the immediate TCP peer; the deck terminates TLS itself), in the canonical form above.
- **Sockets (one admission step):** the deck keeps one registry of board sockets and shell-attach sockets, guarded by one mutex. An entry is added in the guard, before any await of the upgrade handler (state `pending`, with the session hash, the kind and the canonical peer address, and a shared `revoked` flag). The sign-out of rule 1 or 2 takes the same mutex, deletes the session rows, sets `revoked` on every entry of a deleted session (pending or live) and collects their cancellation handles. At `on_upgrade`, under the mutex, the handler checks `revoked` and that the session row still exists; if either fails, it closes the socket at once and never starts the relay; else the entry becomes `live`. Each relay checks `revoked` before it forwards each frame to the station (shell bytes) or to the board, and ends when it is set. The input dispatch of rule 1 waits until every collected relay has acknowledged its end (at most 2 s; a relay that does not acknowledge in time fails the operation with `input_interrupted` before any input). Board socket closes use code 4401 with the reason `signed_out_for_input`. The existing 30 s recheck stays as a second barrier.
- **`host_addresses`:** a new optional field of `DesktopInfo` (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`), sent only by a station with `desktop_input_mcp.v1`. The deck keeps it with the station info and removes it from the board view of the station (`local.rs` `station_view`).
- **The local station:** the deck store gets the station ID of `fleetdeck up` (today only `Board.local_station`), so the input path can add the loopback and deck addresses.

**Tests:** a migrated session (V37 deletes it); a sign-in from the exposed address during exposure and the tail is refused and gets no cookie; a shell attach paused before `on_upgrade` while the sign-out commits never forwards a byte; a session with an old `last_addr` on the target address is deleted; a session with a live connection from the target address is deleted before the dispatch; a delayed approval with the old cookie after exposure is refused; an approver member on the target address is signed out; a restart during exposure and during the 60 s tail; a sign-in during the tail is signed out at its next request; IPv4-mapped IPv6 and IPv6 canonical comparison; a session on another address is untouched; the audit row.

## 8. Execution, disclosure and results

### 8.1 Runner

Extend the existing desktop process facilities with a narrow sensitive-stdin operation.

Requirements:

- Fixed resolved executable.
- Fixed argv construction.
- Existing cleared desktop child environment.
- Explicit usable UTF-8 locale.
- Private stdin pipe for text.
- No text in argv, environment, temporary text files or tracing.
- Bounded output handling.
- No raw stdout/stderr in errors.
- Owned child cancellation, kill and reap.
- No detached input process.
- No program fallback.

The runner may retain transient text in process memory. It does not promise zeroization or protection against the owner UID.

Missing `wtype` refuses keyboard/text tools. Pointer movement does not require `wtype`.

`doctor` and owner desktop readiness report missing `wtype`. Dependency remediation must use an authorized remote management path; the P7 workflow must not instruct the owner to approve something on the station desktop.

### 8.2 Target disclosure

Under **P7-OD6**, send this normal-urgency, actionless notification before each new execution:

> Fleet Deck is sending keyboard input for `<callsign>` to an agent preview.

For pointer movement:

> Fleet Deck is moving the pointer for `<callsign>` in an agent preview.

Use the authenticated P6 caller label. Do not include text, chord, coordinates, title or URL.

Notification failure prevents focus and injection. Do not disable DND or treat successful notification delivery as evidence that the owner saw it.

These disclosures:

- do not consume the user `notify` rate;
- cannot be called independently;
- occur once per actual execution attempt, never on duplicate/status lookup;
- do not require another approval.

### 8.3 Results

Use the existing P6 non-capture operation status shape.

Confirmed completion has:

```json
{
  "state": "done",
  "result": {}
}
```

Together with the usual authorized request, operation and station identity.

`done` means the tool completed its invocation and required observations without a detected failure. It does not mean the application performed the intended action.

Before execution starts, a definitive refusal is `failed`.

If input started and interruption or failure is detected, retain the fixed failure code and its partial-input warning. `failed` does not mean zero effects.

If the station or deck cannot establish the result, use P6 `unknown`. A timeout must not be converted into “nothing happened.”

Return no:

- text echo or chord;
- cursor coordinates;
- window title, address or PID;
- raw desktop snapshot;
- screenshot;
- stdout/stderr;
- application success inference.

Before result delivery, apply DL-242’s final deck-local delivery pass, including idle expiry, and the caller’s `Rechecked` wrapper. Preserve durable results even when delivery is refused.

## 9. Budgets, rates and duplicate handling

This section requires **P7-OD5**.

### 9.1 Lifetime budgets

| Field | Opted-in default | Range | Charge |
|---|---:|---:|---|
| `input_cap` / `input_used` | 100 | 0–5000 | One per newly accepted input mutation |
| `input_character_cap` / `input_character_used` | 20,000 | 0–1,000,000 | Exact scalar count of accepted `type_text` |

Each accepted input mutation also consumes **one P6 desktop action**.

Key chords and pointer movement consume no character budget.

Input does not consume capture, recording or image-read budgets. Screenshots remain separate charged operations.

Counters survive restart, renewal, receipt pruning and preview closing. No refund follows focus failure, missing tools, cancellation, timeout, partial input or ambiguous delivery.

Expose both P7 limits and usage on grant rows.

Provide an owner board “Change limits” path for these fields in this build. Increases require elevation; decreases do not. Limits may be reduced below usage, preventing further acceptance without rewriting usage.

### 9.2 Admission order

Extend the built P6 transaction in this order:

1. Recheck effective grant, binding, classes, allowance, scope and target.
2. Resolve accepted and rejected duplicates.
3. Enforce the existing 10,000-attempt ceiling.
4. Enforce shared and input rates.
5. Check budgets, in order: desktop, input count, input characters.
6. Atomically commit operation, charges, rate entries and acceptance audit.

Preserve built P6 semantics:

- Duplicate accepted request: no new charge or effect.
- Duplicate stored budget refusal: same refusal after current authorization.
- Same request key with changed normalized payload: conflict.
- A stored budget refusal consumes an attempt.
- Rate/capacity refusals do not become accepted operations.
- No database transaction spans a network await.

The operation key remains:

```text
(grant_id, SHA-256(request_id))
```

The shared request namespace includes P6, recording and P7 tools.

### 9.3 Rates and capacity

Retain all P6 limits, including:

- 30 newly accepted desktop mutations per grant per rolling 60 seconds.
- Two mutations in flight per grant.
- 64 desktop calls/read permits in flight on the deck.
- Existing MCP eight concurrent requests and twenty requests/second per binding.

Add:

| Resource | Limit |
|---|---:|
| Newly accepted input mutations per grant | 20 per rolling 60 seconds |
| Newly accepted input mutations per caller session, across grants | 20 per rolling 60 seconds |
| Active P7 input operations per target station, across callers/decks | 1 |

Charge input rate entries atomically with acceptance, alongside the shared mutation bucket.

Reject occupied capacity immediately. Do not create a waiting input queue.

Persist accepted rate timestamps. Restart, renewal and backward clock movement must not replenish capacity.

## 10. Deadlines

All stage limits consume the enclosing operation’s remaining budget.

| Stage | Limit |
|---|---:|
| Caller-policy request | P6 3 s |
| Target-policy request | P6 3 s |
| Execute authority check, including caller policy | P6 5 s |
| Authority execution-start lifetime | Existing 10 s, wall/monotonic/epoch rules |
| Individual preparation command | At most existing 5 s |
| Final active-window query | 250 ms |
| Final authority-answer age at injection | 500 ms |
| Final focus-snapshot age at injection | 100 ms |
| `wtype` invocation | 15 s |
| Pointer dispatch | 1 s |
| Child kill/reap cleanup | 2 s |
| Complete station input handler | 30 s |
| Deck wait | 35 s |
| Deck handler | 40 s |
| Caller MCP response | 45 s |
| Action-status query | P6 deck/MCP 10/15 s |

Both focusing and injection must start within the original authority start lifetime. Disclosure or focusing does not buy a fresh ten seconds.

Once a `wtype` invocation starts, its process deadline governs completion; do not expire it solely because the original start deadline subsequently passes.

The 2,000-character limit is a maximum accepted payload, not a promise of completion on every compositor. A slow or interrupted invocation may produce partial text.

Timeout, cancellation, disconnect and uncertain outcome never initiate another injection.

## 11. Durable state and recovery

Reuse P6’s deck states:

```text
prepared → attempted → done | failed | unknown
```

Persist only the necessary private operation metadata:

- Request-key digest and payload hash.
- Coordinator provenance and binding generation.
- Target station.
- Tool/class and action hash.
- Validated owned `window_id`.
- Length and charges.
- State and fixed code.

Do not persist text, chords, coordinates or serialized input actions in operation records, rejection records, outboxes or recovery jobs.

The live task holds the payload. Restart cannot reconstruct or resume it.

Target claims survive restart and prevent repeating an operation. Persist a claim before any user-visible execution step.

Recovery:

- Prepared operations abandoned by process loss fail without dispatch.
- Attempted operations lacking a definitive outcome become unknown.
- Reconciliation performs status lookup only.
- Terminal identities survive pruning sufficiently to prevent re-execution.
- No automatic retry with either the same or a new request ID.
- A duplicate cannot regain authority after revocation.
- Current authorization governs status delivery.

## 12. Audit and owner visibility

Add audit actions:

```text
desktop.key_press
desktop.type_text
desktop.pointer_move
```

Use stages:

```text
accepted
disclosure
focus
injection
completion
delivery
```

Record applicable acceptance/refusal and terminal outcomes durably and idempotently.

Allowed fields:

```text
actor = coordinator
grant ID
binding generation
caller station/session IDs
target station ID
deck-generated operation ID
validated owned window_id
action kind
length
stage
fixed result code
charged counters
timestamps
```

Length means one chord, text scalar count or zero for movement.

Never record:

- Typed text, prefixes, excerpts or character samples.
- Chord or modifiers.
- Pointer coordinates.
- Raw request ID.
- Payload/authority hashes.
- Titles, URLs, native addresses or PIDs.
- argv, stdin, raw command output or stderr.
- Credentials or binding secrets.

For unresolved/foreign handles, omit the supplied handle and record the fixed code.

A required acceptance/attempt record must commit before execution. Audit failure prevents unstarted work. Failure to record completion never causes replay.

Board visibility includes:

- Input-enabled grant chip and fixed scope.
- Grant stations.
- Input and character usage.
- Expiry and remaining desktop budget.
- Recent input kinds, lengths and fixed outcomes.
- Explicit unknown/partial-outcome text.
- Revoke.
- Deck desktop/fleet kill switches.
- Target input kill switch.
- Existing owner preview close.

Do not display the supplied text in Fleet Deck’s input activity UI. Provider-side conversations and application content remain outside this audit guarantee.

## 13. Errors and exact texts

Retain P6 framing: recognized execution errors return `isError: true` with one JSON text block containing fixed `error` and `message`, plus an authorized operation ID when available.

New codes:

| Code | Exact text |
|---|---|
| `input_not_allowed` | `This grant does not allow desktop input.` |
| `input_scope_unsupported` | `This build supports input only in this caller's preview windows.` |
| `input_tool_missing` | `The keyboard input tool wtype is not installed on the target station.` |
| `input_tool_unsupported` | `The target desktop cannot use the configured keyboard input tool.` |
| `input_board_forbidden` | `Fleet Deck board and wall windows cannot receive agent input.` |
| `input_cap_reached` | `This grant has used its input action limit.` |
| `input_characters_reached` | `This grant has used its typed-character limit.` |
| `input_destination_unavailable` | `The pointer destination is not available inside this preview.` |
| `input_focus_unknown` | `The preview's input focus could not be verified. Input was not started.` |
| `focus_changed` | `The preview no longer has focus. Input stopped; some input may already have been sent.` |
| `input_interrupted` | `Input was interrupted. Some input may already have been sent. Do not retry automatically.` |
| `signed_out_for_input` (board API, 401, §7.6) | `You were signed out because an agent typed on the station where this board is open.` |
| `input_failed` | `The input tool failed. Some input may already have been sent. Do not retry automatically.` |
| `input_timeout` | `Input did not finish in time. Some input may already have been sent. Do not retry automatically.` |

Reuse these P6 codes and texts unchanged:

- `not_bound`, `not_granted`, `binding_stale`.
- `invalid_params`, `request_id_conflict`.
- `desktop_cap_reached`, `request_budget_reached`, `rate_limited`, `busy`.
- `authority_lost`, `authority_unknown`.
- `desktop_window_not_found`.
- `fleet_control_off`, `desktop_control_off`.
- `desktop_forbidden`, `desktop_unsupported`, `desktop_failed`.
- `station_unreachable`, `desktop_timeout`.

Use existing `screen_locked` text:

> The screen is locked or its lock state is unknown.

Rules:

- Missing class: `not_granted`.
- Required input class present but allowance absent: `input_not_allowed`.
- Unsupported scope in an owner grant request: `input_scope_unsupported`.
- Scope supplied as an unknown tool field: `invalid_params`.
- Malformed chord/text: `invalid_params`.
- Unknown and foreign handles: identical `desktop_window_not_found`.
- Detected focus loss before injection: `focus_changed`, with zero input started.
- Unknown focus before injection: `input_focus_unknown`.
- Any post-start error may involve partial input.
- Do not infer unsupported virtual-keyboard protocol by matching arbitrary stderr; use a verified readiness probe or the generic fixed input failure.
- Do not expose a target setting value unless the trusted handler directly checked it.

Unknown durable outcome carries this fixed explanatory text:

> The input outcome is unknown. Inspect the preview before issuing another input request.

## 14. Version and compatibility behavior

1. Existing grants receive no allowance or new tools.
2. False/absent P7 fields preserve legacy start bytes.
3. Unknown class, scope or internal class fails closed.
4. Input-enabled grant creation requires current links and all required features on the caller and every target.
5. Missing support returns stored owner-creation `unsupported` using P6 behavior.
6. Never send P7 fields or action variants to an older strict decoder.
7. Never strip input or coordinator context to make an operation compatible.
8. Owner/wake action hashes remain byte-for-byte unchanged.
9. P6 policy requests and answers retain their existing bytes.
10. Later downgrade makes listed tools fail explicitly; the catalog remains fixed.
11. Reconnect cannot add input to a running provider process.
12. Renewal cannot introduce input allowance or reset usage.
13. P6-only screenshot, preview and recording behavior remains unchanged.
14. No new MCP protocol version is introduced. Test all three currently supported versions.
15. Missing local `wtype` is runtime unavailability, not permission to substitute another backend.
16. Presence of `ydotool` or `wlrctl` never advertises click support.
17. A future click implementation requires another reviewed contract and independently negotiated feature; it must not appear automatically under this feature.

## 15. Live spike and operational acceptance

Fake-desktop proofs validate program behavior under controlled observations. They do not prove compositor input delivery.

Under **P7-OD2**, implementation may precede the live spike, but owner settings default off and the board labels the feature experimental.

Before relying on it, run a remotely initiated, owner-authorized smoke on each intended station configuration. All approvals and settings remain on the board.

Verify and record:

1. Exact Omarchy, Hyprland, Chromium and `wtype` versions.
2. Required virtual-keyboard support and actual seat selection.
3. Targeting when several compositor instances or seats exist; ambiguity must refuse.
4. Focus behavior with tiled/floating previews, multiple monitors, special workspaces, dialogs and overlays.
5. Focus-following-pointer behavior and cursor warping.
6. Final-query latency and authority/focus age bounds.
7. Physical keyboard/pointer interference before and during injection.
8. Focus loss during a long text invocation.
9. UTF-8 stdin, literal option-looking text, quotes and non-ASCII text.
10. Owner keymaps, accented text, combining characters, non-Latin text, modifier state and application shortcuts.
11. A 2,000-scalar payload, including completion time and partial-failure behavior.
12. Child termination and modifier/key release after cancellation or process death.
13. Pointer logical coordinates under scaling, monitor offsets, rotation, overlap and layers.
14. Board detection for wall classes, ordinary browser board windows and previews navigated to the board.
15. The acknowledged false-negative case where current board origin cannot be determined.
16. Locking during preparation and injection.
17. Revocation, expiry, kill switches, link loss and owner preview close.
18. Notification success, failure and DND behavior.
19. Claude and Codex discovery, fixed catalog, operation status and refusal after revocation.

Use dummy preview content and a dummy approval surface. The smoke must not test by approving a real dangerous operation.

Record empirical observations separately from guarantees. Passing a smoke does not eliminate the focus race.

The click spike must determine whether an unprivileged, dependency-compatible backend is available and what targeting it actually supports. P7 installs no candidate click helper and chooses no fallback.

If the required keyboard or focus behavior fails, retain the tested foundation and keep operational input disabled. Do not widen scope or relax checks to obtain a passing smoke.

## 16. Tests: the four proofs

Use the existing station fake desktop and deck `p6_support` over real authenticated links. Extend the fakes with controllable active-window state, geometry, layers, focus events, stdin capture, stalled children and stage barriers.

### 16.1 Behavior proof

1. Grant creation defaults, explicit opt-in, elevation, prerequisite classes and station membership.
2. Board-only enablement and disablement.
3. Complete schema/catalog matrix, including unknown-field rejection.
4. Start-time internal class derivation; direct internal-class injection refused.
5. Local and remote targeting, including a headless caller.
6. Exact key allowlist, modifier order and fixed argv.
7. Text scalar/byte boundaries, supplementary characters, combining characters and forbidden controls.
8. Literal dash/option-looking text travels only through stdin.
9. No trimming, normalization or appended Return.
10. Pointer coordinate conversion, bounds, overflow, scaling fixtures and negative monitor origins.
11. Pointer overlap/layer refusal.
12. Owned preview identity across renewal; new-grant and foreign-handle refusal.
13. Required notification, focusing and fresh injection-check ordering.
14. Each accepted input consumes desktop/input budgets and appropriate characters.
15. Duplicate requests execute and charge once.
16. Status and final result use P6 shapes without text echo.
17. Owner preview close interrupts input without waiting for its slot.

### 16.2 Failure proof

18. Revoke, idle-expire, absolute-expire or change generation at every awaited boundary.
19. Toggle every applicable deck/caller/target gate before admission, after policy, after acceptance, after focus and before injection.
20. Hold caller policy publication while changing its live policy; the round trip still refuses.
21. Exercise the `Start::begin` cache explicitly: a passing disclosure/focus check cannot substitute for injection authorization.
22. Change focus before the final snapshot: no injection.
23. Change focus after the snapshot and during a fake child: interruption is recorded; tests do not claim zero misdelivery.
24. Malformed/empty/stalled active-window, geometry, layer and lock observations fail closed.
25. Exceed each age/deadline using controlled clocks.
26. Missing `wtype`, unsupported readiness, spawn failure, pipe failure, nonzero exit, hang and cancellation.
27. Notification failure prevents focus/injection.
28. Compositor restart, event gap, preview close, reused address and registry eviction.
29. Concurrent input across grants/decks reaches only one station slot; no queue.
30. Crash at prepared, attempted, claim, focus, injection and completion boundaries; never replay.
31. Audit/DB failure before effect prevents effect.
32. Final budget-slot races, stored budget rejections and rate boundaries.
33. Restart, renewal, pruning and backward clocks do not replenish usage.
34. Final delivery authority loss suppresses the response while preserving bookkeeping.

### 16.3 Exposure proof

35. Distinct sentinels for typed text, chord, coordinates, URL, title, stderr, caller and unrelated windows.
36. Text appears only in the intended private fake stdin capture; never in production-facing argv, environment, records, audit, logs, errors or responses.
37. Accepted/rejected operation storage contains no serialized payload.
38. Unknown and foreign handles have identical responses.
39. Board classification precedes focus for an owned board-like preview.
40. A foreign active board produces generic focus loss without identifying the foreign window.
41. Results contain no raw `DesktopActResult.info`, titles, addresses, PIDs or input echo.
42. Ordinary sessions, owner actions and legacy routes cannot invoke P7 variants.
43. Click, scroll, held keys, arbitrary commands and scope overrides remain unavailable.
44. Document the unobservable post-check race and same-UID limits; do not turn them into passing isolation assertions.

### 16.4 Version proof

45. Old/new caller, deck and target combinations.
46. Downgrade after grant creation, acceptance, focus and before injection.
47. Legacy grant/start and owner/wake hash byte fixtures.
48. Unchanged P6 policy wire fixtures.
49. V36→V37 migration preserves recordings, operations, rejected identities, rates and counters.
50. Old grants remain disabled; unknown scope/class fails closed.
51. Fixed catalog across reconnect, renewal and switch changes.
52. All supported MCP protocol versions encode fixed success/error/status results correctly.
53. End-to-end remote key, text and pointer calls through both links, plus revocation and duplicate suppression.
54. Live-smoke results remain separately marked pending until observed on the owner’s stations.

## 17. Build task split

| Task | Deliverables |
|---|---|
| **Foundation** | Shared grammar and validators; schemas; feature constant; typed actions/calls; error allowlists; grant/spec fields; V37; generated types; compatibility fixtures. No executable input path before reviewed authority wiring. |
| **Deck** | Grant creation and cap-edit routes; feature validation; private P6 admission extension; counters/rates; action construction/hashing; live input-policy query; execute checks; durable status/recovery; final delivery pass; redacted audit. |
| **Station** | Input policy setting; internal catalog class; forwarding; sensitive-stdin runner; preview resolution; board detector; station input slot; focus protocol; explicit execute rechecks; cancellation; durable claims; readiness/doctor reporting. |
| **Board** | Elevated opt-in and disclosure; prerequisite validation; fixed scope; caps at creation and editing; usage/activity; input kill switch; experimental state; preview-close integration. |
| **Proofs** | Shared fake extensions; station unit tests; deck admission/migration tests; board tests; real-link end-to-end four proofs; sentinel redaction checks. |
| **Docs** | Accepted owner choices; parent supersession and phase-order change; decisions log; operations, LIVE-SMOKE, PARITY and SECURITY-CHECKLIST updates; actual live limitations. |

Keep every changed source module below 800 lines. Existing large modules must be split before extension.

Suggested station modules separate grammar/argv, sensitive runner, focus snapshots, board classification and input execution. Keep P7 schema and accounting additions separate from already large P6/recording modules where needed.

Run targeted checks through the required queue, for example:

```sh
umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh cargo check -p fleetdeck-station
```

Run relevant targeted nextest and web tests through the same queue. The implementation coordinator runs `make check`.

## 18. Acceptance gates

Before code:

1. Record **P7-OD1 through P7-OD7**.
2. Reconcile the parent’s board-refusal wording with the accepted guarantee.
3. Amend the parent’s spike/review ordering if P7-OD2 is accepted.
4. Obtain adversary DESIGN REVIEW PASS.

Before merge:

1. Complete the four proofs.
2. Complete required project checks.
3. Obtain adversary PRE-PR PASS.
4. Preserve experimental/default-off status and list pending live checks.

Before operational reliance:

1. Complete the live spike on the intended station configurations.
2. Record failures and supported configurations.
3. Obtain the owner’s acceptance of the observed behavior and remaining limitations.
4. Enable input remotely through the elevated board setting and grant flow.

Pointer click remains deferred after these gates.

