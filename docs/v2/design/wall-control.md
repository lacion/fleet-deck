# More control of the wall: controls, a remote, wall pages and wall apps (B17)

- **Status:** Draft, revision 1 (2026-10-23), design agent of track T7 (`wall-control`). Not accepted. The architect decides; an adversary DESIGN REVIEW is required before the build (risk **high**, §5).
- **Owner request (B17):** scroll the wall, move between tiles and items, open and close views; load a custom URL on the wall; a "remote desktop" for the wall browser only; AI-generated mini UIs on the wall.
- **Base:** v2 `497b2fbc`. Every file:line in this document is on that commit.
- **Parents:** [omarchy.md](./omarchy.md) §4, §5, §5A–§5D, §8.4; [omarchy-p6.md](./omarchy-p6.md) §4.2; [omarchy-p7.md](./omarchy-p7.md) §7.2; [omarchy-p7b.md](./omarchy-p7b.md) §8. Sibling track: B15 (`mcp-apps.md`, not written when this draft was made; §3.4 states the interface that this track needs from it).
- **Scratch and recordings:** `~/fleetdeck-briefs/backlog/wall-control/` (`live-dev1-2026-10-23.md`).

---

## 1. The problem and the verified facts

### 1.1 The problem

The wall is a read-only command center. The owner can change only **what** it shows (a directive), and only to one of six Fleet Deck views. The owner cannot scroll, cannot pick an item from another device, cannot show a page that is not a Fleet Deck view on the wall, and cannot show a live, agent-made UI there.

### 1.2 Verified facts (v2 `497b2fbc`)

**The wall and its document**

1. The wall is the board route `#/wall` (`web/src/app/routes.ts:22-41`, `parseWall` `:175-214`). A wall display does not use the board document. It loads `/display` (`crates/fleetdeck-deck/src/http/display_document.rs:1-34`), which the deck serves with a stricter policy: `frame-src` allows only `/api/displays/self/artifact/` of the request origin (`crates/fleetdeck-deck/src/http/assets.rs:44-56`). The board policy is `frame-src 'self'` (`assets.rs:39-41`).
2. The display page runs `DisplayRuntime` (`web/src/display/DisplayRuntime.tsx:92-180`). It shows only the wall, and any other hash goes back to `#/wall` (`:99-102`).
3. The kiosk is `chromium --app=<url> --user-data-dir=<data>/wall-<display_id> --class=fleetdeck-wall-<id> --no-first-run --kiosk` (`crates/fleetdeck-station/src/desktop/actions.rs:575-602`). The profile holds the `fd_display` cookie (`crates/fleetdeck-deck/src/http/guard.rs:48`).

**Directives**

4. The backlog shape `display.directive { display_id, mode, session?, artifact_url?, … }` is **out of date**. The built body is a tagged enum with one variant for each mode and typed references, never a URL: `DirectiveBody::{Mission, Focus, Review, Race, Artifact, Ambient}` (`crates/fleetdeck-proto/src/display_directive.rs:61-108`, `deny_unknown_fields`, a test refuses `url` at `:169`).
5. The deck stores one directive for each display in `display_directives` (V25). Its `mode` column has `CHECK (mode IN ('mission', 'focus', 'review', 'race', 'artifact', 'ambient'))` (`crates/fleetdeck-deck/src/store/display_v25.rs:32-41`). V34 added `sender_generation`, `sender_station`, `sender_session` (`crates/fleetdeck-deck/src/store/coordinator_desktop/schema.rs:54-56`). **A new mode needs a table rebuild migration.**
6. The wall reads the directive with a poll every 2 s: `GET /api/displays/self/directive` (`web/src/display/useDirective.ts:11`, `:47-61`; route `crates/fleetdeck-deck/src/http/display_directive_routes.rs:151-171`). The wall applies a directive once for each generation.
7. Senders (`crates/fleetdeck-deck/src/store/display_directives.rs:40-45`):
   - the owner: `POST /api/displays/{id}/directive`, scope Drive, owner only (`display_directive_routes.rs:131-149`; table row `crates/fleetdeck-deck/src/http/methods_table_display.rs:16-22`);
   - the bound station: link method `display.directive` with the intents `next_needs_you` and `next_mode` only, feature `desktop_shell.v1` (not `desktop.v1` as §8.4 of the parent says), 2 calls per second (`crates/fleetdeck-deck/src/link/display_directive.rs:1-12`, `:29-31`, `:43-50`);
   - a coordinator with the `desktop` class: the MCP tool `wall_show`, only `focus` on its own session or `artifact` on its own artifact (`crates/fleetdeck-deck/src/coordinator_desktop/wall.rs:44-60`).
8. No owner route reads the current directive of a display. `DisplayView` has no directive field (`web/src/gen/display/DisplayView.ts`). The parent §5.2 ("the Displays list shows … the last directive") is not built.

**Keys, scroll and selection on the wall**

9. The wall keys are a capture listener on `document`: `Esc` → Mission, `←/→` → the next or previous mode, `Enter` on a focused tile → focus. Any key wakes the ambient mode. Modifier keys do nothing (`web/src/wall/WallPage.tsx:181-220`). `Tab` is the browser's own focus move. The keys work only when the kiosk window has the keyboard focus.
10. **Nothing on the wall scrolls from a key or a directive.** The focus chat is `overflow: hidden` with `justify-content: flex-end`: older lines are cut and fade out, and no input can bring them back (`web/src/styles/wall.css:1123-1131`). Only the change panes have a scroll container (`.fd-wall-scroll`, `overflow: auto`, `wall.css:1324-1328`; `web/src/wall/DiffPane.tsx:147`, `web/src/wall/displayChanges.tsx:179`), and it scrolls only with a pointer at the kiosk. A phone in portrait scrolls the whole grid (`wall.css:1500-1505`).
11. The wall has no selection. A tile is "focused" only through the browser focus (`WallPage.tsx:199`, `:209`).

**Artifacts on the wall**

12. The backlog says the artifact mode uses "its `/a/{token}/` URL". That is **wrong for a display**. A display reads `GET /api/displays/self/artifact/{station}/{id}/{version}/_` (`crates/fleetdeck-deck/src/http/display_artifact.rs:51-57`). The answer has `INERT_CSP` = `sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src data:; font-src data:` (`:47-49`), and the pane puts HTML in an `<iframe sandbox="">` (`web/src/wall/DisplayArtifactPane.tsx:156-164`). **A script in an HTML artifact never runs on a wall.** An agent-made HTML artifact on the wall is not only "static with no bridge": it is inert. The owner's own `#/wall` uses the `/a/` link (`web/src/wall/ArtifactPane.tsx:29`).
13. The parent cannot scroll the content of an opaque-origin frame. So a long HTML artifact on a wall cannot be scrolled by any control today.

**Pages that are not Fleet Deck views**

14. The backlog says the owner "cannot show a page that is not a Fleet Deck view". **Partly wrong.** The board Desktop panel has "Open a link" (`web/src/desktop/DesktopForms.tsx:151-196`): `desktop.act` `browser_open` opens any `http`/`https` URL (grammar `crates/fleetdeck-proto/src/desktop_text.rs:132-155`) in a fresh Chromium profile window on a chosen monitor of a desktop station. It is a separate window, not a wall mode, and the owner cannot scroll it from the board.

**Pixels and input (P5a, P6, P7)**

15. The owner can take a screenshot of a monitor from the board (P5a, `web/src/desktop/capture.ts:1-30`). A coordinator can too (P6 `screenshot`), and it reads the image through `read_capture_image` with a 700 KiB limit (`omarchy-p6.md` §4.9).
16. Live check on dev1 (2026-10-23, recording `live-dev1-2026-10-23.md`): one PNG of the 2880×1800 monitor `eDP-1` was **867,171 bytes**. `read_capture_image` refused it with `image_too_large`. A P6 screenshot is one still image for each call, not a stream. dev1 has **no wall display bound** (`desktop_info` → `walls: []`), so no live wall could be observed.
17. P7 and P7b input refuses every wall window by design: the board barrier refuses `Kind::Wall` and the classes `fleetdeck-wall`, `fleetdeck-wall-*` (`crates/fleetdeck-station/src/desktop/input/barrier.rs:1-10`, `:67-72`, `:136`; `omarchy-p7.md` §7.2; `omarchy-p7b.md` §8). P7 is coordinator-only. "P7 input limited to the wall window" is therefore the opposite of the built rule, not a narrowing of it.

**Compatibility hooks that already exist**

18. Feature declaration from a board: the header `X-FleetDeck-Features` and, for a WebSocket, the query `features=` (`crates/fleetdeck-deck/src/http/artifact_projection.rs:1-56`; the board sends `artifacts_video.v1` at `web/src/runtime/connection.ts:40-46`). The declaration grants no authority.
19. A board WebSocket message of an unknown `type` makes the client throw and reconnect (`web/src/runtime/decode.ts:105-106`, `web/src/runtime/connection.ts:268-293`). **A new server frame must only go to a socket that declared it.**
20. A display socket may only subscribe and unsubscribe; every other client message gets `display_read_only` (`crates/fleetdeck-deck/src/http/ws.rs:639-643`).
21. `GET /api/features` answers `feature::ALL` to the owner (`crates/fleetdeck-deck/src/http/features_routes.rs:27-30`; list `crates/fleetdeck-proto/src/version.rs:305-325`, which already holds the board-side feature `artifacts_video.v1`).
22. The next free migration slot is V48 (the last is V47 `secret_files::SQL`, `crates/fleetdeck-deck/src/store/schema.rs:787-789`). Other backlog tracks can also take V48: the build uses the next free number at build time.

### 1.3 Corrections to the backlog (B17)

| Backlog text | Correction |
| --- | --- |
| "`display.directive { display_id, mode, session?, artifact_url?, batch_id?, pin?, filter? }`" | A tagged enum with one variant for each mode and typed references, never a URL (fact 4). |
| "the bound station … (link method `display.directive`, feature `desktop.v1`)" | Feature `desktop_shell.v1`; only the intents `next_needs_you` and `next_mode` (fact 7). |
| "The `artifact` mode shows one artifact … through its `/a/{token}/` URL" | A display uses the display artifact route with an inert policy; scripts never run (fact 12). |
| "cannot show a page that is not a Fleet Deck view" | The Desktop panel "Open a link" opens any web page in its own window on a desktop monitor (fact 14). It is not a wall mode and has no remote scroll. |
| "P7 … input … not limited to the wall" | P7 refuses wall windows on purpose (fact 17). |
| (not in the backlog) | The wall cannot scroll at all, not even with a pointer in the focus chat (fact 10). |
| (not in the backlog) | No owner route reads a display's current directive (fact 8). |
| (side finding, not this track) | `desktop_info { station: "dev1" }` answers `not_granted`, and the same call with the station id works. `fleet_view me` lists the grant stations by name. The desktop tools match station ids only. Note it for the MCP track. |

---

## 2. Answers to the questions of B17

### 2.1 "Wall control: new directive kinds, or a narrow input channel that only the wall page reads? Who may send them?"

**Answer: a narrow, typed control channel that only the wall page reads. It is neither a directive nor synthesized input.**

- A directive is durable state: "what the wall shows". The deck stores it, checks it on every read and restores it after a reload (facts 5, 6). A scroll or a "next item" is a transient step. It has no meaning after a reload, and storing it would cost a migration, an audit row for each key and a 2 s poll delay.
- Synthesized input (keys into the kiosk) is the P7 path. P7 refuses wall windows (fact 17), and key synthesis cannot be tied to the wall document.
- A control is a closed enum with no string field (`scroll`, `select`, `open`, `back`, `mode`, §3.1.1). The deck relays it to the display's own WebSocket in about one round trip. The wall applies it to its own local state, in the same way as its keys (fact 9). A control can only move inside what the display may already show: it cannot widen the scope, name a session, or load anything.
- **Senders: the owner only** (board and phone are the same owner login), scope Drive, owner only, the same as a directive (fact 7). Not the display itself (read-only by construction), not members or share guests, not coordinators in this track (they already choose what the wall shows with `wall_show`; an agent that scrolls a wall has no clear use), not the station hotkeys in this track (owner question Q5).

### 2.2 "Custom URLs: a new `url` mode in a sandboxed frame? Which URLs? The wall principal and the kiosk profile must not leak."

**Answer: a new `page` mode that names an entry of an owner allow-list ("Wall pages") by id. The wall shows the page in a credentialless, sandboxed frame. The browser enforces the allow-list at the origin level through the display document's `frame-src`.**

- **Which URLs:** only the entries that the owner adds on the board (with elevation). `https` for any host; `http` only for loopback, private networks, the tailnet range and `.local`/`.ts.net` names (§3.2.3). Never an origin of the board itself. At most 50 entries.
- **Why a list and not "any URL":** the page runs as the owner's UID on the wall machine, in that machine's network position, and on a screen that people trust. A free URL in a directive would make every directive sender a page loader. The list moves the decision to one elevated owner act, and the directive stays a typed reference by id (the rule of §5A item 3: "never a URL").
- **No leak of the wall principal or the kiosk profile** (the full list is in §5.2):
  1. `credentialless` frame: the page gets no cookie and no stored state of the kiosk profile, and its own storage is thrown away. A browser without `credentialless` does not show the page at all.
  2. The deck refuses an entry whose origin is a board origin, and the wall checks it again at render time. A same-origin frame with scripts could reach the wall document.
  3. `sandbox="allow-scripts allow-same-origin allow-forms"` with no `allow-top-navigation*`, `allow-popups`, `allow-modals` or `allow-downloads`: the page cannot move the kiosk away from the wall or open a window in the kiosk profile.
  4. `allow=""` (no camera, microphone, geolocation, fullscreen or local-network access) and `referrerpolicy="no-referrer"`.
  5. The wall ignores every `message` event from a page frame.
  6. The display document's `frame-src` lists only the allow-list origins: the page can navigate inside its own origin, and the browser blocks a navigation or a redirect to any other origin.
- **Sites that refuse to be framed** (`X-Frame-Options`, `frame-ancestors`; for example GitHub, and Grafana by default) show their own error inside the frame. For them the owner keeps "Open a link" (fact 14), which opens a separate window with a fresh profile. The Wall pages section says this.

### 2.3 "Remote control of the wall only: a live view (P6 screenshots or a stream) plus input that P7 limits to the wall window, or a board-side mirror?"

**Answer: the board-side mirror. Recommended and designed in §3.3. The pixel and input path is rejected for this track.**

| Criterion | A. Mirror (route + directive + controls, no pixels) | B. P6 screenshots or a stream + P7 input on the wall window |
| --- | --- | --- |
| What the owner sees | The same `WallPage` component, fed with the owner's data, narrowed to the display's effective scope, at the wall's own size, with the same directive and the same controls | Real pixels |
| Latency | One HTTP POST for a control; the mirror applies its own control at once | A P5a/P6 capture is one still for each call (dev1: 867 KB PNG, fact 16); a stream needs a new video path (P5b records to an MP4 file only) |
| Cost | Small: one deck relay, one board page | Large: capture loop or streaming, new owner input path |
| Security | No new pixels leave the machine; controls are a closed enum with no text; no new display write | P7 refuses wall windows on purpose (fact 17); owner input is a new command path (P7 is coordinator-only); pixels of a wall can show a page or app that the owner did not expect |
| Fidelity | Approximate: the same view and items, not the same pixels; keys pressed on the kiosk keyboard are not mirrored; a page or app frame shows as a card | Exact |
| Works for pages | Scroll position only (a card with "Screen N of 8") | Yes, also clicks |

The mirror covers the owner's need ("see the wall and drive it from another device") for every Fleet Deck view, and scroll for pages. For an exact check, the Remote page has the button "Take a screenshot of the wall": it reuses the built owner screenshot (P5a, fact 15) of the bound monitor. That gives real pixels on demand without a new path. Clicks inside a page stay out of scope (§3.6).

### 2.4 "AI-generated wall UIs: an agent publishes an HTML artifact with a small bridge … Do B15 and this item share one app host?"

**Answer: yes, one app host. The wall is a second "role" of the B15 app frame. Phase C of this track builds the wall role after the B15 host exists.**

- Today an agent can publish an HTML artifact and show it with `wall_show`, but it is inert (fact 12).
- Design (§3.4): an `artifact` directive gets `app: true`; the display artifact route then serves the file with the app policy of B15 (scripts, no network) instead of `INERT_CSP`, but only when the owner turned on "Allow apps on this wall" for that display. The wall renders it in the B15 `AppFrame` with the role `wall`.
- Live data: the wall pushes a read-only feed to the app (the cards of the author session and of its spawn lineage, inside the display scope). The app gets nothing that its author could not read already.
- Actions back to the agent: none on the wall in this track. Every app request except `ui/initialize` gets "not available on a wall". A later item can route actions through the owner's approval with the B15 consent rules.
- `mcp-apps.md` did not exist when this draft was made. §3.4.1 lists the interface that this track needs from B15. If B15 decides differently, the architect aligns phase C with B15; the security rules of §3.4 stay binding.

---

## 3. Design

The design has four parts. Each part has its own feature token, so a part can ship alone.

| Part | Want | Feature token (board ↔ deck) | Migration | Phase |
| --- | --- | --- | --- | --- |
| 3.1 Controls | 1 | `wall_control.v1` | none | A |
| 3.3 Remote page (mirror) | 3 | `wall_control.v1` | none | A |
| 3.2 Wall pages | 2 | `wall_pages.v1` | V48 (new table + rebuild of `display_directives`) | B |
| 3.4 Wall apps | 4 | `wall_apps.v1` | the next free one (one column) | C (after B15) |

The feature tokens are constants in `fleetdeck_proto::version::feature` and members of `feature::ALL` (fact 21, the same as `artifacts_video.v1`). No station and no link method changes in any part.

### 3.1 Controls (want 1)

#### 3.1.1 Types (new file `crates/fleetdeck-proto/src/wall_control.rs`)

```rust
//! Wall controls (B17 §3.1): transient steps that the owner sends to one
//! wall display. A control holds no text and no reference.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::display_directive::DirectiveView;

/// The most controls that the deck keeps for each display.
pub const MAX_CONTROLS_KEPT: usize = 32;
/// The age after which the deck drops a kept control, in milliseconds.
pub const CONTROL_KEEP_MS: u64 = 30_000;
/// The most controls for each display in one second.
pub const MAX_CONTROLS_PER_SECOND: usize = 10;

/// A scroll area of the wall. A control for an area that the wall does not
/// show now does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum WallArea { Needs, Stream, Chat, Changes, Artifact, Page }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum ScrollStep { LineUp, LineDown, PageUp, PageDown, Top, Bottom }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum StepDir { Next, Previous }

/// One control. A closed list; an unknown op or field is a parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "display/")]
pub enum WallControl {
    Scroll { area: WallArea, step: ScrollStep },
    Select { dir: StepDir },
    Open,
    Back,
    Mode { dir: StepDir },
}

/// The body of `POST /api/displays/{id}/control`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct WallControlBody { pub control: WallControl }

/// The answer of the POST: the sequence number of the control and the
/// directive generation that it is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallControlAck { pub seq: u64, pub generation: u64 }

/// One relayed control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallControlFrame { pub seq: u64, pub generation: u64, pub control: WallControl }

/// The link of the wall page of a display, as the deck sees it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum WallLink {
    /// No socket of this display is open.
    Offline,
    /// Sockets are open, and none declared `wall_control.v1`.
    Older,
    /// At least one socket declared `wall_control.v1`.
    Ready,
}

/// `GET /api/displays/{id}/remote?after=<seq>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallRemoteView {
    pub directive: DirectiveView,
    pub link: WallLink,
    /// The sequence number of the newest control (0: none since the deck started).
    pub seq: u64,
    /// The kept controls with a `seq` above `after`, oldest first.
    pub controls: Vec<WallControlFrame>,
}
```

New variant of `BoardServerMessage` (`crates/fleetdeck-proto/src/board.rs:274-349`):

```rust
/// A wall control for a display socket that declared `wall_control.v1`
/// (B17 §3.1). No other socket gets it.
WallControl { seq: u64, generation: u64, control: crate::wall_control::WallControl },
```

TypeScript (generated by `make ts-types`): `WallArea`, `ScrollStep`, `StepDir`, `WallControl`, `WallControlBody`, `WallControlAck`, `WallControlFrame`, `WallLink`, `WallRemoteView` in `web/src/gen/display/`, and the `wall_control` member of `BoardServerMessage`.

#### 3.1.2 Routes

| Method row | Route | Scope | Who | Body / answer |
| --- | --- | --- | --- | --- |
| `displays.control` | `POST /api/displays/{id}/control` | Drive | owner only (no elevation, the same as a directive) | `WallControlBody` (at most 256 bytes) → `WallControlAck` |
| `displays.remote` | `GET /api/displays/{id}/remote?after=<u64>` | Read | owner only | `WallRemoteView` |

No `.display()` marker on either row. The test `exactly_these_methods_are_open_to_a_display` (`crates/fleetdeck-deck/src/http/methods_tests.rs:105`) stays as it is and proves it.

#### 3.1.3 The deck relay (new file `crates/fleetdeck-deck/src/wall_control.rs`)

`WallControlHub` is a field `wall_controls: Arc<WallControlHub>` of `Board` (`crates/fleetdeck-deck/src/http/mod.rs:106`). It is memory only. A deck restart forgets it.

```rust
pub struct WallControlHub { slots: Mutex<HashMap<String, Slot>> } // key: display_id
struct Slot {
    seq: u64,                                  // last seq of this display
    kept: VecDeque<(Instant, WallControlFrame)>,// ≤ MAX_CONTROLS_KEPT, ≤ CONTROL_KEEP_MS old
    tx: tokio::sync::broadcast::Sender<WallControlFrame>, // capacity 64
    ready: usize,                              // open display sockets with the feature
    older: usize,                              // open display sockets without it
    recent: VecDeque<Instant>,                 // rate window of 1 s
    last_audit: Option<Instant>,
}
impl WallControlHub {
    /// A display socket that declared `wall_control.v1`: a receiver and a guard
    /// that decrements `ready` on drop.
    pub fn attach_ready(&self, display_id: &str) -> (broadcast::Receiver<WallControlFrame>, SocketGuard);
    /// A display socket without the feature: a guard that decrements `older`.
    pub fn attach_older(&self, display_id: &str) -> SocketGuard;
    pub fn link(&self, display_id: &str) -> WallLink;
    /// Rate check, seq + 1, keep, broadcast. Err(TooMany) above the rate.
    pub fn push(&self, display_id: &str, generation: u64, control: WallControl, now: Instant)
        -> Result<WallControlFrame, PushError>;
    pub fn since(&self, display_id: &str, after: u64, now: Instant) -> (u64, Vec<WallControlFrame>);
    /// Revoke: drops the slot (receivers end).
    pub fn forget(&self, display_id: &str);
}
```

`POST /api/displays/{id}/control`, in order:

1. `owner(...)` check (the helper of `display_directive_routes.rs:137`).
2. Parse; a parse error is `400 invalid_control` "The control is not valid." (fixed text).
3. `effective_directive(&board.link, &id)` (`crates/fleetdeck-deck/src/coordinator_desktop/directive.rs:159`). `None` → `404 display_unknown`.
4. `link(id)`: `Offline` → `409 wall_offline` "The wall is not connected."; `Older` → `409 wall_controls_unsupported` "This wall runs an older board. Reload the wall to use the remote."
5. `push(id, view.generation, control)`. Above 10 in one second → `429 too_many_controls` "Too many controls. Wait a moment."
6. Audit: one row `display.remote`, target the display id, detail `start` for the first accepted control of a display after 10 minutes without one. No row for each control (10 per second would flood the audit). A refused POST writes no row.
7. Answer `WallControlAck { seq, generation }`.

`GET /api/displays/{id}/remote?after=N`: owner check; effective directive (`None` → 404); `since(id, N)`; `link(id)`. When `N` is above the current `seq` (a deck restart), the answer holds every kept control and the client resets its cursor to `seq`.

Revoke (`display_routes.rs:717`) calls `wall_controls.forget(id)`.

#### 3.1.4 WebSocket delivery

- `ws.rs::upgrade` (`crates/fleetdeck-deck/src/http/ws.rs:141-176`) reads the declaration with a new helper `declares(headers, query, feature::WALL_CONTROL)` next to `video_aware_socket` (fact 18). Put the helper and the display forwarding into a new file `crates/fleetdeck-deck/src/http/ws_wall_control.rs`, because `ws.rs` has 774 lines (the module limit is 800).
- A display socket with the feature calls `attach_ready`; the session loop also selects on the receiver and sends `BoardServerMessage::WallControl` for each frame. A lagged receiver skips the lost frames (a control is transient). A display socket without the feature calls `attach_older`. Owner and member sockets never attach and never get the frame.
- The frame passes the same send deadline as other display frames (`ws_display_send`, `http/mod.rs:127-129`).

#### 3.1.5 Behavior on the wall

New pure module `web/src/wall/control.ts`:

```ts
export type ControlState = {
  /** The selected item: `station/session` of a needs item, a tile or a race column. */
  readonly selected: string | null;
  /** Unix ms until which auto-focus and the ambient timer wait. */
  readonly holdUntil: number;
};
export const REMOTE_HOLD_MS = 120_000;
export type ControlEffect =
  | { readonly kind: 'none' }
  | { readonly kind: 'navigate'; readonly route: WallRoute }
  | { readonly kind: 'scroll'; readonly area: WallArea; readonly step: ScrollStep };
/** The selectable refs, in reading order: mission → the needs items, then the tiles (no
 *  repeats); race → the columns; every other mode → none. */
export function selectable(route: WallRoute, needs: readonly string[], tiles: readonly string[],
  columns: readonly string[]): readonly string[];
export function applyControl(state: ControlState, frame: WallControlFrame, ctx: {
  readonly route: WallRoute; readonly generation: number | null; readonly items: readonly string[];
  readonly now: number; }): { readonly state: ControlState; readonly effect: ControlEffect };
```

Rules of `applyControl`:

1. A frame whose `generation` is not the generation that the wall applied last does nothing (a control for an old view never acts on a new one). The owner wall and the mirror use the generation of the directive that they show.
2. Every accepted frame sets `holdUntil = now + REMOTE_HOLD_MS` and counts as activity for the ambient timer.
3. `select next|previous`: the next or previous ref of `items`, with wrap; no selection yet → the first (next) or the last (previous). Empty `items` → none.
4. `open`: in mission or race with a selected ref → navigate to `focus` on that session. Else none.
5. `back`: in ambient → wake (the same as a key, `WallPage.tsx:188-193`); in any mode other than mission → navigate to mission and clear the selection; in mission → clear the selection.
6. `mode next|previous`: `stepMode(route, ±1, selected ?? first tile)` (`web/src/wall/model.ts:107-120`).
7. `scroll`: effect `scroll` for the area.

`WallPage` (`web/src/wall/WallPage.tsx`) gets two new props:

```ts
/** B17: the controls of the deck (a display socket) or of the remote mirror. */
readonly controls?: ((on: (frame: WallControlFrame) => void) => () => void) | undefined;
/** B17: a mirror on the Remote page: no key listener, no QR codes, never a frame. */
readonly mirror?: boolean | undefined;
```

- A navigate effect calls `navigate(route, { replace: true })`, the same as a key.
- Auto-focus treats a hold as a pin: `nextAutoFocus(…, r.pin || now < holdUntil)` (`WallPage.tsx:170`). The ambient timer resets on each control (`lastChange`, `:139`).
- A scroll effect scrolls the element with `data-wall-scroll="<area>"`: `line_*` = 10 % of its `clientHeight`, `page_*` = 90 %, `top` = 0, `bottom` = `scrollHeight`. `scrollTo` with `behavior: 'smooth'`, and `'auto'` when Reduce motion is on.
- Scroll areas and their elements: `needs` (the needs list, mission), `stream` (the Activity stream, mission), `chat` (the focus chat), `changes` (the change pane of focus and review), `artifact` (the artifact pane), `page` (the page pane, §3.2).
- **Chat scroll:** `.fd-wall-chat` stays `overflow: hidden` (programmatic scroll works on it), but the bottom alignment moves from `justify-content: flex-end` to `margin-top: auto` on `.fd-wall-chatlines`, so that the older lines are reachable. While the chat is not at its end, its foot shows the line "Scrolled back. New lines are below." (`--dim`, `fd-wall-note`), and new lines do not move it. `bottom` returns to follow mode.
- **HTML artifacts:** the opaque frame cannot be scrolled from outside (fact 13). On the first `page_down` or `line_down` in an HTML artifact, the pane switches the frame to the long fit: the frame is 8 times the pane height inside the `artifact` scroll container, and the container scrolls. `top` returns to the one-screen fit. Markdown and images scroll in the pane itself.
- **Selection look:** the selected tile, needs item or race column gets `data-selected="true"` and an outline in `--focus` (the selection color, DESIGN-SYSTEM §2.2), 3 wall units, offset 2 wall units, in `wall.css`. Forced colors: `outline-color: Highlight`. No new token.
- The kiosk keys keep working and stay local (fact 9).

`DisplayRuntime` gives `WallPage` a `controls` feed from its `Connection`. `Connection` (`web/src/runtime/connection.ts`) gets an optional features list: the display environment declares `artifacts_video.v1 wall_control.v1 wall_pages.v1` in `features=`; `decode.ts` validates `wall_control` (`sequence(seq)`, `sequence(generation)`, an object `control` with a text `op`) and the connection passes each frame to its control listeners. It does not change the read model.

### 3.2 Wall pages (want 2)

#### 3.2.1 Types (new file `crates/fleetdeck-proto/src/wall_pages.rs`)

```rust
pub const MAX_WALL_PAGES: usize = 50;
/// The longest page name, in characters.
pub const MAX_PAGE_LABEL: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum PageFit {
    /// The frame is one screen.
    Screen,
    /// The frame is 8 screens tall and the wall scrolls it.
    Long,
}

/// One entry of the allow-list, as the owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallPage {
    pub page_id: String,   // a ULID
    pub label: String,
    pub url: String,
    /// `scheme://host[:port]`, lower case, no default port.
    pub origin: String,
    pub fit: PageFit,
    /// Unix ms.
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct NewWallPage { pub label: String, pub url: String, pub fit: PageFit }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallPageList { pub pages: Vec<WallPage> }

/// A page in a directive: by id, never a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct PageRef { pub page_id: String }

/// `GET /api/displays/self/page/{page_id}`: what the wall needs to show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayPage { pub page_id: String, pub label: String, pub url: String, pub fit: PageFit }
```

`DirectiveBody` gets one variant and `DirectiveMode` gets `Page` (`display_directive.rs:49-108`):

```rust
Page {
    page: crate::wall_pages::PageRef,
    #[serde(default)]
    pin: bool,
    #[serde(default)]
    filter: Option<DirectiveFilter>,
},
```

#### 3.2.2 Storage: migration V48 (`crates/fleetdeck-deck/src/store/wall_pages_schema.rs`)

```sql
CREATE TABLE wall_pages (
    page_id    TEXT    PRIMARY KEY,
    label      TEXT    NOT NULL,
    url        TEXT    NOT NULL,
    origin     TEXT    NOT NULL,
    fit        TEXT    NOT NULL CHECK (fit IN ('screen', 'long')),
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE display_directives_v48 (
    display_id        TEXT    PRIMARY KEY,
    generation        INTEGER NOT NULL,
    mode              TEXT    NOT NULL
        CHECK (mode IN ('mission', 'focus', 'review', 'race', 'artifact', 'ambient', 'page')),
    sender_kind       TEXT    NOT NULL CHECK (sender_kind IN ('owner', 'station', 'coordinator')),
    sender_id         TEXT    NOT NULL DEFAULT '',
    body              TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    sender_generation INTEGER,
    sender_station    TEXT,
    sender_session    TEXT
) STRICT;
INSERT INTO display_directives_v48
    (display_id, generation, mode, sender_kind, sender_id, body, created_at,
     sender_generation, sender_station, sender_session)
SELECT display_id, generation, mode, sender_kind, sender_id, body, created_at,
       sender_generation, sender_station, sender_session
FROM display_directives;
DROP TABLE display_directives;
ALTER TABLE display_directives_v48 RENAME TO display_directives;
```

`display_directives` has no index, trigger or foreign key (V25, V34), so the rebuild loses nothing. Reversal: rebuild again without `'page'` after deleting the `page` rows, and drop `wall_pages`. Use the next free migration number if V48 is taken when the task starts.

#### 3.2.3 Validation of an entry (deck, `crates/fleetdeck-deck/src/store/wall_pages.rs`)

In order; the first failure answers `400 invalid_page` with the text in the table:

| Check | Text |
| --- | --- |
| `label` trimmed, 1 to 60 characters, no control character | "Give a name of 1 to 60 characters." |
| `fleetdeck_proto::desktop_text::url(url)` is `Some` (http/https, ≤ 2048 bytes, printable ASCII, host grammar, port, no user info; fact 14) | "Give an http or https address." |
| `http` only for a host that is `localhost`, ends in `.localhost`, `.local` or `.ts.net`, an IPv4 address in 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 or 100.64.0.0/10, or `[::1]` | "Use https for this host. http is allowed only for this machine, a private network and the tailnet." |
| the origin is not one of `board.origins` (`http/mod.rs:110`) | "This is the address of the board. A wall cannot show the board in a frame." |
| fewer than 50 entries | `409 too_many_pages` "A deck holds at most 50 wall pages." |

`origin` = the lower-case scheme and host, and the port only when it is not 80 for `http` or 443 for `https`.

#### 3.2.4 Routes

| Method row | Route | Scope | Who | Notes |
| --- | --- | --- | --- | --- |
| `wall_pages.list` | `GET /api/wall-pages` | Read | owner only | `WallPageList`, oldest first |
| `wall_pages.add` | `POST /api/wall-pages` | Drive | owner only, **fresh elevation** (the pattern of `display_routes.rs:116-131`, key `wall_pages`) | `NewWallPage` (≤ 4 KiB) → `WallPage`. Audit `wall_page.add`, detail `page=<id> host=<host>` (never the path or the query) |
| `wall_pages.delete` | `POST /api/wall-pages/{id}/delete` | Drive | owner only | One transaction: delete the row and call `invalidate_directive` (`display_directive_schema.rs:22`) for each display whose stored directive is `page` with this id. Answer `{ "displays_reset": n }`. Audit `wall_page.delete`, detail `page=<id> displays_reset=<n>`. Unknown id → `404 unknown_page` "No such page." |
| `displays.self_page` | `GET /api/displays/self/page/{page_id}` | Read | display only, **`.display()`** | `DisplayPage` only when the effective directive is `page` with this id and the entry exists; the directive is read before and after the entry, and both reads must be equal (the pattern of `display_artifact.rs:137-185`). Every miss is `404 unknown_page`. `Cache-Control: no-store` |

The test `exactly_these_methods_are_open_to_a_display` adds `displays.self_page`.

#### 3.2.5 Directive rules for `page`

- `check_shape`: `page_id` is 26 characters of `[0-9A-Za-z]`; else "the page reference is not valid".
- `check_references`: the filter stations must be inside the ceiling (unchanged); the entry must exist, else `Invalid("unknown reference")`. A page has no session reference: the answer is an empty reference list.
- `write_directive`: only `DirectiveSender::Owner` may store a `page` directive; `Station` and `Coordinator` get `NotGranted` (they cannot build one today: `display_directives_station.rs:146-159`, `coordinator_desktop/wall.rs:44-60`; this is a second barrier).
- `still_valid` re-checks that the entry exists, so a deleted entry gives `mission` also without the delete transaction.
- Audit detail: `mode=page page=<id>`; `mode_name` → `"page"`.
- The station intent `next_mode` from a `page` directive goes to mission (the `_ => plain(true)` arm, `display_directives_station.rs:157-158`). No change.
- **Older wall pages:** `GET /api/displays/self/directive` answers `DirectiveBody::mission()` (with the same generation) when the stored directive is `page` and the request does not declare `wall_pages.v1` in `X-FleetDeck-Features`. The display environment adds the header to every display request (`web/src/display/api.ts:21-27`). An older wall therefore shows Mission, never a mode that it cannot draw.

#### 3.2.6 The display document policy

`display_document::document` (`display_document.rs:27-35`) reads the distinct `origin` values of `wall_pages` and passes them to `display_csp`:

```rust
pub fn display_csp(frame_origin: Option<&str>, page_origins: &[String]) -> String
// frame-src {origin}/api/displays/self/artifact/ <o1> <o2> …   (or 'none' plus the page origins)
```

Each page origin is checked again with the grammar `^https?://([a-z0-9.-]+|\[[0-9a-f:]+\])(:[0-9]{1,5})?$` before it goes into the header; an origin that fails is left out. The document keeps `no-cache`, so a reload gets the current list.

#### 3.2.7 The page pane (new `web/src/wall/PagePane.tsx`)

- The display wall reads `GET /api/displays/self/page/{page_id}` for each new directive generation.
- Support check: `'credentialless' in HTMLIFrameElement.prototype`. Without it: "This browser cannot show pages on a wall safely."
- Same-origin check: `new URL(url).origin === window.location.origin` → "This is the address of the board. A wall cannot show the board in a frame."
- The frame:

```tsx
<section className="fd-wall-page" data-fit={fit} data-wall-scroll="page" aria-label={label}>
  <iframe className="fd-wall-pageframe" title={label} ref={frameRef}
    sandbox="allow-scripts allow-same-origin allow-forms" allow="" referrerPolicy="no-referrer" />
</section>
```

  The ref callback sets the attribute `credentialless` **before** it sets `src` (the attribute must be present before the first navigation). React does not know the attribute, so the code sets it with `setAttribute`.
- Fit: `screen` → the frame fills the pane. `long` → the frame is `800%` of the pane height and the section scrolls (`overflow: hidden`, programmatic scroll).
- A `securitypolicyviolation` event with `effectiveDirective === 'frame-src'` and a `blockedURI` of the page origin (a page added after the document loaded) reloads the document once; the guard is the `sessionStorage` key `fd-wall-page-reload:<page_id>`. A second violation shows "The wall could not load this page. Reload the wall."
- The wall ignores `message` events (no listener).
- The owner's `#/wall` and the mirror never load the page: they show a card with the page name, the host in mono, and for the long fit "Screen N of 8" (the long-fit scroll position is a pure count of the page controls, clamped to 0..7).

#### 3.2.8 Board: Settings › Displays › "Wall pages"

A section under the display list (`web/src/displays/DisplaysTab.tsx`), as a `.fd-panel`:

- Heading: "Wall pages". Text: "Pages that a wall may show besides Fleet Deck views. A wall shows a page in a closed frame: the page gets no login and no cookies of the wall, and it can only move inside its own site."
- Note (`--dim`): "Some sites refuse to show inside another page. Open those with Desktop › Open a link."
- Warning line (`fd-note`, hazard tone): "Do not put a password or a token in an address. Programs that run as you on the wall's machine can read it."
- Form: "Name" (text), "Address" (url), a radio group "Fits one screen" / "Long page (scrolls)", button "Add the page" (`.fd-primary`). It asks for the elevation the same way as "Add a wall display".
- List: one row for each page: the name, the host (mono, `--dim`), "Long page" or "One screen", a `.fd-danger` "Delete…" with the confirm "Delete this page? Walls that show it go back to Mission view." and the buttons "Delete" / "Keep".
- Empty: "No wall pages yet."
- Errors: the deck texts of §3.2.3 under the field (`fd-fielderror`); a load error is `ErrorNotice`.

"Show a page…" in the display row menu (`web/src/displays/DisplayRow.tsx:360-393`, after "Switch to Mission view") opens a dialog "Show a page on <display name>": a select of the pages, the checkbox "Keep it on the screen (no auto-focus)" (checked), the button "Show the page" (`.fd-primary`). Empty list: "No wall pages yet. Add one under Wall pages." Success: "The wall shows the page." It sends `POST /api/displays/{id}/directive` with `{ mode: "page", page: { page_id }, pin, filter: null }`.

### 3.3 The Remote page (want 3)

#### 3.3.1 Route and entry points

- New board route `#/remote/<display_id>` (`web/src/app/routes.ts`: `{ name: 'remote'; id: string }`). Owner only; another login sees "The remote needs the owner login."
- Entry points: "Remote control" in the display row menu (`DisplayRow.tsx`, first item), and a "Remote" button on each wall row of the Desktop panel (`web/src/desktop/DesktopPanel.tsx:238-275`).
- A deck without `wall_control.v1` in `GET /api/features` shows "This deck has no wall remote." and no pad.

#### 3.3.2 Layout (desktop and phone)

1. Page header: title "Remote" and the display name (`.fd-wide`); the link line: `.fd-dot[data-status=online]` "Connected", or "The wall is not connected.", or "This wall runs an older board. Reload the wall to use the remote." with the button "Reload the wall" when the station's Desktop has this wall (the built `wall_reload` action).
2. **The mirror:** a box with the aspect ratio of the bound monitor (logical size = pixels × 100 / `scale_pct`, from `GET /api/stations/{station}/desktop`; no binding or no desktop: 1920 × 1080). Inside, `WallPage` renders at that logical size with `initialSize`, scaled down with `transform: scale(boxWidth / logicalWidth)`, `inert`, `mirror`. Its data: the owner's sessions and stations, with `route.stations` = the display ceiling narrowed by the directive filter (the same rule as `EffectiveScope::narrowed`, `display_directives.rs:56-71`), and its route from the directive of `WallRemoteView` (through `directiveRoute`, `web/src/display/api.ts:149-175`). Caption (`--dim`): "A copy of what the wall shows, made on this device. It is not a picture of the screen."
3. **The pad** (`.fd-panel`), every control a plain button with an icon and an accessible name, 44 px on touch:
   - "Previous view" · the current mode name · "Next view";
   - "Previous item", "Next item", "Open", "Back";
   - a `.fd-seg` of the scroll areas that the current mode has (Mission: "Needs you", "Activity"; Focus: "Chat", "Changes"; Review: "Changes"; Artifact: "Artifact"; Page: "Page"; none in Race and Ambient, and then the scroll buttons are disabled);
   - "Scroll up" (`page_up`), "Scroll down" (`page_down`), "To the top", "To the end".
4. Footer: "Switch to Mission view" (the built directive) and "Take a screenshot of the wall" (only when the display is bound, the station has a desktop and its monitor allows capture; it runs the built owner capture of the bound monitor, `web/src/desktop/capture.ts`, and opens the artifact when it is done).
5. A `<details>` "Keys": "← → views · J K items · Enter open · Esc back · ↑ ↓ scroll a little · Page Up, Page Down a screen · Home, End · [ ] scroll area". The keys act only while the focus is inside the pad.

#### 3.3.3 Behavior

- The page polls `GET /api/displays/{id}/remote?after=<seq>` every 1 s while it is visible (`document.visibilityState`), and every 10 s when it is hidden.
- A pad action sends `POST /api/displays/{id}/control`. On success the mirror applies the control at once (with the `seq` of the answer) and skips the same `seq` when the poll returns it. Controls of other devices arrive with the poll and the mirror applies them in `seq` order.
- Status texts (`role="status"`, cleared after 4 s): success "Sent."; `409 wall_offline` "The wall is not connected."; `409 wall_controls_unsupported` the "older board" text; `429` "Too many controls. Wait a moment."; `404` "This display does not exist any more."; other errors: `describe(e)`.
- Errors never repeat a POST.

### 3.4 Wall apps (want 4, phase C, after B15)

#### 3.4.1 The interface that this track needs from B15

B15 builds one app host. The wall uses it as a second role. The names below are proposals; the contract items are binding for the wall:

| Id | What | Contract |
| --- | --- | --- |
| I-1 | A web component `AppFrame` (proposed `web/src/apps/AppFrame.tsx`) | Props `{ src: string; title: string; role: 'chat' \| 'wall'; data?: unknown }`. An iframe with `sandbox="allow-scripts"` (never `allow-same-origin`), an opaque origin, `allow=""`, `referrerPolicy="no-referrer"`. The host accepts a message only from that frame's `contentWindow` with origin `"null"`. |
| I-2 | The MCP Apps host bridge | `ui/initialize` answers the host context (theme variables `--color-*` from the board tokens, `displayMode: "fullscreen"`, the container size). With `role: 'wall'`, every other app request (`tools/call`, `resources/read`, `ui/message`, `ui/open-link`, `ui/update-model-context`, `ui/download-file`, display mode changes) answers JSON-RPC error `-32601` "Not available on a wall", and size changes are ignored (the app fills the pane). |
| I-3 | A host notification for live data | `notifications/fleetdeck/wall-data` with the params `WallAppFeed` (§3.4.3), sent after `ui/initialize` and again on each change. The chat role does not send it. |
| I-4 | One deck app policy constant (proposed `APP_CSP`) | `sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; form-action 'none'; base-uri 'none'`. |

If `mcp-apps.md` chooses other names, phase C uses them. If B15 is not built, phase C waits.

#### 3.4.2 Directive and setting

- `DirectiveBody::Artifact` gets `#[serde(default)] app: bool` (default false). An older wall ignores it and shows the file inert (fail closed).
- New display setting `allow_apps` (column `displays.allow_apps INTEGER NOT NULL DEFAULT 0`, the next free migration), in `POST /api/displays/{id}/settings` (`display_routes.rs:593`). Turning it on needs fresh elevation; turning it off does not. A change of `allow_apps` invalidates the stored directive (the same as a ceiling change).
- The display artifact route serves `APP_CSP` instead of `INERT_CSP` only when **all** hold: the effective directive is `artifact` with `app: true` for exactly this version; the display has `allow_apps` on (read live on each request); the one file is HTML; the request declares `wall_apps.v1`. Else `INERT_CSP` (unchanged).
- Coordinator `wall_show` gets the optional argument `app` (boolean, default false), mapped to the directive field. The P6 rules of §4.2 stay (own lineage only).
- "Send to wall…" (`web/src/wall/SendToWall.tsx`) shows the checkbox "Run it as an app" for a single HTML file when the chosen display has `allow_apps`.

#### 3.4.3 The feed

```rust
/// `GET /api/displays/self/app-feed/{station}/{id}/{version}` (display only, `.display()`).
pub struct WallAppFeed { pub at: i64, pub sessions: Vec<WallAppCard> }
pub struct WallAppCard {
    pub station: String,          // station name
    pub session_id: SessionId,
    pub callsign: String,
    pub title: String,
    pub status: String,           // the card status word
    pub needs_you: Option<String>,// the open request kind
    pub tool: Option<String>,     // the safe tool name, ≤ 40 characters
}
```

- The route answers only while the effective directive is this app artifact with `app: true` and `allow_apps` is on; else `404 unknown_artifact`.
- Contents: the display card projection (the fields that a display already gets, §5D items 7 and 11 of the parent), limited to the **author session of the artifact and its spawn lineage** (the OD-7 rule of `omarchy-p6.md`), inside the effective scope. At most 64 cards and 32 KiB.
- The wall polls it every 5 s and passes it to `AppFrame` as `data`.

#### 3.4.4 Display row text

The switch "Allow apps on this wall" with the description: "An app is an HTML artifact that runs its own scripts in a closed frame. It sees only the cards of the session that made it and of the sessions that this session started. It cannot reach the network."

### 3.5 Compatibility

| Combination | Behavior |
| --- | --- |
| New deck, wall page loaded before the upgrade (old bundle) | It does not declare `wall_control.v1`: no control frame reaches it, the Remote page shows the "older board" text with "Reload the wall". A `page` directive reads as `mission` (§3.2.5). `app: true` is ignored (inert). |
| New bundle on an old deck (a stale tab after a downgrade) | `GET /api/features` lacks `wall_control.v1`: the Remote page shows "This deck has no wall remote."; the Wall pages section is hidden when `wall_pages.v1` is absent; the routes answer 404 anyway. |
| Stations, links, coordinators (phases A, B) | No change. No link feature. The station hotkeys keep their two intents. |
| Coordinators (phase C) | `wall_show` gains an optional boolean; an older coordinator never sends it. |
| A browser without `credentialless` as the kiosk | The page pane refuses to show the page (text of §3.2.7). |

### 3.6 Out of scope

- Clicks, keys or text inside a page or an app on the wall; any input synthesis into a wall window (P7 refuses it, fact 17).
- A pixel stream of the wall.
- Coordinators sending controls or `page` directives; station hotkeys for select, open, back or scroll (owner question Q5).
- A free URL in a directive; per-display page lists (owner question Q1).
- App actions back to the agent; multi-file apps (DL-221 stays: single file only).
- Mirroring keys pressed on the kiosk keyboard.
- The `desktop_info` station-name finding (§1.3, last row).

---

## 4. Errors and edge cases

| Case | Behavior |
| --- | --- |
| A control while the directive changes | The control carries the generation of the POST time; the wall drops it if its applied generation differs (§3.1.5 rule 1). |
| Two owner devices drive one wall | Both POST; the deck orders them by `seq`; each mirror applies the other's controls on its next poll. |
| A wall with two sockets (a reload in progress) | `ready` counts both; each socket gets the frame; the old page is closing. |
| Deck restart | Controls are lost (memory only); `seq` restarts at 0; a mirror with `after` above `seq` resets (§3.1.3). |
| Display revoked | `forget(id)` ends the receivers; the socket closes within 5 s (existing); POST → 404. |
| `select` with no items (focus, artifact, page, ambient) | Nothing. |
| `scroll` for an area that the mode does not show | Nothing. |
| A page entry deleted while shown | The directive is deleted in the same transaction; the wall returns to Mission on its next poll; `self/page` answers 404 at once. |
| A page redirects to another origin (SSO, `www.`) | The browser blocks it under `frame-src`; the frame shows the browser's block page. The owner adds the target origin as its own entry. |
| A page that refuses framing | The frame shows the site's own error. The settings note names "Open a link". |
| A long-fit page that sizes itself to `100vh` | It becomes 8 screens tall. The owner chooses "Fits one screen" for such a page. |
| `long` fit scrolled past the end | The scroll clamps; the mirror count clamps to 7. |
| HTML artifact scrolled | Switches to the long fit on the first downward scroll; `top` returns to one screen. |
| More than 10 controls per second | `429`; the pad shows the text; nothing is queued. |
| Kiosk keyboard and remote together | Both act on the local state of the wall; the mirror does not see the kiosk keys. |

---

## 5. Risk class: **high**

Reasons: a new owner command path to a display (controls); a migration that rebuilds a table (`display_directives`); third-party web content inside the kiosk of a read-only principal (pages); agent scripts on a wall (apps). Each phase needs an adversary DESIGN REVIEW before its build and a PRE-PR review before its merge.

### 5.1 Threat notes: controls and the mirror

1. **Authority.** Only the owner sends controls (Drive, owner only); a member, a share guest, a token without Drive, a display and a coordinator get the existing refusals. A control can do only what the kiosk keys do (fact 9), inside the display's existing scope.
2. **No data channel.** A control is a closed enum with no string, number or reference; the deck relays nothing that a display could not already read. A display frame adds no field to the display wire model (§5A item 2 of the parent).
3. **No new display write.** The display still only subscribes (fact 20). The mirror computes from owner data; it needs no report from the wall.
4. **Flooding.** 10 controls per second per display; at most 32 kept, 30 s; a broadcast capacity of 64 with lag skip; the audit writes one row per remote session start.
5. **Old clients.** Frames go only to sockets that declared the feature (fact 19).

### 5.2 Threat notes: wall pages

1. **The display credential.** `fd_display` is `HttpOnly; Secure; SameSite=Strict` for the deck host. Cookies ignore the port, so a page on another port of the deck host would receive the cookie in a normal frame. Mitigation: the `credentialless` frame (no cookies at all), and no frame without support. Same-origin entries are refused twice (deck and wall).
2. **The kiosk profile.** The frame storage is ephemeral (credentialless); the sandbox has no top navigation and no popups, so a page cannot take the top-level kiosk document (which is not credentialless) to another site.
3. **Navigation away from the allow-list.** Blocked by the document's `frame-src`, built from the stored origins (§3.2.6). The allow-list is origin-level, not URL-level; the settings text says "it can only move inside its own site".
4. **The wall machine's network position.** A page runs in the kiosk on the wall machine and can try to reach `localhost` and the LAN. Mitigation: owner-chosen origins only; `allow=""` denies the local-network-access permission in the frame. Residual: an allowed page that turns hostile can probe what the browser permits. Stated in the settings text ("runs as you on the wall's machine").
5. **Trusted screen.** A hostile page can show fake Fleet Deck content (a fake request with a QR code). Residual; the owner chooses the origins.
6. **Secrets in URLs.** The URL is stored and sent to the display. The audit holds only the id and the host. The settings warning tells the owner not to put a token in an address.
7. **Header size.** At most 50 origins, each re-validated before it enters the header.
8. **Migration.** The rebuild copies every row and column; a test opens a V47 database with one row of each sender kind, migrates, and reads the rows back unchanged.

### 5.3 Threat notes: wall apps

1. **Scripts on a trusted screen.** Off by default for each display; elevation to turn on; the owner or the author coordinator must set `app: true` on the directive.
2. **Exfiltration.** `connect-src 'none'`, no frames, no forms, opaque origin, no popups, no top navigation. Residual: Chromium does not block WebRTC through CSP, so an app can send what it holds. Mitigation: an app holds only its own document and the feed of its author's lineage, which the author session can read already.
3. **App to host.** The wall role refuses every app request except `ui/initialize` (I-2); messages are accepted only from the frame's own window.
4. **Live authority.** The app policy is chosen per request from the live directive and the live `allow_apps`; a change ends the next request and the pane reloads on the new generation.

---

## 6. Build plan

Order: **A1 → A2 → A3**; **B1** after A1 (it may run in parallel with A2); **B2** after B1 (it may run in parallel with A3); **B3** after B2 and A3; **C1 → C2** after B2 and the B15 host tasks. Every heavy command runs through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Every task also runs the guards:

```sh
umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'
```

A task that changes a proto type runs `make ts-types` through `heavy.sh` and owns the generated files under `web/src/gen/` that change, and runs the drift test `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(ts_drift)'`. Every UI task follows `docs/v2/DESIGN-SYSTEM.md` (the `fleet-design-system` skill) and runs `sh tools/design-check.sh`.

### A1 (backend): wall control types, relay and owner routes (≈ 560 lines)

- **Owned files:** `crates/fleetdeck-proto/src/wall_control.rs` (new), `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/board.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-proto/src/ts.rs`, `web/src/gen/**` (generated only), `crates/fleetdeck-deck/src/wall_control.rs` (new), `crates/fleetdeck-deck/src/lib.rs`, `crates/fleetdeck-deck/src/http/mod.rs`, `crates/fleetdeck-deck/src/serve.rs`, `crates/fleetdeck-deck/tests/common/board.rs`, `crates/fleetdeck-deck/src/http/wall_control_routes.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/ws.rs`, `crates/fleetdeck-deck/src/http/ws_wall_control.rs` (new), `crates/fleetdeck-deck/src/http/display_routes.rs` (revoke calls `forget`), `crates/fleetdeck-deck/tests/board_wall_control.rs` (new).
- **Changes:** §3.1.1 types and the `WallControl` frame; `feature::WALL_CONTROL = "wall_control.v1"` in `feature::ALL`; the hub of §3.1.3 as a `Board` field (both constructors); the two routes and their method rows; the WS attach and forwarding of §3.1.4; revoke → `forget`.
- **Tests:**
  - proto `wall_control_ops_are_closed` — an unknown op, an extra field, and a text `area` are parse errors; each op round-trips.
  - deck unit `hub_orders_and_keeps_at_most_32` — 40 pushes keep the last 32 with increasing `seq`.
  - deck unit `hub_drops_controls_older_than_30_s` — `since` after 31 s returns none.
  - deck unit `hub_refuses_the_eleventh_control_in_a_second` — `PushError::TooMany`.
  - deck unit `hub_link_counts_ready_and_older_sockets` — `Offline`, `Older`, `Ready`, and back to `Offline` when the guards drop.
  - `board_wall_control::owner_control_reaches_a_declaring_display_socket` — a display socket with `features=wall_control.v1` receives `wall_control` with the `seq` and generation of the ack.
  - `…::a_display_socket_without_the_feature_gets_no_frame_and_the_post_answers_older` — 409 `wall_controls_unsupported`, and the socket gets no frame.
  - `…::no_socket_answers_wall_offline` — 409.
  - `…::owner_and_member_sockets_never_get_a_control_frame`.
  - `…::member_token_and_display_are_refused` — member 403, a Read token 403, a display cookie 403 `display_read_only`.
  - `…::remote_view_returns_the_effective_directive_and_controls_after_the_cursor`.
  - `…::revoke_ends_the_relay` — after revoke the POST answers 404 and the socket ends.
  - `…::one_audit_row_for_a_burst` — 5 controls give one `display.remote` row.
  - The existing `exactly_these_methods_are_open_to_a_display` passes unchanged.
- **depends_on:** none.
- **Acceptance:**
  - `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(wall_control) | test(ts_drift)'`
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_wall_control) | test(hub_) | test(exactly_these_methods_are_open_to_a_display) | binary(board_display_foundation)'`
  - the guards.

### A2 (ux): the wall applies controls (≈ 520 lines)

- **Owned files:** `web/src/runtime/decode.ts`, `web/src/runtime/connection.ts`, `web/src/display/DisplayRuntime.tsx`, `web/src/display/api.ts` (only `displayEnvironment` and `withDisplayHeader`: the features header), `web/src/wall/control.ts` (new), `web/src/wall/control.test.ts` (new), `web/src/wall/WallPage.tsx`, `web/src/wall/ChatPane.tsx`, `web/src/wall/Areas.tsx`, `web/src/wall/RacePane.tsx`, `web/src/wall/DisplayArtifactPane.tsx`, `web/src/wall/DiffPane.tsx`, `web/src/wall/displayChanges.tsx`, `web/src/styles/wall.css`, `web/src/wall/wall.test.tsx`, `web/src/display/display.test.tsx`.
- **Changes:** §3.1.5 completely; the display connection declares `artifacts_video.v1 wall_control.v1 wall_pages.v1` in `features=` and in `X-FleetDeck-Features`; `decode.ts` validates `wall_control`; the `controls` and `mirror` props of `WallPage` (`mirror` turns off the key listener and the QR codes); `data-wall-scroll` on the six areas; `data-selected` and its outline; the chat bottom alignment change and the "Scrolled back. New lines are below." line; the long fit of an HTML artifact.
- **Tests:**
  - `control.test.ts`: `stale_generation_does_nothing`; `select_wraps_both_ways`; `select_without_items_does_nothing`; `open_focuses_the_selected_session_in_mission_and_race`; `back_goes_to_mission_and_clears_the_selection`; `back_wakes_ambient`; `mode_steps_like_the_arrow_keys`; `every_control_sets_the_hold`; `selectable_lists_needs_then_tiles_without_repeats`.
  - `wall.test.tsx`: `a_scroll_control_scrolls_the_named_area` (the element's `scrollTop` changes by 90 % of its height for `page_down`); `the_chat_can_scroll_back_and_shows_the_note`; `a_hold_stops_auto_focus_for_120_s`; `the_selected_tile_has_data_selected`; `mirror_has_no_key_listener` (a keydown on `document` is not stopped); `an_html_artifact_switches_to_the_long_fit_on_scroll_down`.
  - `display.test.tsx`: `the_display_socket_declares_wall_control`; `a_wall_control_frame_reaches_the_wall`; `an_unknown_frame_still_reconnects` (unchanged behavior).
- **depends_on:** A1.
- **Acceptance:** `cd web && bun run typecheck && bun run lint`; `cd web && bun run test src/wall src/display src/runtime` (through heavy.sh); `sh tools/design-check.sh`; the guards.

### A3 (ux): the Remote page and the mirror (≈ 580 lines)

- **Owned files:** `web/src/app/routes.ts`, `web/src/App.tsx` (the route switch that renders `WallPage` for the `wall` route today), `web/src/remote/RemotePage.tsx` (new), `web/src/remote/Pad.tsx` (new), `web/src/remote/mirror.ts` (new), `web/src/remote/api.ts` (new), `web/src/remote/remote.test.tsx` (new), `web/src/styles/remote.css` (new), `web/src/main.tsx` (the CSS import), `web/src/displays/DisplayRow.tsx`, `web/src/displays/actions.ts`, `web/src/desktop/DesktopPanel.tsx`, `web/src/displays/displays.test.tsx`.
- **Changes:** §3.3 completely: route `#/remote/<id>` (`web/src/App.tsx` has 789 lines and the module limit is 800 for `.rs`, `.ts` and `.tsx`, `tools/module-size.sh:10-11`: add at most one `case` line that renders `RemotePage`; all logic lives in `web/src/remote/`); the mirror (scope narrowing, monitor size, scale, `inert`); the pad, the keys, the polling and the de-duplication by `seq`; the status texts; the screenshot button through `capture.ts`; the entry points.
- **Tests:**
  - `routes`: `remote_route_round_trips`.
  - `remote.test.tsx`: `the_pad_posts_each_control`; `the_mirror_applies_its_own_control_once` (the same `seq` from the poll is skipped); `controls_of_another_device_arrive_by_poll_in_seq_order`; `older_wall_shows_the_reload_text_and_button`; `offline_wall_shows_the_text`; `too_many_controls_shows_the_text`; `deck_without_the_feature_shows_no_pad`; `the_mirror_uses_the_display_ceiling_and_the_directive_filter`; `the_scroll_areas_follow_the_mode`; `keys_act_only_inside_the_pad`; `the_screenshot_button_shows_only_for_a_bound_capture_monitor`.
  - `displays.test.tsx`: `the_row_menu_opens_the_remote`.
- **depends_on:** A2.
- **Acceptance:** typecheck, lint; `cd web && bun run test src/remote src/displays src/app` (through heavy.sh); `sh tools/design-check.sh`; gallery shots of the Remote page in both themes, desktop and phone (the skill, §3 step 3); the guards.

### B1 (backend): migration V48, the wall page store and owner routes (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-proto/src/wall_pages.rs` (new), `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/ts.rs`, `crates/fleetdeck-proto/src/version.rs`, `web/src/gen/**` (generated only), `crates/fleetdeck-deck/src/store/wall_pages_schema.rs` (new), `crates/fleetdeck-deck/src/store/wall_pages.rs` (new), `crates/fleetdeck-deck/src/store/wall_pages_tests.rs` (new), `crates/fleetdeck-deck/src/store/mod.rs`, `crates/fleetdeck-deck/src/store/schema.rs`, `crates/fleetdeck-deck/src/http/wall_page_routes.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/mod.rs`, `crates/fleetdeck-deck/tests/board_wall_pages.rs` (new), `crates/fleetdeck-deck/tests/merged_migrations.rs`.
- **Changes:** §3.2.1 types except the `DirectiveBody` variant; §3.2.2 migration; §3.2.3 validation; the three owner routes of §3.2.4 (the delete invalidates `page` rows through a JSON check of `body`, which B2 makes reachable); `feature::WALL_PAGES = "wall_pages.v1"` in `feature::ALL`.
- **Tests:** `migration_v48_keeps_every_directive_row` (a V47 database with an owner, a station and a coordinator row: every column equal after the migration); `migration_v48_accepts_mode_page`; `page_validation_table` (each row of §3.2.3, with the exact text); `http_is_refused_for_a_public_host`; `http_is_allowed_for_loopback_private_and_tailnet`; `a_board_origin_is_refused`; `the_fifty_first_page_is_refused`; `origin_drops_the_default_port`; `board_wall_pages::add_needs_elevation`; `…::member_and_display_are_refused`; `…::delete_of_an_unknown_page_is_404`; `…::audit_holds_the_host_and_never_the_path`.
- **depends_on:** A1 (shared `http/mod.rs`, `methods_table_display.rs`, `version.rs`).
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(wall_page) | test(ts_drift)'`; `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(migration_v48) | test(page_) | binary(board_wall_pages) | binary(merged_migrations) | test(schema)'`; the guards.

### B2 (backend): the `page` directive, the display page route and the document policy (≈ 480 lines)

- **Owned files:** `crates/fleetdeck-proto/src/display_directive.rs`, `web/src/gen/**` (generated only), `crates/fleetdeck-deck/src/store/display_directives.rs`, `crates/fleetdeck-deck/src/store/display_directives_check.rs`, `crates/fleetdeck-deck/src/store/display_directives_tests.rs`, `crates/fleetdeck-deck/src/http/display_directive_routes.rs`, `crates/fleetdeck-deck/src/http/display_page.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/methods_tests.rs`, `crates/fleetdeck-deck/src/http/mod.rs`, `crates/fleetdeck-deck/src/http/assets.rs`, `crates/fleetdeck-deck/src/http/display_document.rs`, `crates/fleetdeck-deck/src/store/wall_pages.rs` (the delete query, if B1 left a stub), `crates/fleetdeck-deck/tests/board_display_page.rs` (new), `web/src/display/api.ts` (`directiveRoute`: `page` → a `mission` route until B3; one test).
- **Changes:** the `Page` variant and mode; §3.2.5 rules; the feature projection on the directive read; `displays.self_page`; §3.2.6 `display_csp`.
- **Tests:** proto `page_takes_only_its_own_fields`; `page_directive_needs_an_existing_entry`; `only_the_owner_may_store_a_page_directive` (station and coordinator senders → `NotGranted`); `deleted_entry_reads_as_mission`; `board_display_page::self_page_answers_only_for_the_current_directive`; `…::every_miss_is_the_same_404`; `…::an_older_wall_reads_mission_for_a_page_directive` (no features header → mission, same generation); `…::page_delete_resets_the_display`; `…::display_document_frame_src_lists_the_page_origins`; `…::an_invalid_stored_origin_is_left_out_of_the_header`; `exactly_these_methods_are_open_to_a_display` lists `displays.self_page`; `next_mode_from_a_page_goes_to_mission`.
- **depends_on:** B1.
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(page) | test(ts_drift)'`; `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_display_page) | test(page_) | test(exactly_these_methods_are_open_to_a_display) | binary(board_displays_directives) | binary(display_directive_station) | binary(coordinator_desktop_wall) | test(display_csp)'`; `cd web && bun run typecheck && bun run test src/display` (through heavy.sh); the guards.

### B3 (ux): Wall pages settings, "Show a page…", the page pane (≈ 560 lines)

- **Owned files:** `web/src/app/routes.ts` (`WallMode` gains `page`; `WallRoute.page?: { id: string }`), `web/src/display/api.ts` (`directiveRoute` for `page`), `web/src/wall/PagePane.tsx` (new), `web/src/wall/WallPage.tsx`, `web/src/wall/model.ts` (`withMode` for `page`), `web/src/wall/page.test.tsx` (new), `web/src/displays/WallPages.tsx` (new), `web/src/displays/ShowPage.tsx` (new), `web/src/displays/DisplaysTab.tsx`, `web/src/displays/DisplayRow.tsx`, `web/src/displays/actions.ts`, `web/src/displays/displays.test.tsx`, `web/src/remote/Pad.tsx` (the "Page" area), `web/src/remote/mirror.ts` (the page card count), `web/src/styles/wall.css`, `web/src/styles/displays.css`.
- **Changes:** §3.2.7 and §3.2.8 completely; the owner wall and the mirror show the page card.
- **Tests:** `page.test.tsx`: `credentialless_is_set_before_src`; `no_credentialless_support_shows_the_text`; `a_same_origin_url_is_refused`; `the_frame_has_the_exact_sandbox_and_allow`; `long_fit_scrolls_by_page_controls`; `a_frame_src_violation_reloads_once_then_shows_the_text`; `the_owner_wall_shows_a_card_and_no_frame`. `displays.test.tsx`: `add_page_asks_for_elevation`; `the_deck_error_text_shows_under_the_field`; `delete_confirms_with_the_exact_text`; `show_a_page_sends_a_page_directive`; `no_pages_shows_the_empty_text`. `routes`: `page_mode_round_trips`; `page_mode_without_a_page_is_mission`.
- **depends_on:** B2, A3.
- **Acceptance:** typecheck, lint; `cd web && bun run test src/wall src/displays src/remote src/display src/app` (through heavy.sh); `sh tools/design-check.sh`; gallery shots of Settings › Displays and a wall in page mode (both themes, desktop and phone); the guards.

### C1 (backend, after B15): wall apps on the deck (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-proto/src/display_directive.rs` (`app` field), `crates/fleetdeck-proto/src/wall_apps.rs` (new), `crates/fleetdeck-proto/src/coordinator_desktop.rs` (the `app` argument of `wall_show`), `crates/fleetdeck-proto/src/version.rs`, `web/src/gen/**` (generated only), the next free migration file `crates/fleetdeck-deck/src/store/display_apps_schema.rs` (new), `crates/fleetdeck-deck/src/store/schema.rs`, `crates/fleetdeck-deck/src/store/displays.rs`, `crates/fleetdeck-deck/src/http/display_routes.rs` (setting + elevation), `crates/fleetdeck-deck/src/http/display_artifact.rs`, `crates/fleetdeck-deck/src/http/display_app_feed.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/methods_tests.rs`, `crates/fleetdeck-deck/src/coordinator_desktop/wall.rs`, `crates/fleetdeck-deck/tests/board_wall_apps.rs` (new).
- **Changes:** §3.4.2 and §3.4.3; `feature::WALL_APPS = "wall_apps.v1"`.
- **Tests:** `app_policy_only_with_directive_flag_setting_html_and_feature` (a table over the four conditions; only all four give `APP_CSP`); `allow_apps_needs_elevation_to_turn_on`; `allow_apps_change_resets_the_directive`; `feed_holds_only_the_author_lineage_inside_the_scope`; `feed_is_404_without_an_app_directive`; `feed_never_holds_tool_input_or_mail` (sentinel test, as §5B item 1 of the parent); `wall_show_app_maps_to_the_directive_flag`.
- **depends_on:** B2, and the B15 deck task that adds `APP_CSP`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_wall_apps) | binary(board_display_artifact) | binary(coordinator_desktop_wall) | test(exactly_these_methods_are_open_to_a_display)'`; `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(ts_drift)'`; the guards.

### C2 (ux, after B15): wall apps on the board (≈ 300 lines)

- **Owned files:** `web/src/wall/DisplayArtifactPane.tsx`, `web/src/wall/appFeed.ts` (new), `web/src/wall/apps.test.tsx` (new), `web/src/displays/DisplayRow.tsx`, `web/src/wall/SendToWall.tsx`, `web/src/display/api.ts` (`wall_apps.v1` in the features header), `web/src/displays/displays.test.tsx`.
- **Changes:** the app branch of the pane with the B15 `AppFrame` (`role: 'wall'`) and the 5 s feed; the "Allow apps on this wall" switch with elevation; the "Run it as an app" checkbox.
- **Tests:** `an_app_directive_renders_appframe_with_role_wall`; `the_feed_is_polled_and_passed_as_data`; `without_app_the_frame_stays_inert`; `the_switch_asks_for_elevation`; `run_it_as_an_app_shows_only_for_a_single_html_file_and_an_allowing_display`.
- **depends_on:** C1, and the B15 task that adds `AppFrame`.
- **Acceptance:** typecheck, lint; `cd web && bun run test src/wall src/displays src/display` (through heavy.sh); `sh tools/design-check.sh`; the guards.

---

## 7. Decisions made in this draft

1. **Controls are a transient relay, not directives.** Rejected: new directive kinds (`scroll`, `select`, …) stored in `display_directives` (a migration, a 2 s delay, an audit row for each key, and state with no meaning after a reload).
2. **Delivery over the display WebSocket, only to sockets that declare `wall_control.v1`.** Rejected: piggyback on the 2 s directive poll (too slow for a remote); push to every socket (old clients reconnect on an unknown frame, fact 19).
3. **A control is bound to the directive generation.** Rejected: unbound controls (a late control could act on the next view).
4. **Owner-only senders for controls and pages.** Rejected: coordinators, members, the station hotkeys, the display itself.
5. **The remote is a board-side mirror plus an on-demand P5a screenshot.** Rejected: P6 screenshots or a stream with P7 input into the wall window (P7 refuses wall windows; owner input is a new command path; 867 KB stills; high cost).
6. **The mirror computes from owner data; the wall reports nothing.** Rejected: a display→deck view report (it would break "a display may only subscribe", fact 20).
7. **Scroll in discrete steps (line = 10 %, page = 90 %, top, bottom) on named areas.** Rejected: pixel offsets from the remote (the remote does not know the wall's layout).
8. **A remote hold of 120 s stops auto-focus and ambient after each control.** Rejected: no hold (auto-focus takes the screen during a scroll); a pin (persistent and stored).
9. **Pages are an owner allow-list referenced by id.** Rejected: a free `url` in a directive; "local network only" (it excludes public docs and dashboards).
10. **The page frame is `credentialless` and refused without support.** Rejected: a normal frame (the display cookie reaches other ports of the deck host); a separate preview window as the main path (no remote scroll; kept as the fallback "Open a link").
11. **The browser enforces the allow-list through `frame-src` origins of the display document.** Rejected: URL-level matching in JS (cannot see navigations inside a cross-origin frame).
12. **`http` only for loopback, private, tailnet and `.local`/`.ts.net` hosts.** Rejected: any `http` host.
13. **A long fit (a frame 8 screens tall) gives scroll for pages and HTML artifacts.** Rejected: scripts injected into the frame (breaks the inert rule); no scroll.
14. **One deck-wide page list.** Rejected: one list for each display (owner question Q1).
15. **Adding a page needs elevation; deleting does not.** Rejected: no elevation (the list decides what a kiosk loads).
16. **An older wall reads a `page` directive as `mission`, through `X-FleetDeck-Features`.** Rejected: send `page` to every wall (an old bundle cannot draw it).
17. **Wall apps reuse the B15 host as role `wall`, off by default for each display, with a feed of the author's lineage and no app actions.** Rejected: a separate wall bridge; a feed of the whole display scope (an app can leak through WebRTC).
18. **Board feature tokens join `feature::ALL`**, the same as `artifacts_video.v1`. Rejected: a separate board feature list (a second discovery route).
19. **The audit writes one `display.remote` row for each burst of controls.** Rejected: one row for each control.

---

## 8. Owner questions (taste or policy)

| Id | Question | Recommended default |
| --- | --- | --- |
| Q1 | One list of wall pages for the whole deck, or one list for each display? | One list for the deck. |
| Q2 | Which hosts may use plain `http`? | Loopback, private networks, the tailnet range, `.local` and `.ts.net` names only. |
| Q3 | May a coordinator (an agent with the `desktop` class) show a wall page or send controls? | No, not in this track. |
| Q4 | How long does the wall wait after a remote control before auto-focus or ambient can take the screen? | 120 seconds. |
| Q5 | Add Omarchy hotkeys for the remote (for example `Super+Alt+J/K` items, `Super+Alt+Return` open, `Super+Alt+Escape` back)? | Not in this track. |
| Q6 | May the owner widen the feed of a wall app to the whole display scope? | No: the author's lineage only. |

---

## Decision log entries (to append)

## DL-470: B17 wall controls are a transient owner relay (architect, 2026-10-23)

- Context: the owner cannot scroll the wall or move between its items from the board or the phone (B17 want 1). Directives are durable "what the wall shows" state, and the wall keys act only at the kiosk.
- Decision: a closed control enum (`scroll { area, step }`, `select { dir }`, `open`, `back`, `mode { dir }`) with no text field. The owner sends it with `POST /api/displays/{id}/control` (Drive, owner only). The deck keeps it in memory (32 for each display, 30 s, 10 per second) and pushes it as the `wall_control` WebSocket frame only to display sockets that declare `wall_control.v1`. Each control is bound to the directive generation. The wall applies it like its keys and holds auto-focus and ambient for 120 s.
- Rejected: new directive kinds; delivery by the 2 s directive poll; coordinator, member, station or display senders.

## DL-471: B17 remote control is a board-side mirror (architect, 2026-10-23)

- Context: B17 want 3 asks for a remote desktop for the wall browser only.
- Decision: the board route `#/remote/<display_id>` renders `WallPage` from the owner's data, narrowed to the display's effective scope, at the bound monitor's logical size, with the display's directive (`GET /api/displays/{id}/remote`) and the same controls. "Take a screenshot of the wall" reuses the P5a owner screenshot of the bound monitor. No pixels stream and no input reaches the wall.
- Rejected: P6 screenshots or a stream with P7 input into the wall window (P7 refuses wall windows on purpose; one dev1 still was 867 KB, above the 700 KiB MCP limit); a display→deck view report.

## DL-472: B17 wall pages are an owner allow-list in a credentialless frame (architect, 2026-10-23)

- Context: B17 want 2 asks for custom URLs on the wall. The kiosk holds the read-only display login.
- Decision: the owner keeps at most 50 "Wall pages" (name, URL, fit) on the deck, added with elevation. A `page` directive names an entry by id. The wall shows it in an `<iframe credentialless sandbox="allow-scripts allow-same-origin allow-forms" allow="" referrerpolicy="no-referrer">`, and refuses to show it without `credentialless` support or for a board origin. The display document's `frame-src` lists the entry origins, so the browser blocks navigation to other origins. `https` anywhere; `http` only for loopback, private, tailnet, `.local` and `.ts.net` hosts. Migration V48 adds `wall_pages` and rebuilds `display_directives` with the mode `page`. An older wall reads a `page` directive as `mission`.
- Rejected: a free URL in a directive; a normal frame; a separate window as the main path (it stays as Desktop › Open a link).

## DL-473: B17 scroll model (architect, 2026-10-23)

- Context: nothing on the wall can scroll from a key or a directive; the focus chat clips older lines; an opaque artifact frame cannot be scrolled from outside.
- Decision: named scroll areas (`needs`, `stream`, `chat`, `changes`, `artifact`, `page`) and discrete steps (10 % line, 90 % page, top, bottom). The focus chat becomes scrollable with a "Scrolled back. New lines are below." note. Pages with the long fit and HTML artifacts after the first downward scroll use a frame 8 screens tall inside a scrolled container.
- Rejected: pixel offsets from the remote; scripts in the inert artifact frame.

## DL-474: B17 wall apps share the B15 app host (architect, 2026-10-23)

- Context: B17 want 4 asks for agent-made live UIs on the wall; B15 designs MCP Apps in the chat.
- Decision: phase C, after the B15 host. An `artifact` directive with `app: true` on a display with "Allow apps on this wall" (off by default, elevation to turn on) is served with the B15 app policy (scripts, `connect-src 'none'`) and rendered in the B15 `AppFrame` with the role `wall`. The only app request on a wall is `ui/initialize`. The wall sends the feed `notifications/fleetdeck/wall-data` with the cards of the author session and its spawn lineage inside the display scope. Coordinator `wall_show` gets an optional `app` flag.
- Rejected: a separate wall bridge; a feed of the whole display scope; app actions on the wall.

## DL-475: B17 backlog corrections (architect, 2026-10-23)

- The directive is a tagged enum with typed references, not `{ mode, artifact_url, … }`. The station sender uses `desktop_shell.v1` and two intents. A display reads artifacts through the inert display artifact route, not `/a/{token}/`, so scripts never run on a wall. Desktop › Open a link already opens any web page in its own window on a desktop monitor. P7 refuses wall windows. The wall cannot scroll at all today. No owner route reads a display's current directive. Side finding for the MCP track: `desktop_info` refuses a grant station given by name and accepts its id.
