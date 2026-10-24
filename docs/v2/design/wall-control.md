# More control of the wall: controls, a remote, wall pages and wall apps (B17)

- **Status:** Draft, revision 2 (2026-10-23), design agent of track T7 (`wall-control`). Not accepted. Revision 2 answers the adversary DESIGN REVIEW round 1 (the last section lists each finding and the change). The architect decides; an adversary DESIGN REVIEW is required before the build (risk **high**, §5).
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

**Answer: navigation (open, back, the next or previous view) is an owner directive; the transient steps (select an item, scroll) are a narrow, typed control channel that only the wall page reads. Neither path is synthesized input.**

- A directive is durable state: "what the wall shows". The deck stores it, checks it on every read and restores it after a reload (facts 5, 6). "Open", "Back" and "Next view" change what the wall shows, so the remote sends them as owner directives (`POST /api/displays/{id}/remote/navigate`, §3.1.2). Every device then reads the same view from the deck, also after a control expired or the deck restarted (revision 2, finding 5). The deck pushes a `navigate` frame, so the wall reads the new directive at once and not after the 2 s poll.
- A scroll or a "next item" is a transient step. Storing it in `display_directives` would cost an audit row for each key and a 2 s poll delay. The deck keeps it in memory with a small checkpoint of its result (the selected item and the scroll position of a long frame, §3.1.3), so that a new or late remote can start from the same state as the wall.
- Synthesized input (keys into the kiosk) is the P7 path. P7 refuses wall windows (fact 17), and key synthesis cannot be tied to the wall document.
- A control is a closed enum (`scroll { area, step }`, `select { item }`, §3.1.1). Its only reference is the selected session of `select`, and the deck checks that this session is inside the display's effective scope (the same check as the session of a `focus` directive). The deck relays the control to the display's own WebSocket in about one round trip. A control can only move inside what the display may already show: it cannot widen the scope or load anything.
- **Senders: the owner only** (board and phone are the same owner login), scope Drive, owner only, the same as a directive (fact 7). Not the display itself (read-only by construction), not members or share guests, not coordinators in this track (they already choose what the wall shows with `wall_show`; an agent that scrolls a wall has no clear use), not the station hotkeys in this track (owner question Q5).

### 2.2 "Custom URLs: a new `url` mode in a sandboxed frame? Which URLs? The wall principal and the kiosk profile must not leak."

**Answer: a new `page` mode that names an entry of an owner allow-list ("Wall pages") by id. The wall shows the page in a credentialless, sandboxed frame. The allow-list chooses the site that fills the pane. It is not a boundary for the content that this site embeds (revision 2, finding 3).**

- **Which URLs:** only the entries that the owner adds on the board (with elevation). `https` for any host; `http` only for loopback hosts (`localhost`, `*.localhost`, `127.0.0.0/8`, `[::1]`), because the wall document is `https` and the browser blocks every other `http` frame as mixed content (§3.2.3; revision 2, finding 4). Never an origin of the board itself. At most 50 entries.
- **Why a list and not "any URL":** the page runs as the owner's UID on the wall machine, in that machine's network position, and on a screen that people trust. A free URL in a directive would make every directive sender a page loader. The list moves the decision to one elevated owner act, and the directive stays a typed reference by id (the rule of §5A item 3: "never a URL").
- **No leak of the wall principal or the kiosk profile** (the full list is in §5.2):
  1. `credentialless` frame: the page gets no cookie and no stored state of the kiosk profile, and its own storage is thrown away. A browser without `credentialless` does not show the page at all.
  2. The deck refuses an entry whose origin is a board origin, and the wall checks it again at render time. A same-origin frame with scripts could reach the wall document.
  3. `sandbox="allow-scripts allow-same-origin allow-forms"` with no `allow-top-navigation*`, `allow-popups`, `allow-modals` or `allow-downloads`: the page cannot move the kiosk away from the wall or open a window in the kiosk profile.
  4. `allow=""` (no camera, microphone, geolocation, fullscreen or local-network access) and `referrerpolicy="no-referrer"`.
  5. The wall ignores every `message` event from a page frame.
  6. The browser applies items 1, 3 and 4 also to every frame that the page embeds: a frame inside a credentialless frame is credentialless, the sandbox flags pass to nested frames, and a nested frame cannot get a permission that its container denies.
  7. The display document's `frame-src` holds the origin of the one page that the display shows now, and only for a request with that display's own login (§3.2.6). It keeps the pane's own frame on that origin (a navigation or a redirect of that frame to another origin is blocked). It does **not** control what the page embeds: an allowed page can show a frame of any origin inside itself, the same as in a normal browser tab. The owner adds a site only when the owner trusts the site and what it embeds. The settings text says this.
- **Sites that refuse to be framed** (`X-Frame-Options`, `frame-ancestors`; for example GitHub, and Grafana by default) show their own error inside the frame. For them the owner keeps "Open a link" (fact 14), which opens a separate window with a fresh profile. The Wall pages section says this.

### 2.3 "Remote control of the wall only: a live view (P6 screenshots or a stream) plus input that P7 limits to the wall window, or a board-side mirror?"

**Answer: the board-side mirror. Recommended and designed in §3.3. The pixel and input path is rejected for this track.**

| Criterion | A. Mirror (route + directive + controls, no pixels) | B. P6 screenshots or a stream + P7 input on the wall window |
| --- | --- | --- |
| What the owner sees | The same `WallPage` component, fed with the owner's data, narrowed to the display's effective scope, at the wall's own size, with the same directive and the same controls | Real pixels |
| Latency | One HTTP POST for a control or a navigation, then one poll; the mirror applies the control from that poll | A P5a/P6 capture is one still for each call (dev1: 867 KB PNG, fact 16); a stream needs a new video path (P5b records to an MP4 file only) |
| Cost | Small: one deck relay, one board page | Large: capture loop or streaming, new owner input path |
| Security | No new pixels leave the machine; controls are a closed enum with no text; no new display write | P7 refuses wall windows on purpose (fact 17); owner input is a new command path (P7 is coordinator-only); pixels of a wall can show a page or app that the owner did not expect |
| Fidelity | The same view (from the directive), the same selected item and the same long-frame position (from the deck checkpoint); other scroll positions are approximate; not the same pixels; keys pressed on the kiosk keyboard are not mirrored; a page or app frame shows as a card | Exact |
| Works for pages | Scroll position only (a card with "Screen N of 8") | Yes, also clicks |

The mirror covers the owner's need ("see the wall and drive it from another device") for every Fleet Deck view, and scroll for pages. For an exact check, the Remote page has the button "Take a screenshot of the wall": it reuses the built owner screenshot (P5a, fact 15) of the bound monitor. That gives real pixels on demand without a new path. Clicks inside a page stay out of scope (§3.6).

### 2.4 "AI-generated wall UIs: an agent publishes an HTML artifact with a small bridge … Do B15 and this item share one app host?"

**Answer: yes, one app host. The wall is a second "role" of the B15 app frame. Phase C of this track builds the wall role after the B15 host exists.**

- Today an agent can publish an HTML artifact and show it with `wall_show`, but it is inert (fact 12).
- Design (§3.4): an `artifact` directive gets `app: true`; the display artifact route then serves the file with the app policy of B15 (scripts, no network) instead of `INERT_CSP`, but only when the owner turned on "Allow apps on this wall" for that display. The wall renders it in the B15 `AppFrame` with the role `wall`. The frame URL declares `wall_apps.v1` in its query (`?features=wall_apps.v1`), because a frame navigation cannot carry a request header (revision 2, finding 2).
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
//! wall display, and the remote state that the deck keeps for them.
//! Navigation (open, back, the next or previous view) is not a control: it
//! is an owner directive (`RemoteNavigate`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::display_directive::{DirectiveBody, DirectiveView, SessionRef};

/// The most controls that the deck keeps for each display (for the
/// incremental poll of a remote; the checkpoint does not expire).
pub const MAX_CONTROLS_KEPT: usize = 32;
/// The age after which the deck drops a kept control, in milliseconds.
pub const CONTROL_KEEP_MS: u64 = 30_000;
/// The most controls and navigations for each display in one second.
pub const MAX_CONTROLS_PER_SECOND: usize = 10;
/// How long auto-focus and the ambient timer wait after a control or a
/// navigation, in milliseconds (owner question Q4).
pub const REMOTE_HOLD_MS: u64 = 120_000;
/// The scroll range of a frame with the long fit (8 screens tall, so 7
/// screens to scroll), in tenths of a screen.
pub const FRAME_TENTHS_MAX: u8 = 70;

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

/// One control. A closed list; an unknown op or field is a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "display/")]
pub enum WallControl {
    Scroll { area: WallArea, step: ScrollStep },
    /// The selected item, absolute: the sender computes it. `None` clears
    /// the selection. The deck checks that the session is inside the
    /// display's effective scope.
    Select { item: Option<SessionRef> },
}

/// The body of `POST /api/displays/{id}/control`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct WallControlBody { pub control: WallControl }

/// The body of `POST /api/displays/{id}/remote/navigate`: the directive that
/// the remote computed for "Open", "Back" or the next or previous view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct RemoteNavigate { pub directive: DirectiveBody }

/// The checkpoint of one directive generation: the result of every control
/// of that generation, in `seq` order. A new generation starts from
/// `WallRemoteState::default()` (no selection, the frame at the top).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallRemoteState {
    pub selected: Option<SessionRef>,
    /// The position of a frame with the long fit (a page, or an HTML
    /// artifact), in tenths of a screen, `0..=FRAME_TENTHS_MAX`.
    pub frame_tenths: u8,
}

impl WallRemoteState {
    /// The state after `control`: `select` sets `selected`; `scroll` of the
    /// area `page` or `artifact` moves `frame_tenths` (`line_up` -1,
    /// `line_down` +1, `page_up` -9, `page_down` +9, `top` 0, `bottom` 70,
    /// clamped to 0..=70); every other scroll leaves the state as it is.
    #[must_use]
    pub fn after(&self, control: &WallControl) -> Self;
}

/// What a frame reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum WallEvent {
    /// The state now: sent when a display socket attaches and after it
    /// lagged. It uses the current `seq` (no new number), never sets the
    /// hold, and is never kept.
    Sync,
    /// The owner remote stored a new directive with `generation`: read the
    /// directive now, and hold.
    Navigate,
    /// A control: apply it, and hold.
    Control { control: WallControl },
}

/// One relayed frame. `state` is the checkpoint of `generation` after this
/// frame, so a receiver that missed frames still has the exact selection
/// and frame position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallControlFrame {
    /// The id of the relay (a new one at each deck start): 32 random bytes
    /// as hex (`store::auth::random_token`).
    pub incarnation: String,
    pub seq: u64,
    pub generation: u64,
    pub event: WallEvent,
    pub state: WallRemoteState,
}

/// The answer of the control and navigate POSTs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallControlAck { pub incarnation: String, pub seq: u64, pub generation: u64 }

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
    pub incarnation: String,
    /// The sequence number of the newest control or navigation (0: none
    /// since the deck started).
    pub seq: u64,
    /// The checkpoint of `directive.generation` (the default when no control
    /// of this generation exists).
    pub state: WallRemoteState,
    /// What is left of the hold, in milliseconds (0: no hold).
    pub hold_left_ms: u64,
    /// True when `controls` holds every frame with a `seq` above `after`
    /// (no gap). False when `after` is above `seq` or older frames expired.
    pub complete: bool,
    /// The kept frames with a `seq` above `after`, oldest first (`navigate`
    /// and `control` events only).
    pub controls: Vec<WallControlFrame>,
}
```

New variant of `BoardServerMessage` (`crates/fleetdeck-proto/src/board.rs:274-349`):

```rust
/// A wall frame for a display socket that declared `wall_control.v1`
/// (B17 §3.1). No other socket gets it.
WallControl { frame: crate::wall_control::WallControlFrame },
```

TypeScript (generated by `make ts-types`): `WallArea`, `ScrollStep`, `WallControl`, `WallControlBody`, `RemoteNavigate`, `WallRemoteState`, `WallEvent`, `WallControlFrame`, `WallControlAck`, `WallLink`, `WallRemoteView` in `web/src/gen/display/`, and the `wall_control` member of `BoardServerMessage`.

#### 3.1.2 Routes

| Method row | Route | Scope | Who | Body / answer |
| --- | --- | --- | --- | --- |
| `displays.control` | `POST /api/displays/{id}/control` | Drive | owner only (no elevation, the same as a directive) | `WallControlBody` (at most 512 bytes) → `WallControlAck` |
| `displays.remote_navigate` | `POST /api/displays/{id}/remote/navigate` | Drive | owner only (no elevation, the same as a directive) | `RemoteNavigate` (the body limit of the directive route) → `WallControlAck` |
| `displays.remote` | `GET /api/displays/{id}/remote?after=<u64>` | Read | owner only | `WallRemoteView` |

No `.display()` marker on any row. The test `exactly_these_methods_are_open_to_a_display` (`crates/fleetdeck-deck/src/http/methods_tests.rs:105`) stays as it is and proves it.

#### 3.1.3 The deck relay (new file `crates/fleetdeck-deck/src/wall_control.rs`)

`WallControlHub` is a field `wall_controls: Arc<WallControlHub>` of `Board` (`crates/fleetdeck-deck/src/http/mod.rs:106`). It is memory only. A deck restart forgets it, and the new hub has a new `incarnation`.

```rust
pub struct WallControlHub {
    incarnation: String,                        // random_token() at creation
    slots: Mutex<HashMap<String, Slot>>,        // key: display_id
}
struct Slot {
    seq: u64,                                   // last seq of this display
    generation: u64,                            // the generation of `state`
    state: WallRemoteState,                     // the checkpoint; no expiry
    hold_until: Option<Instant>,                // last control or navigate + REMOTE_HOLD_MS
    kept: VecDeque<(Instant, WallControlFrame)>,// ≤ MAX_CONTROLS_KEPT, ≤ CONTROL_KEEP_MS old
    tx: tokio::sync::broadcast::Sender<WallControlFrame>, // capacity 64
    ready: usize,                               // open display sockets with the feature
    older: usize,                               // open display sockets without it
    recent: VecDeque<Instant>,                  // rate window of 1 s
    last_audit: Option<Instant>,
}
pub enum PushEvent { Navigate, Control(WallControl) }
impl WallControlHub {
    /// A display socket that declared `wall_control.v1`: a receiver, the
    /// `sync` frame for `generation` (the current effective generation), and a
    /// guard that decrements `ready` on drop.
    pub fn attach_ready(&self, display_id: &str, generation: u64)
        -> (broadcast::Receiver<WallControlFrame>, WallControlFrame, SocketGuard);
    /// A display socket without the feature: a guard that decrements `older`.
    pub fn attach_older(&self, display_id: &str) -> SocketGuard;
    pub fn link(&self, display_id: &str) -> WallLink;
    /// The `sync` frame for `generation` (also after a lag).
    pub fn sync(&self, display_id: &str, generation: u64) -> WallControlFrame;
    /// Rate check; a `generation` below `slot.generation` is Err(Stale) and
    /// records nothing (build answer 2); a newer `generation` resets the state
    /// to the default and `slot.generation = generation`; seq + 1; the
    /// new state (`after` for a control, unchanged for navigate); the hold;
    /// keep; broadcast. All of it under the slot lock, so the broadcast order
    /// is the `seq` order. Err(TooMany) above the rate.
    pub fn push(&self, display_id: &str, generation: u64, event: PushEvent, now: Instant)
        -> Result<WallControlFrame, PushError>;
    /// The remote view parts for `generation`: incarnation, seq, the state
    /// (the default when `slot.generation != generation`), hold left,
    /// complete, and the kept frames above `after`.
    pub fn view(&self, display_id: &str, generation: u64, after: u64, now: Instant) -> RemoteParts;
    /// Revoke: drops the slot (receivers end).
    pub fn forget(&self, display_id: &str);
}
```

`complete` is true when `after == seq`, or when `after < seq` and the oldest kept frame has `seq <= after + 1`. It is false when `after > seq` and when frames between `after` and the oldest kept frame expired.

`POST /api/displays/{id}/control`, in order:

1. `owner(...)` check (the helper of `display_directive_routes.rs:137`).
2. Parse; a parse error is `400 invalid_control` "The control is not valid." (fixed text).
3. `effective_directive(&board.link, &id)` (`crates/fleetdeck-deck/src/coordinator_desktop/directive.rs:159`). `None` → `404 display_unknown`.
4. For `select` with `Some(item)`: `effective_scope(&board.link, &id)` (`directive.rs:178`), then `board.store.session_summary(item.station_id, item.session_id)`; the session must exist and `display_view::in_scope(&scope, &item.station_id, repo_root)` (`crates/fleetdeck-deck/src/http/display_view.rs:106`) must be true. Else `400 invalid_control` with the same fixed text.
5. `link(id)`: `Offline` → `409 wall_offline` "The wall is not connected."; `Older` → `409 wall_controls_unsupported` "This wall runs an older board. Reload the wall to use the remote."
6. `push(id, view.generation, PushEvent::Control(control))`. Above 10 in one second → `429 too_many_controls` "Too many controls. Wait a moment." A `Stale` push → `409 wall_changed` "The wall changed its view. Try again." Steps 3 to 6 run inside `hub.turn(id)` (build answer 2).
7. Audit: one row `display.remote`, target the display id, detail `start` for the first accepted control or navigation of a display after 10 minutes without one. No row for each control (10 per second would flood the audit). A refused POST writes no row.
8. Answer `WallControlAck { incarnation, seq, generation }`.

`POST /api/displays/{id}/remote/navigate`, in order:

1. The rate check of the hub (without a push): above 10 in one second → `429 too_many_controls`.
2. The same function as `POST /api/displays/{id}/directive` (`display_directive_routes.rs:131-149`): extract its body after the parse into `pub(super) async fn owner_set(board, principal, id, body) -> Result<DirectiveView, ApiError>`, and call it from both routes. The owner check, the checks of `write_directive`, the errors (`400 invalid_directive`, `403 outside_ceiling`, `403 not_granted`, `404 display_unknown`) and the `display.directive` audit row are the same as for the directive route.
3. `push(id, stored.generation, PushEvent::Navigate)`. A `TooMany` here (a race with another sender after step 1) or a `Stale` is ignored and logged at `debug`: the directive is stored, and the wall reads the directive that holds with its next poll. Steps 2 and 3 run inside `hub.turn(id)` (build answer 2).
4. The `display.remote` audit rule of the control route (step 7).
5. Answer `WallControlAck`.

There is no link check: the directive is durable. An offline wall shows it when it connects; an older wall reads it with its 2 s poll.

`GET /api/displays/{id}/remote?after=N`: owner check; effective directive (`None` → 404); `view(id, directive.generation, N)`; `link(id)`.

Revoke (`display_routes.rs:717`) calls `wall_controls.forget(id)`.

#### 3.1.4 WebSocket delivery

- `ws.rs::upgrade` (`crates/fleetdeck-deck/src/http/ws.rs:141-176`) reads the declaration with a new helper `declares(headers, query, token)` in `crates/fleetdeck-deck/src/http/artifact_projection.rs`, next to `video_aware_socket` (fact 18): the header `X-FleetDeck-Features` or the query `features=`, a comma, space or tab separated token list. `video_aware` and `video_aware_socket` call it. Put the display forwarding into a new file `crates/fleetdeck-deck/src/http/ws_wall_control.rs`, because `ws.rs` has 774 lines (the module limit is 800).
- A display socket with the feature reads the effective generation, calls `attach_ready`, and sends the `sync` frame first. The session loop also selects on the receiver and sends `BoardServerMessage::WallControl` for each frame. On `RecvError::Lagged` it reads the effective generation again and sends `hub.sync(...)`: the selection and the frame position are exact again; a lost relative scroll is lost. A display socket without the feature calls `attach_older`. Owner and member sockets never attach and never get the frame.
- The frame passes the same send deadline as other display frames (`ws_display_send`, `http/mod.rs:127-129`).

#### 3.1.5 Behavior on the wall

New pure module `web/src/wall/control.ts`:

```ts
export type ControlState = {
  /** The relay of the last frame (`null` before the first). */
  readonly incarnation: string | null;
  /** The last `navigate` or `control` seq of this incarnation (0 before the first). */
  readonly seq: number;
  /** The generation of `remote` (`null` before the first frame). */
  readonly generation: number | null;
  readonly remote: WallRemoteState;
  /** Unix ms until which auto-focus and the ambient timer wait. */
  readonly holdUntil: number;
};
export const initialControl: ControlState;
export type ControlEffect =
  | { readonly kind: 'none' }
  | { readonly kind: 'scroll'; readonly area: WallArea; readonly step: ScrollStep };
/** The selectable refs, in reading order: mission → the needs items, then the tiles (no
 *  repeats); race → the columns; every other mode → none. A ref is `station/session`. */
export function selectable(route: WallRoute, needs: readonly string[], tiles: readonly string[],
  columns: readonly string[]): readonly string[];
/** The next or previous ref of `items`, with wrap; no selection (or a selection that is not in
 *  `items`) → the first (next) or the last (previous); empty `items` → `null`. */
export function stepSelection(items: readonly string[], selected: string | null,
  dir: 'next' | 'previous'): string | null;
/** The remote state that the wall draws: `state.remote` when `state.generation` is the shown
 *  generation, else the default. */
export function shownRemote(state: ControlState, shown: number | null): WallRemoteState;
export function applyFrame(state: ControlState, frame: WallControlFrame, ctx: {
  readonly shown: number | null; readonly now: number; }): {
  readonly state: ControlState; readonly effect: ControlEffect; readonly reread: boolean };
```

Rules of `applyFrame`:

1. A new `incarnation` (another value than `state.incarnation`) is accepted, and `seq` restarts from the frame.
2. A `navigate` or `control` frame of the same incarnation with `frame.seq <= state.seq` does nothing (a repeat).
3. `frame.generation` below `shown` does nothing (a control for an old view never acts on a new one).
4. `frame.generation` above `shown` (or `shown` is `null`): keep `generation` and `remote` from the frame, `reread: true` (the wall reads the directive at once), no effect. A `navigate` or `control` frame also sets the hold.
5. `frame.generation` equal to `shown`: `remote = frame.state`. `control` → hold, and the effect `scroll` for the areas `needs`, `stream`, `chat`, `changes`, and for `artifact` when the artifact is not an HTML frame (the long-fit frames use `remote.frame_tenths`, not an effect). `navigate` → hold. `sync` → nothing more.
6. The hold is `holdUntil = now + REMOTE_HOLD_MS`, and it counts as activity for the ambient timer.

Navigation never comes as a control: the wall reaches a new view only through its directive (fact 6). `useDirective` (`web/src/display/useDirective.ts`) gets a `reread()` function that runs one poll at once (without a second poll in flight); the wall calls it when `applyFrame` answers `reread: true`.

`WallPage` (`web/src/wall/WallPage.tsx`) gets three new props:

```ts
/** B17: the frames of the deck (a display socket). */
readonly controls?: ((on: (frame: WallControlFrame) => void) => () => void) | undefined;
/** B17: the remote state and hold that the Remote page computed (the mirror only). */
readonly remote?: { readonly state: WallRemoteState; readonly holdUntil: number;
  readonly effects: readonly ControlEffect[] } | undefined;
/** B17: a mirror on the Remote page: no key listener, no QR codes, never a frame. */
readonly mirror?: boolean | undefined;
```

- Auto-focus treats a hold as a pin: `nextAutoFocus(…, r.pin || now < holdUntil)` (`WallPage.tsx:170`). The ambient timer resets on each hold (`lastChange`, `:139`).
- The selected ref is `shownRemote(...).selected` (as `station/session`).
- A scroll effect scrolls the element with `data-wall-scroll="<area>"`: `line_*` = 10 % of its `clientHeight`, `page_*` = 90 %, `top` = 0, `bottom` = `scrollHeight`. `scrollTo` with `behavior: 'smooth'`, and `'auto'` when Reduce motion is on.
- A long-fit frame (a page with the long fit, §3.2.7, or an HTML artifact) scrolls to the absolute position `frame_tenths × clientHeight / 10` of its container on each change of `frame_tenths`, never relative to the current `scrollTop` (a second step during a smooth scroll then lands on the same place as the deck's count).
- Scroll areas and their elements: `needs` (the needs list, mission), `stream` (the Activity stream, mission), `chat` (the focus chat), `changes` (the change pane of focus and review), `artifact` (the artifact pane), `page` (the page pane, §3.2).
- **Chat scroll:** `.fd-wall-chat` stays `overflow: hidden` (programmatic scroll works on it), but the bottom alignment moves from `justify-content: flex-end` to `margin-top: auto` on `.fd-wall-chatlines`, so that the older lines are reachable. While the chat is not at its end, its foot shows the line "Scrolled back. New lines are below." (`--dim`, `fd-wall-note`), and new lines do not move it. `bottom` returns to follow mode.
- **HTML artifacts:** the opaque frame cannot be scrolled from outside (fact 13). While `frame_tenths` is above 0, the pane uses the long fit: the frame is 8 times the pane height inside the `artifact` scroll container, at the position of `frame_tenths`. At 0 it uses the one-screen fit. Markdown and images scroll in the pane itself (relative effects).
- **Selection look:** the selected tile, needs item or race column gets `data-selected="true"` and an outline in `--focus` (the selection color, DESIGN-SYSTEM §2.2), 3 wall units, offset 2 wall units, in `wall.css`. Forced colors: `outline-color: Highlight`. No new token. A selected ref that the wall does not show has no outline.
- The kiosk keys keep working and stay local (fact 9).

`DisplayRuntime` gives `WallPage` a `controls` feed from its `Connection`, keeps the `ControlState`, and calls `reread()` of `useDirective`. `Connection` (`web/src/runtime/connection.ts`) gets an optional features list: the display environment declares `artifacts_video.v1 wall_control.v1 wall_pages.v1` in `features=`; `decode.ts` validates `wall_control` (an object `frame` with a text `incarnation`, `sequence(seq)`, `sequence(generation)`, an object `event` with a text `kind`, an object `state`) and the connection passes each frame to its control listeners. It keeps the newest frame and gives it to a new listener at once as a `sync` frame (build answer 3). It does not change the read model.

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
| `http` only for a loopback host: `localhost`, a name that ends in `.localhost`, an IPv4 address in 127.0.0.0/8, or `[::1]`. The deck serves the wall over `https` only (`crates/fleetdeck-deck/src/http/guard.rs:112`), and the browser blocks an `http` frame of any other host as mixed content; these four are the hosts that the browser trusts as secure | "Use https for this address. A wall blocks plain http pages, except pages of its own machine (localhost)." |
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

`/display` stays outside the guard, and its answer for a request without a valid display login holds no configuration data (`display_document.rs:10-11`; revision 2, finding 1). Only a request with the display's own login gets the origin of the one page that this display shows now.

`display_document::document` (`display_document.rs:27-35`), in order:

1. `frame_origin` as today (the request origin, when it is a board origin).
2. `page_origin = None`. When the request has the `fd_display` cookie (`guard::cookie(headers, guard::DISPLAY_COOKIE)`) and `board.store.display_session(&value)` gives a login: `effective_directive(&board.link, &login.display_id)`; when it is `page { page }`, read the entry `page.page_id` of `wall_pages`; when the entry exists and its `origin` passes the grammar `^https?://([a-z0-9.-]+|\[[0-9a-f:]+\])(:[0-9]{1,5})?$`, `page_origin = Some(origin)`. The owner cookie (`fd_session`) plays no part: the document never reads it.
3. Any miss or error in step 2 (no cookie, an unknown or revoked login, another mode, a deleted entry, an origin that fails the grammar, a database error) gives `page_origin = None`, and the document still loads. A database error is logged at `warn`, without the cookie.
4. `display_csp(frame_origin, page_origin)`. With a `page_origin`, the answer has `Cache-Control: no-store` (else the current `no-cache`): `BoardAssets::index_with_csp` (`assets.rs:81`) gets a `cache: &'static str` argument, and its other callers pass `"no-cache"`.

```rust
pub fn display_csp(frame_origin: Option<&str>, page_origin: Option<&str>) -> String
// frame-src {origin}/api/displays/self/artifact/ [<page origin>]   (or 'none' [<page origin>])
```

An anonymous request, a request with the owner's or a member's login, and a request of another display never get the page origin of this display. A display gets at most one page origin: the one of its current directive.

**Reload for a new page.** The policy of a document is fixed when the document loads. When the wall gets a `page` directive with a generation `G` and the document URL does not have the query `page_gen=G`, the page pane calls `location.replace('/display?page_gen=' + G + location.hash)` before it creates the frame. The new document has the page origin (step 2), and its first directive poll applies `G` again (`useDirective.ts:1-3`: the first answer always applies). The wall reloads at most once for each page generation. A document that keeps an old page origin after the directive changed holds nothing new: that display already had this origin.

#### 3.2.7 The page pane (new `web/src/wall/PagePane.tsx`)

- The display wall reads `GET /api/displays/self/page/{page_id}` for each new directive generation, after the reload check of §3.2.6.
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
- Fit: `screen` → the frame fills the pane. `long` → the frame is `800%` of the pane height and the section scrolls (`overflow: hidden`, programmatic scroll) to the position `frame_tenths` of the remote state (§3.1.5).
- A `securitypolicyviolation` event with `effectiveDirective === 'frame-src'` (the directive changed during the reload, or the entry changed) does not reload again: the pane shows "The wall could not load this page. Reload the wall." The next page generation reloads as usual (§3.2.6).
- The wall ignores `message` events (no listener).
- The owner's `#/wall` and the mirror never load the page: they show a card with the page name, the host in mono, and for the long fit "Screen N of 8", with `N = floor(frame_tenths / 10) + 1` from the deck checkpoint (§3.1.1).

#### 3.2.8 Board: Settings › Displays › "Wall pages"

A section under the display list (`web/src/displays/DisplaysTab.tsx`), as a `.fd-panel`:

- Heading: "Wall pages". Text: "Pages that a wall may show besides Fleet Deck views. A wall shows a page in a closed frame: the page gets no login and no cookies of the wall, and it cannot open windows or leave the frame. A page can show parts of other sites inside itself. Add only sites that you trust."
- Note (`--dim`): "Some sites refuse to show inside another page. Open those with Desktop › Open a link."
- Note (`--dim`): "Use https addresses. Plain http works only for pages on the wall's own machine (localhost)."
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
2. **The mirror:** a box with the aspect ratio of the bound monitor (logical size = pixels × 100 / `scale_pct`, from `GET /api/stations/{station}/desktop`; no binding or no desktop: 1920 × 1080). Inside, `WallPage` renders at that logical size with `initialSize`, scaled down with `transform: scale(boxWidth / logicalWidth)`, `inert`, `mirror`. Its data: the owner's sessions and stations, with `route.stations` = the display ceiling narrowed by the directive filter (the same rule as `EffectiveScope::narrowed`, `display_directives.rs:56-71`), its route from the directive of `WallRemoteView` (through `directiveRoute`, `web/src/display/api.ts:149-175`), and its `remote` prop from the mirror state (§3.3.3). Caption (`--dim`): "A copy of what the wall shows, made on this device. It is not a picture of the screen."
3. **The pad** (`.fd-panel`), every control a plain button with an icon and an accessible name, 44 px on touch:
   - "Previous view" · the current mode name · "Next view";
   - "Previous item", "Next item", "Open", "Back";
   - what each button sends (the pure function `padAction` in `web/src/remote/mirror.ts`; `current` is the directive of the last `WallRemoteView`, and the new directive keeps its `pin` and `filter`):

     | Button | Sends | Disabled when |
     | --- | --- | --- |
     | "Previous view", "Next view" | navigate: `routeDirective(stepMode(route, -1 or +1, selected ?? first tile), pin, filter)` | the stepped route equals the route |
     | "Open" | navigate: `focus { session: selected }` | the mode is not mission or race, or nothing is selected |
     | "Back" | in mission: control `select { item: null }`; in any other mode (also ambient): navigate `mission` | mission without a selection |
     | "Previous item", "Next item" | control `select { item: stepSelection(items, selected, dir) }` | `items` is empty |
     | Scroll buttons | control `scroll { area, step }` | the mode has no scroll area |

     `routeDirective(route, pin, filter)` is the inverse of `directiveRoute`: mission and ambient → the mode only; focus and review → `session`; race → `batch_id`; artifact → `artifact` (B3 adds page → `page`).
   - a `.fd-seg` of the scroll areas that the current mode has (Mission: "Needs you", "Activity"; Focus: "Chat", "Changes"; Review: "Changes"; Artifact: "Artifact"; Page: "Page"; none in Race and Ambient, and then the scroll buttons are disabled);
   - "Scroll up" (`page_up`), "Scroll down" (`page_down`), "To the top", "To the end".
4. Footer: "Switch to Mission view" (the built directive) and "Take a screenshot of the wall" (only when the display is bound, the station has a desktop and its monitor allows capture; it runs the built owner capture of the bound monitor, `web/src/desktop/capture.ts`, and opens the artifact when it is done).
5. A `<details>` "Keys": "← → views · J K items · Enter open · Esc back · ↑ ↓ scroll a little · Page Up, Page Down a screen · Home, End · [ ] scroll area". The keys act only while the focus is inside the pad.

#### 3.3.3 Behavior

- The page polls `GET /api/displays/{id}/remote?after=<seq>` every 1 s while it is visible (`document.visibilityState`), and every 10 s when it is hidden.
- A pad action sends `POST /api/displays/{id}/control` or `POST /api/displays/{id}/remote/navigate`. The pad sends one action at a time: a button press while a POST is in flight does nothing.
- **The mirror applies nothing from an answer** (revision 2, finding 6). After each answer (success or error) it polls at once. It applies frames only from polls, in `seq` order, so its order is the deck's order also when another device sent an earlier control.
- The mirror state (pure functions in `web/src/remote/mirror.ts`): `{ incarnation, cursor, holdUntil, effects }`. For each `WallRemoteView`:
  1. **Rebase** when it is the first view, when `incarnation` differs from the mirror's (the deck restarted), or when `complete` is false (a gap): `cursor = seq`; the relative scroll areas go back to their default position (top; the chat at its end); no effect from `controls`.
  2. Else apply each frame of `controls` with a `seq` above `cursor`, in order: a `control` frame of `directive.generation` gives its relative scroll effect (the same rule as §3.1.5 rule 5). Then `cursor = seq`.
  3. In both cases: the route comes from `directive`, the selected item and `frame_tenths` from `state`, and `holdUntil = now + hold_left_ms`. So the view, the selection and the long-frame position are always the deck's checkpoint; only the relative scroll positions can differ from the wall.
- Status texts (`role="status"`, cleared after 4 s): success "Sent."; `409 wall_offline` "The wall is not connected."; `409 wall_controls_unsupported` the "older board" text; `429` "Too many controls. Wait a moment."; `404` "This display does not exist any more."; `403 outside_ceiling` and `400` "The wall cannot show this."; other errors: `describe(e)`.
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
- The display artifact route serves `APP_CSP` instead of `INERT_CSP` only when **all** hold: the effective directive is `artifact` with `app: true` for exactly this version; the display has `allow_apps` on (read live on each request); the one file is HTML; the request declares `wall_apps.v1`. Else `INERT_CSP` (unchanged). Every check of `authorize` (`display_artifact.rs:137-185`) runs as today before this choice.
- **The declaration on the real frame load** (revision 2, finding 2). A frame navigation cannot carry the header `X-FleetDeck-Features`, so the route reads the declaration with `declares(headers, query, feature::WALL_APPS)` (§3.1.4): the header or the query `features=`. The wall pane uses one URL for the probe fetch and for the frame: `artifactPath(...) + '?features=wall_apps.v1'`, and passes it as `AppFrame.src`. The frame navigation is same-origin, so the browser sends the `fd_display` cookie (`SameSite=Strict`), and `/api/displays/self/...` answers the display login only (`guard.rs:326-330`). The query grants no authority: without the directive flag, the live `allow_apps` and an HTML file, the answer stays inert. The query does not change the path, so the display document's `frame-src {origin}/api/displays/self/artifact/` still matches. Both answers keep `Cache-Control: no-store` (`display_artifact.rs:118`).
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
| New deck, wall page loaded before the upgrade (old bundle) | It does not declare `wall_control.v1`: no control frame reaches it, and a control POST answers the "older board" text; the Remote page shows it with "Reload the wall". A navigation (open, back, views) still works: it is a directive, and the old wall reads it with its 2 s poll. A `page` directive reads as `mission` (§3.2.5). `app: true` is ignored (inert). |
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
| A control while the directive changes | The control carries the generation of the POST time; the wall drops it when that generation is older than the one it shows (§3.1.5 rule 3). A frame of a newer generation makes the wall read the directive at once (rule 4). |
| Two owner devices drive one wall | Both POST; the deck orders them by `seq`, and each frame carries the checkpoint after it. Each mirror polls after its own answer and applies the frames in `seq` order (§3.3.3), so both mirrors and the wall end with the same selection and frame position. Two "Next item" presses from the same start select the same item (the last one wins). |
| Device A's control has an earlier `seq` than device B's own control, and B has not polled A's control yet | B applies nothing from its answer; its next poll returns A's frame, then B's frame; B applies them in that order (test `another_devices_earlier_control_is_applied_first`). |
| A remote opens after the controls expired (more than 30 s, or more than 32) | `complete` is false or the controls list is empty; the mirror rebases: the view from the directive (a remote "Open" is a directive), the selection and frame position from the checkpoint, and the hold from `hold_left_ms`. |
| A wall with two sockets (a reload in progress) | `ready` counts both; each socket gets the frame; the old page is closing. |
| A wall socket lags (more than 64 frames behind) | The deck sends a `sync` frame: the selection and frame position are exact again; lost relative scroll steps are lost. |
| A wall reconnects (network or reload) | The first frame of the new socket is `sync`: the wall gets the checkpoint of its generation. |
| Deck restart | Controls and checkpoints are lost (memory only); the new hub has a new `incarnation`; `seq` restarts at 0. The wall gets a `sync` frame with the new incarnation and the default state; a mirror sees the new incarnation and rebases. The view stays: it is the stored directive. |
| Display revoked | `forget(id)` ends the receivers; the socket closes within 5 s (existing); POST → 404. |
| `select` with no items (focus, artifact, page, ambient) | Nothing. |
| `scroll` for an area that the mode does not show | Nothing. |
| A page entry deleted while shown | The directive is deleted in the same transaction; the wall returns to Mission on its next poll; `self/page` answers 404 at once. |
| A page redirects to another origin (SSO, `www.`) | The browser blocks it under `frame-src` (the pane's own frame may hold only the entry origin); the frame shows the browser's block page. The owner adds the target as its own entry and shows that entry. |
| An allowed page embeds a frame of another origin | The nested frame loads (the parent policy does not reach it, §2.2 item 7). It is credentialless, sandboxed and without permissions, the same as the page (§2.2 item 6). |
| An entry is `http` on a private address, the tailnet or a `.local` name | The deck refuses it with the text of §3.2.3; the owner uses an `https` address (for example `tailscale serve` for a tailnet service). |
| A page directive arrives while the document has no page origin | The pane reloads the document once with `page_gen=<generation>` (§3.2.6). |
| A page that refuses framing | The frame shows the site's own error. The settings note names "Open a link". |
| A long-fit page that sizes itself to `100vh` | It becomes 8 screens tall. The owner chooses "Fits one screen" for such a page. |
| `long` fit scrolled past the end | The scroll clamps; the mirror count clamps to 7. |
| HTML artifact scrolled | The long fit while `frame_tenths` is above 0; `top` (0) returns to one screen. |
| More than 10 controls per second | `429`; the pad shows the text; nothing is queued. |
| Kiosk keyboard and remote together | Both act on the local state of the wall; the mirror does not see the kiosk keys. |

---

## 5. Risk class: **high**

Reasons: a new owner command path to a display (controls); a migration that rebuilds a table (`display_directives`); third-party web content inside the kiosk of a read-only principal (pages); agent scripts on a wall (apps). Each phase needs an adversary DESIGN REVIEW before its build and a PRE-PR review before its merge.

### 5.1 Threat notes: controls and the mirror

1. **Authority.** Only the owner sends controls and navigations (Drive, owner only); a member, a share guest, a token without Drive, a display and a coordinator get the existing refusals. A navigation is an owner directive and passes every check of the directive route (shared function, §3.1.3). A control can only select an item or scroll, inside the display's existing scope.
2. **No data channel.** A control is a closed enum. Its only reference is the session of `select`, which the deck checks against the display's effective scope (the session exists, its station is in the scope, a repository filter matches) before the relay. The deck relays nothing that a display could not already read. A display frame adds no field to the display wire model (§5A item 2 of the parent). The `incarnation` is random and names nothing.
3. **No new display write.** The display still only subscribes (fact 20). The mirror computes from owner data; it needs no report from the wall.
4. **Flooding.** 10 controls and navigations per second per display; at most 32 kept, 30 s; one checkpoint per display; a broadcast capacity of 64, and a `sync` frame after a lag; the audit writes one `display.remote` row per remote session start, and each navigation writes the usual `display.directive` row.
5. **Old clients.** Frames go only to sockets that declared the feature (fact 19).
6. **Divergence.** The view, the selection and the long-frame position of every mirror and of the wall come from deck state (the directive and the checkpoint), which a restart, a gap or a lag cannot leave stale: a new `incarnation`, `complete: false` and `sync` frames make every receiver take the deck state again.

### 5.2 Threat notes: wall pages

1. **The display credential.** `fd_display` is `HttpOnly; Secure; SameSite=Strict` for the deck host. Cookies ignore the port, so a page on another port of the deck host would receive the cookie in a normal frame. Mitigation: the `credentialless` frame (no cookies at all), and no frame without support. Same-origin entries are refused twice (deck and wall).
2. **The kiosk profile.** The frame storage is ephemeral (credentialless); the sandbox has no top navigation and no popups, so a page cannot take the top-level kiosk document (which is not credentialless) to another site. The browser applies the credentialless state, the sandbox flags and the empty permissions policy also to every frame inside the page (§2.2 item 6); the B3 browser test proves it for a nested frame of another origin.
3. **The allow-list is not a content boundary** (revision 2, finding 3). The document's `frame-src` keeps the pane's own frame on the entry origin (no redirect or navigation of that frame to another origin). It does not reach the frames that the page embeds: an allowed page can show content of any origin inside itself. The owner trusts each added site, and what it embeds, as in a normal browser tab. The settings text says this ("A page can show parts of other sites inside itself. Add only sites that you trust."). Nothing in this design depends on the allow-list for the protection of the display login, the kiosk profile or the wall document: items 1, 2 and 4 of this list hold for nested frames too.
3a. **Configuration exposure** (revision 2, finding 1). The page origins reach only the display that shows the page, and only through its own login (§3.2.6). An anonymous request to `/display`, a request with the owner's or a member's login, and a request of another display get no page origin. The owner reads the list through `GET /api/wall-pages` (owner only). A display also reads its page URL through `displays.self_page`, only while its directive names the page.
4. **The wall machine's network position.** A page runs in the kiosk on the wall machine and can try to reach `localhost` and the LAN. Mitigation: owner-chosen origins only; `allow=""` denies the local-network-access permission in the frame and in its nested frames. Residual: an allowed page that turns hostile can probe what the browser permits. Stated in the settings text ("runs as you on the wall's machine").
4a. **Transport** (revision 2, finding 4). The wall document is `https` (`guard.rs:112`), so the browser blocks an `http` page frame as mixed content unless the host is loopback. The deck accepts `http` only for loopback hosts. Residual: when the kiosk loads the deck by a non-loopback address, Chromium's Local Network Access rules can still block or prompt for a loopback page, and a kiosk cannot answer a prompt. The pane then shows the browser's own error. The B3 manual check on an Omarchy wall records the result in the pull request.
5. **Trusted screen.** A hostile page can show fake Fleet Deck content (a fake request with a QR code). Residual; the owner chooses the origins.
6. **Secrets in URLs.** The URL is stored and sent to the display. The audit holds only the id and the host. The settings warning tells the owner not to put a token in an address.
7. **Header size.** At most 50 origins, each re-validated before it enters the header.
8. **Migration.** The rebuild copies every row and column; a test opens a V47 database with one row of each sender kind, migrates, and reads the rows back unchanged.

### 5.3 Threat notes: wall apps

1. **Scripts on a trusted screen.** Off by default for each display; elevation to turn on; the owner or the author coordinator must set `app: true` on the directive.
2. **Exfiltration.** `connect-src 'none'`, no frames, no forms, opaque origin, no popups, no top navigation. Residual: Chromium does not block WebRTC through CSP, so an app can send what it holds. Mitigation: an app holds only its own document and the feed of its author's lineage, which the author session can read already.
3. **App to host.** The wall role refuses every app request except `ui/initialize` (I-2); messages are accepted only from the frame's own window.
4. **Live authority.** The app policy is chosen per request from the live directive and the live `allow_apps`; a change ends the next request and the pane reloads on the new generation.
5. **The declaration.** `?features=wall_apps.v1` only says that the client renders an app frame (revision 2, finding 2). Anyone with the display login can add it; it changes nothing without the directive flag, the live setting and an HTML file. A request from an older bundle without it stays inert.

---

## 6. Build plan

Order: **A1 → A2 → A3**; **B1** after A1 (it may run in parallel with A2); **B2** after B1 and A2 (both A2 and B2 own `web/src/display/api.ts`; B2 may run in parallel with A3); **B3** after B2 and A3; **C1 → C2** after B2 and the B15 host tasks. Every heavy command runs through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Every task also runs the guards:

```sh
umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'
```

A task that changes a proto type runs `make ts-types` through `heavy.sh` and owns the generated files under `web/src/gen/` that change, and runs the drift test `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(ts_drift)'`. Every UI task follows `docs/v2/DESIGN-SYSTEM.md` (the `fleet-design-system` skill) and runs `sh tools/design-check.sh`.

### A1 (backend): wall control types, relay, checkpoint and owner routes (≈ 720 lines)

- **Owned files:** `crates/fleetdeck-proto/src/wall_control.rs` (new), `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/board.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-proto/src/ts.rs`, `web/src/gen/**` (generated only), `crates/fleetdeck-deck/src/wall_control.rs` (new), `crates/fleetdeck-deck/src/wall_control_tests.rs` (new, the hub unit tests, so that the hub file stays under 800 lines), `crates/fleetdeck-deck/src/lib.rs`, `crates/fleetdeck-deck/src/http/mod.rs`, `crates/fleetdeck-deck/src/serve.rs`, `crates/fleetdeck-deck/tests/common/board.rs`, `crates/fleetdeck-deck/src/http/wall_control_routes.rs` (new), `crates/fleetdeck-deck/src/http/display_directive_routes.rs` (extract `owner_set`, no behavior change), `crates/fleetdeck-deck/src/http/artifact_projection.rs` (the helper `declares`), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/ws.rs`, `crates/fleetdeck-deck/src/http/ws_wall_control.rs` (new), `crates/fleetdeck-deck/src/http/display_routes.rs` (revoke calls `forget`), `crates/fleetdeck-deck/tests/board_wall_control.rs` (new).
- **Changes:** §3.1.1 types, `WallRemoteState::after` and the `WallControl` frame; `feature::WALL_CONTROL = "wall_control.v1"` in `feature::ALL`; the hub of §3.1.3 as a `Board` field (both constructors; the `incarnation` comes from `store::auth::random_token()`, and its error is a deck start error); the three routes of §3.1.2 and their method rows; `owner_set` shared by the directive route and the navigate route; `declares` in `artifact_projection.rs`, with `video_aware` and `video_aware_socket` calling it; the WS attach, the `sync` first frame, the forwarding and the `sync` after a lag of §3.1.4; revoke → `forget`.
- **Tests:**
  - proto `wall_control_ops_are_closed` — the ops `open`, `back` and `mode`, an unknown op, an extra field, and a text `area` are parse errors; `scroll` and `select` (with and without an item) round-trip.
  - proto `remote_state_after_moves_and_clamps_frame_tenths` — each step for `page` and `artifact`; 0 and 70 are the limits; a `chat` scroll leaves the state as it is.
  - proto `remote_state_after_select_sets_and_clears`.
  - deck unit `hub_orders_and_keeps_at_most_32` — 40 pushes keep the last 32 with increasing `seq`.
  - deck unit `hub_drops_controls_older_than_30_s_and_keeps_the_checkpoint` — `view` after 31 s has no frames, `complete` false for `after = 0`, and the state of the last control.
  - deck unit `hub_resets_the_checkpoint_on_a_new_generation`.
  - deck unit `hub_view_is_complete_only_without_a_gap` — `after = seq`, `after` inside the kept range, `after` below the oldest kept frame, `after` above `seq`.
  - deck unit `hub_refuses_the_eleventh_push_in_a_second` — controls and navigations count together; `PushError::TooMany`.
  - deck unit `hub_link_counts_ready_and_older_sockets` — `Offline`, `Older`, `Ready`, and back to `Offline` when the guards drop.
  - deck unit `two_hubs_have_different_incarnations`.
  - deck unit `declares_reads_the_header_and_the_query` — and the existing `a_socket_declares_the_feature_in_the_header_or_the_query` passes unchanged.
  - `board_wall_control::a_declaring_display_socket_first_gets_a_sync_frame` — with the checkpoint of its generation.
  - `…::owner_control_reaches_a_declaring_display_socket` — the `control` frame has the `seq`, the generation and the incarnation of the ack, and the state after the control.
  - `…::select_outside_the_scope_is_refused` — a session of a station outside the ceiling, a session that does not exist, and a session outside the repository filter: `400 invalid_control`, no frame.
  - `…::a_display_socket_without_the_feature_gets_no_frame_and_the_post_answers_older` — 409 `wall_controls_unsupported`, and the socket gets no frame.
  - `…::no_socket_answers_wall_offline` — 409.
  - `…::navigate_stores_an_owner_directive_and_pushes_a_navigate_frame` — the directive route then reads the new body and generation; one `display.directive` audit row.
  - `…::navigate_works_without_a_socket` — the directive is stored, no 409.
  - `…::navigate_has_the_checks_of_the_directive_route` — outside the ceiling 403 `outside_ceiling`; an unknown session 400 `invalid_directive`.
  - `…::owner_and_member_sockets_never_get_a_control_frame`.
  - `…::member_token_and_display_are_refused` — for the three routes: member 403, a Read token 403 on the two POSTs, a display cookie 403 `display_read_only`.
  - `…::a_late_remote_sees_the_remote_navigation_and_the_checkpoint` — the reviewer's case: navigate to `focus`, select an item, wait until the controls expire (a hub with a test clock), then `GET remote?after=0`: the directive is `focus`, the state has the selection, `complete` is false.
  - `…::a_restart_gives_a_new_incarnation` — a second `Board` on the same database answers another `incarnation` and `seq` 0.
  - `…::revoke_ends_the_relay` — after revoke the POST answers 404 and the socket ends.
  - `…::one_audit_row_for_a_burst` — 5 controls give one `display.remote` row.
  - The existing `exactly_these_methods_are_open_to_a_display` passes unchanged.
- **depends_on:** none.
- **Acceptance:**
  - `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(wall_control) | test(remote_state) | test(ts_drift)'`
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_wall_control) | test(hub_) | test(incarnations) | test(declares) | test(a_socket_declares) | test(exactly_these_methods_are_open_to_a_display) | binary(board_display_foundation) | binary(board_displays_directives)'`
  - the guards.

### A2 (ux): the wall applies frames (≈ 560 lines)

- **Owned files:** `web/src/runtime/decode.ts`, `web/src/runtime/connection.ts`, `web/src/display/DisplayRuntime.tsx`, `web/src/display/useDirective.ts` (`reread`), `web/src/display/api.ts` (only `displayEnvironment` and `withDisplayHeader`: the features header), `web/src/wall/control.ts` (new), `web/src/wall/control.test.ts` (new), `web/src/wall/WallPage.tsx`, `web/src/wall/ChatPane.tsx`, `web/src/wall/Areas.tsx`, `web/src/wall/RacePane.tsx`, `web/src/wall/DisplayArtifactPane.tsx`, `web/src/wall/DiffPane.tsx`, `web/src/wall/displayChanges.tsx`, `web/src/styles/wall.css`, `web/src/wall/wall.test.tsx`, `web/src/display/display.test.tsx`.
- **Changes:** §3.1.5 completely; the display connection declares `artifacts_video.v1 wall_control.v1 wall_pages.v1` in `features=` and in `X-FleetDeck-Features`; `decode.ts` validates `wall_control`; the `controls`, `remote` and `mirror` props of `WallPage` (`mirror` turns off the key listener and the QR codes); `reread()` of `useDirective`; `data-wall-scroll` on the six areas; `data-selected` and its outline; the chat bottom alignment change and the "Scrolled back. New lines are below." line; the long fit of an HTML artifact from `frame_tenths`.
- **Tests:**
  - `control.test.ts`: `stale_generation_does_nothing`; `a_newer_generation_asks_for_a_reread_and_keeps_the_state`; `a_repeated_seq_does_nothing`; `a_new_incarnation_is_accepted`; `sync_sets_the_state_without_a_hold`; `control_and_navigate_set_the_hold`; `only_relative_areas_give_a_scroll_effect`; `shown_remote_is_the_default_for_another_generation`; `step_selection_wraps_both_ways`; `step_selection_without_items_is_null`; `selectable_lists_needs_then_tiles_without_repeats`.
  - `wall.test.tsx`: `a_scroll_control_scrolls_the_named_area` (the element's `scrollTop` changes by 90 % of its height for `page_down`); `the_chat_can_scroll_back_and_shows_the_note`; `a_hold_stops_auto_focus_for_120_s`; `the_selected_tile_has_data_selected`; `mirror_has_no_key_listener` (a keydown on `document` is not stopped); `an_html_artifact_uses_the_long_fit_while_frame_tenths_is_above_0`; `a_long_frame_scrolls_to_the_absolute_position`.
  - `display.test.tsx`: `the_display_socket_declares_wall_control`; `a_wall_control_frame_reaches_the_wall`; `a_navigate_frame_rereads_the_directive_at_once`; `the_sync_frame_restores_the_selection_after_a_reconnect`; `an_unknown_frame_still_reconnects` (unchanged behavior).
- **depends_on:** A1.
- **Acceptance:** `cd web && bun run typecheck && bun run lint`; `cd web && bun run test src/wall src/display src/runtime` (through heavy.sh); `sh tools/design-check.sh`; the guards.

### A3 (ux): the Remote page and the mirror (≈ 640 lines)

- **Owned files:** `web/src/app/routes.ts`, `web/src/App.tsx` (the route switch that renders `WallPage` for the `wall` route today), `web/src/remote/RemotePage.tsx` (new), `web/src/remote/Pad.tsx` (new), `web/src/remote/mirror.ts` (new), `web/src/remote/api.ts` (new), `web/src/remote/remote.test.tsx` (new), `web/src/remote/mirror.test.ts` (new), `web/src/styles/remote.css` (new), `web/src/main.tsx` (the CSS import), `web/src/displays/DisplayRow.tsx`, `web/src/displays/actions.ts`, `web/src/desktop/DesktopPanel.tsx`, `web/src/displays/displays.test.tsx`.
- **Changes:** §3.3 completely: route `#/remote/<id>` (`web/src/App.tsx` has 789 lines and the module limit is 800 for `.rs`, `.ts` and `.tsx`, `tools/module-size.sh:10-11`: add at most one `case` line that renders `RemotePage`; all logic lives in `web/src/remote/`); the mirror (scope narrowing, monitor size, scale, `inert`, the `remote` prop); `padAction` and `routeDirective`; the pad, the keys, one action at a time, the poll after each answer, the rebase and the in-order application of §3.3.3; the status texts; the screenshot button through `capture.ts`; the entry points.
- **Tests:**
  - `routes`: `remote_route_round_trips`.
  - `mirror.test.ts`: `the_first_view_rebases`; `a_new_incarnation_rebases`; `an_incomplete_view_rebases_and_gives_no_effect`; `frames_apply_in_seq_order_above_the_cursor`; `another_devices_earlier_control_is_applied_first` (the mirror's own POST answers `seq` 2; the next view has the frames 1 (another device: `select` X, `scroll` down) and 2 (own: `select` Y, `scroll` up); the effects come in the order 1, 2 and the selection is Y); `state_always_comes_from_the_view`; `the_hold_follows_hold_left_ms`; `route_directive_is_the_inverse_of_directive_route` (every mode); `pad_action_table` (each row of the table of §3.3.2, with the disabled cases).
  - `remote.test.tsx`: `the_pad_posts_each_control_and_navigation`; `the_mirror_applies_nothing_from_an_answer_and_polls_at_once`; `one_action_at_a_time`; `older_wall_shows_the_reload_text_and_button`; `offline_wall_shows_the_text`; `too_many_controls_shows_the_text`; `deck_without_the_feature_shows_no_pad`; `the_mirror_uses_the_display_ceiling_and_the_directive_filter`; `the_scroll_areas_follow_the_mode`; `keys_act_only_inside_the_pad`; `the_screenshot_button_shows_only_for_a_bound_capture_monitor`.
  - `displays.test.tsx`: `the_row_menu_opens_the_remote`.
- **depends_on:** A2.
- **Acceptance:** typecheck, lint; `cd web && bun run test src/remote src/displays src/app` (through heavy.sh); `sh tools/design-check.sh`; gallery shots of the Remote page in both themes, desktop and phone (the skill, §3 step 3); the guards.

### B1 (backend): migration V48, the wall page store and owner routes (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-proto/src/wall_pages.rs` (new), `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/ts.rs`, `crates/fleetdeck-proto/src/version.rs`, `web/src/gen/**` (generated only), `crates/fleetdeck-deck/src/store/wall_pages_schema.rs` (new), `crates/fleetdeck-deck/src/store/wall_pages.rs` (new), `crates/fleetdeck-deck/src/store/wall_pages_tests.rs` (new), `crates/fleetdeck-deck/src/store/mod.rs`, `crates/fleetdeck-deck/src/store/schema.rs`, `crates/fleetdeck-deck/src/http/wall_page_routes.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/mod.rs`, `crates/fleetdeck-deck/tests/board_wall_pages.rs` (new), `crates/fleetdeck-deck/tests/merged_migrations.rs`.
- **Changes:** §3.2.1 types except the `DirectiveBody` variant; §3.2.2 migration; §3.2.3 validation; the three owner routes of §3.2.4 (the delete invalidates `page` rows through a JSON check of `body`, which B2 makes reachable); `feature::WALL_PAGES = "wall_pages.v1"` in `feature::ALL`.
- **Tests:** `migration_v48_keeps_every_directive_row` (a V47 database with an owner, a station and a coordinator row: every column equal after the migration); `migration_v48_accepts_mode_page`; `page_validation_table` (each row of §3.2.3, with the exact text); `http_is_refused_for_every_host_but_loopback` (`example.com`, `192.168.1.20:3000`, `10.0.0.5`, `172.16.0.1`, `100.100.1.1`, `nas.local`, `box.tail1234.ts.net`); `http_is_allowed_for_loopback` (`localhost`, `grafana.localhost`, `127.0.0.1:3000`, `127.8.8.8`, `[::1]:8080`); `https_is_allowed_for_private_and_tailnet_hosts`; `a_board_origin_is_refused`; `the_fifty_first_page_is_refused`; `origin_drops_the_default_port`; `board_wall_pages::add_needs_elevation`; `…::member_and_display_are_refused`; `…::delete_of_an_unknown_page_is_404`; `…::audit_holds_the_host_and_never_the_path`.
- **depends_on:** A1 (shared `http/mod.rs`, `methods_table_display.rs`, `version.rs`).
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(wall_page) | test(ts_drift)'`; `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(migration_v48) | test(page_) | binary(board_wall_pages) | binary(merged_migrations) | test(schema)'`; the guards.

### B2 (backend): the `page` directive, the display page route and the document policy (≈ 480 lines)

- **Owned files:** `crates/fleetdeck-proto/src/display_directive.rs`, `web/src/gen/**` (generated only), `crates/fleetdeck-deck/src/store/display_directives.rs`, `crates/fleetdeck-deck/src/store/display_directives_check.rs`, `crates/fleetdeck-deck/src/store/display_directives_tests.rs`, `crates/fleetdeck-deck/src/http/display_directive_routes.rs`, `crates/fleetdeck-deck/src/http/display_page.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/methods_tests.rs`, `crates/fleetdeck-deck/src/http/mod.rs`, `crates/fleetdeck-deck/src/http/assets.rs`, `crates/fleetdeck-deck/src/http/display_document.rs`, `crates/fleetdeck-deck/src/store/wall_pages.rs` (the delete query, if B1 left a stub), `crates/fleetdeck-deck/tests/board_display_page.rs` (new), `web/src/display/api.ts` (`directiveRoute`: `page` → a `mission` route until B3; one test).
- **Changes:** the module comment of `display_document.rs` (the anonymous answer holds no data; a display login adds its one page origin); the `Page` variant and mode; §3.2.5 rules (the feature projection on the directive read uses `declares(headers, &[], feature::WALL_PAGES)` of A1); `displays.self_page`; §3.2.6 completely: the optional display login in `display_document::document`, the one page origin, `display_csp(frame_origin, page_origin)`, `no-store` with a page origin.
- **Tests:** proto `page_takes_only_its_own_fields`; `page_directive_needs_an_existing_entry`; `only_the_owner_may_store_a_page_directive` (station and coordinator senders → `NotGranted`); `deleted_entry_reads_as_mission`; `board_display_page::self_page_answers_only_for_the_current_directive`; `…::every_miss_is_the_same_404`; `…::an_older_wall_reads_mission_for_a_page_directive` (no features header → mission, same generation); `…::page_delete_resets_the_display`; `…::anonymous_display_document_has_no_page_origin` (a page is stored and shown on a display; `GET /display` without a cookie has the base `frame-src` and `no-cache`); `…::owner_and_member_logins_get_no_page_origin`; `…::a_display_document_has_only_its_own_page_origin` (display A shows page X, display B shows page Y, display C shows mission: A's cookie gets X only, B's cookie Y only, C's cookie none, and each page answer is `no-store`); `…::a_revoked_display_gets_no_page_origin`; `…::a_deleted_entry_gives_no_page_origin`; `…::an_invalid_stored_origin_is_left_out_of_the_header`; unit `display_csp_with_and_without_a_page_origin`; `exactly_these_methods_are_open_to_a_display` lists `displays.self_page`; `next_mode_from_a_page_goes_to_mission`.
- **depends_on:** B1.
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(page) | test(ts_drift)'`; `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_display_page) | test(page_) | test(exactly_these_methods_are_open_to_a_display) | binary(board_displays_directives) | binary(display_directive_station) | binary(coordinator_desktop_wall) | test(display_csp) | binary(board_display_foundation)'`; `cd web && bun run typecheck && bun run test src/display` (through heavy.sh); the guards.

### B3 (ux): Wall pages settings, "Show a page…", the page pane (≈ 560 lines)

- **Owned files:** `web/src/app/routes.ts` (`WallMode` gains `page`; `WallRoute.page?: { id: string }`), `web/src/display/api.ts` (`directiveRoute` for `page`), `web/src/wall/PagePane.tsx` (new), `web/src/wall/WallPage.tsx`, `web/src/wall/model.ts` (`withMode` for `page`), `web/src/wall/page.test.tsx` (new), `web/src/displays/WallPages.tsx` (new), `web/src/displays/ShowPage.tsx` (new), `web/src/displays/DisplaysTab.tsx`, `web/src/displays/DisplayRow.tsx`, `web/src/displays/actions.ts`, `web/src/displays/displays.test.tsx`, `web/src/remote/Pad.tsx` (the "Page" area), `web/src/remote/mirror.ts` (the page card count; `routeDirective` for `page`), `web/src/styles/wall.css`, `web/src/styles/displays.css`, `web/e2e-real/wall-pages.spec.ts` (new), `web/e2e-real/pages.ts` (new: the local page servers of the browser test).
- **Changes:** §3.2.7 and §3.2.8 completely, with the reload of §3.2.6 (`page_gen`); the owner wall and the mirror show the page card; the browser test below.
- **Tests:** `page.test.tsx`: `credentialless_is_set_before_src`; `no_credentialless_support_shows_the_text`; `a_same_origin_url_is_refused`; `the_frame_has_the_exact_sandbox_and_allow`; `a_page_directive_without_page_gen_reloads_once` (`location.replace` with `/display?page_gen=<generation>` and the hash; no frame before it); `with_page_gen_of_the_generation_there_is_no_reload`; `a_frame_src_violation_shows_the_text_and_does_not_reload`; `long_fit_follows_frame_tenths`; `the_owner_wall_shows_a_card_and_no_frame`; `the_card_counts_screen_n_of_8_from_frame_tenths`.
  - Browser test `web/e2e-real/wall-pages.spec.ts` (real deck and the production bundle, the pattern of `web/e2e-real/display.spec.ts`; the wall has its own browser context, as a kiosk profile). `pages.ts` starts two small servers on `127.0.0.1` with free ports: server P (`https`, a self-signed certificate, and `http` on a second port) and server N (`https`, another port). Each server records the `Cookie` header of every request. P's page embeds a full-size frame of N's page. N's page runs a script that calls `window.open('about:blank')` and sets `top.location.href = 'https://example.invalid/'` in a `try`, and reports the results to N by a `fetch` of `/report?open=<null|window>&top=<blocked|done>`. The owner adds the pages through the board form (the "Confirm it is you" dialog, as `display.spec.ts` does):
    1. `an_https_page_shows_in_the_wall_frame` — show P (`https`) on the wall; the frame shows P's text; the document URL of the wall has `page_gen`.
    2. `an_http_loopback_page_shows_in_the_wall_frame` — show P on its `http` port; the frame shows P's text.
    3. `the_deck_refuses_http_for_a_private_tailnet_or_local_host` — the form shows the §3.2.3 text for `http://192.168.1.20:3000`, `http://100.100.1.1/` and `http://nas.local/`.
    4. `the_page_and_its_nested_frame_get_no_deck_cookie` — P and N (other ports of the deck's host `127.0.0.1`) never receive `fd_display` or `fd_session`.
    5. `a_nested_frame_of_another_origin_cannot_open_a_window_or_move_the_wall` — N reports `open=null` and `top=blocked`, and the wall page URL is still `/display`.
    6. `the_anonymous_display_document_has_no_page_origin` — a request to `/display` without cookies (Playwright `request`) has no origin of P or N in its `Content-Security-Policy`. `displays.test.tsx`: `add_page_asks_for_elevation`; `the_deck_error_text_shows_under_the_field`; `delete_confirms_with_the_exact_text`; `show_a_page_sends_a_page_directive`; `no_pages_shows_the_empty_text`. `routes`: `page_mode_round_trips`; `page_mode_without_a_page_is_mission`.
- **depends_on:** B2, A3.
- **Acceptance:** typecheck, lint; `cd web && bun run test src/wall src/displays src/remote src/display src/app` (through heavy.sh); `cd web && bun run build && bun run e2e:real wall-pages.spec.ts` (through heavy.sh; every case passes); `sh tools/design-check.sh`; gallery shots of Settings › Displays and a wall in page mode (both themes, desktop and phone); a manual check on an Omarchy wall (a kiosk that loads the deck by its tailnet address): one `https` page and one `http://localhost` page, with the result in the pull request (§5.2 item 4a); the guards.

### C1 (backend, after B15): wall apps on the deck (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-proto/src/display_directive.rs` (`app` field), `crates/fleetdeck-proto/src/wall_apps.rs` (new), `crates/fleetdeck-proto/src/coordinator_desktop.rs` (the `app` argument of `wall_show`), `crates/fleetdeck-proto/src/version.rs`, `web/src/gen/**` (generated only), the next free migration file `crates/fleetdeck-deck/src/store/display_apps_schema.rs` (new), `crates/fleetdeck-deck/src/store/schema.rs`, `crates/fleetdeck-deck/src/store/displays.rs`, `crates/fleetdeck-deck/src/http/display_routes.rs` (setting + elevation), `crates/fleetdeck-deck/src/http/display_artifact.rs`, `crates/fleetdeck-deck/src/http/display_app_feed.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_display.rs`, `crates/fleetdeck-deck/src/http/methods_tests.rs`, `crates/fleetdeck-deck/src/coordinator_desktop/wall.rs`, `crates/fleetdeck-deck/tests/board_wall_apps.rs` (new).
- **Changes:** §3.4.2 and §3.4.3; `feature::WALL_APPS = "wall_apps.v1"`; the route reads the declaration with `declares(headers, query, feature::WALL_APPS)` (the header or `?features=`).
- **Tests:** `app_policy_only_with_directive_flag_setting_html_and_feature` (a table over the four conditions; only all four give `APP_CSP`); `the_query_declares_wall_apps_without_the_header` (a request with `?features=wall_apps.v1` and no `X-FleetDeck-Features` header gets `APP_CSP` when the other three hold); `the_query_alone_grants_nothing` (the query with `allow_apps` off, or without `app: true`, gets `INERT_CSP`); `both_answers_are_no_store`; `allow_apps_needs_elevation_to_turn_on`; `allow_apps_change_resets_the_directive`; `feed_holds_only_the_author_lineage_inside_the_scope`; `feed_is_404_without_an_app_directive`; `feed_never_holds_tool_input_or_mail` (sentinel test, as §5B item 1 of the parent); `wall_show_app_maps_to_the_directive_flag`.
- **depends_on:** B2, and the B15 deck task that adds `APP_CSP`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_wall_apps) | binary(board_display_artifact) | binary(coordinator_desktop_wall) | test(exactly_these_methods_are_open_to_a_display)'`; `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(ts_drift)'`; the guards.

### C2 (ux, after B15): wall apps on the board (≈ 300 lines)

- **Owned files:** `web/src/wall/DisplayArtifactPane.tsx`, `web/src/wall/appFeed.ts` (new), `web/src/wall/apps.test.tsx` (new), `web/src/displays/DisplayRow.tsx`, `web/src/wall/SendToWall.tsx`, `web/src/display/api.ts` (`wall_apps.v1` in the features header; `appArtifactPath`), `web/src/displays/displays.test.tsx`, `web/e2e-real/wall-apps.spec.ts` (new).
- **Changes:** the app branch of the pane with the B15 `AppFrame` (`role: 'wall'`) and the 5 s feed; the probe and `AppFrame.src` use the same URL `appArtifactPath(...) = artifactPath(...) + '?features=wall_apps.v1'`; the "Allow apps on this wall" switch with elevation; the "Run it as an app" checkbox; the browser test.
- **Tests:** `an_app_directive_renders_appframe_with_role_wall`; `the_app_frame_src_declares_wall_apps_in_the_query`; `the_probe_uses_the_same_url_as_the_frame`; `the_feed_is_polled_and_passed_as_data`; `without_app_the_frame_stays_inert`; `the_switch_asks_for_elevation`; `run_it_as_an_app_shows_only_for_a_single_html_file_and_an_allowing_display`.
  - Browser test `web/e2e-real/wall-apps.spec.ts` (real deck, production bundle, a wall context; the spec publishes the artifact with the publish helper that the B15 browser test uses; C2 waits for that helper, the same as for `AppFrame`): an HTML file whose script writes "app ran" into its body and sends `ui/initialize`. 1. `an_app_runs_and_initializes_in_the_wall_frame` — with `allow_apps` on and `app: true`, the frame body shows "app ran", and the wall host answered `ui/initialize` (the app writes "initialized" after the answer). 2. `without_allow_apps_the_same_file_stays_inert` — the frame body has no "app ran".
- **depends_on:** C1, the B15 task that adds `AppFrame`, and the B15 browser-test publish helper.
- **Acceptance:** typecheck, lint; `cd web && bun run test src/wall src/displays src/display` (through heavy.sh); `cd web && bun run build && bun run e2e:real wall-apps.spec.ts` (through heavy.sh); `sh tools/design-check.sh`; the guards.

---

## 7. Decisions made in this draft

1. **Navigation from the remote is an owner directive; select and scroll are a transient relay with a deck checkpoint** (revision 2). Rejected: new directive kinds (`scroll`, `select`, …) stored in `display_directives` (a migration, a 2 s delay, an audit row for each key); navigation as a transient control (revision 1: a remote that comes late or after a gap cannot know the view, finding 5).
2. **Delivery over the display WebSocket, only to sockets that declare `wall_control.v1`.** Rejected: piggyback on the 2 s directive poll (too slow for a remote); push to every socket (old clients reconnect on an unknown frame, fact 19).
3. **A control is bound to the directive generation.** Rejected: unbound controls (a late control could act on the next view).
4. **Owner-only senders for controls and pages.** Rejected: coordinators, members, the station hotkeys, the display itself.
5. **The remote is a board-side mirror plus an on-demand P5a screenshot.** Rejected: P6 screenshots or a stream with P7 input into the wall window (P7 refuses wall windows; owner input is a new command path; 867 KB stills; high cost).
6. **The mirror computes from owner data and the deck state (directive and checkpoint); the wall reports nothing.** Rejected: a display→deck view report (it would break "a display may only subscribe", fact 20).
7. **Scroll in discrete steps (line = 10 %, page = 90 %, top, bottom) on named areas.** Rejected: pixel offsets from the remote (the remote does not know the wall's layout).
8. **A remote hold of 120 s stops auto-focus and ambient after each control.** Rejected: no hold (auto-focus takes the screen during a scroll); a pin (persistent and stored).
9. **Pages are an owner allow-list referenced by id.** Rejected: a free `url` in a directive; "local network only" (it excludes public docs and dashboards).
10. **The page frame is `credentialless` and refused without support.** Rejected: a normal frame (the display cookie reaches other ports of the deck host); a separate preview window as the main path (no remote scroll; kept as the fallback "Open a link").
11. **The allow-list chooses the site of the pane's own frame; it is not a content boundary** (revision 2). The display document's `frame-src` holds the one page origin of the display's current directive and keeps the pane's frame on it. An allowed page may embed any origin; the protections of the display login, the kiosk profile and the wall document hold for nested frames through credentialless, the sandbox flags and the permissions policy. Rejected: URL-level matching in JS (cannot see navigations inside a cross-origin frame); CSP Embedded Enforcement (the `csp` frame attribute: a third-party site must opt in with `Allow-CSP-From`, so almost every real page would not load).
12. **`http` only for loopback hosts** (revision 2). Rejected: any `http` host; `http` for private, tailnet, `.local` and `.ts.net` hosts (revision 1: the `https` wall document blocks them as mixed content, finding 4); a deck proxy for `http` pages (it would make the deck a fetcher of third-party content).
13. **A long fit (a frame 8 screens tall) gives scroll for pages and HTML artifacts.** Rejected: scripts injected into the frame (breaks the inert rule); no scroll.
14. **One deck-wide page list.** Rejected: one list for each display (owner question Q1).
15. **Adding a page needs elevation; deleting does not.** Rejected: no elevation (the list decides what a kiosk loads).
16. **An older wall reads a `page` directive as `mission`, through `X-FleetDeck-Features`.** Rejected: send `page` to every wall (an old bundle cannot draw it).
17. **Wall apps reuse the B15 host as role `wall`, off by default for each display, with a feed of the author's lineage and no app actions.** Rejected: a separate wall bridge; a feed of the whole display scope (an app can leak through WebRTC).
18. **Board feature tokens join `feature::ALL`**, the same as `artifacts_video.v1`. Rejected: a separate board feature list (a second discovery route).
19. **The audit writes one `display.remote` row for each burst of controls.** Rejected: one row for each control.
20. **The page origin reaches only the display that shows the page, through its own login, in the `/display` document; the wall reloads its document once for each page generation** (revision 2). Rejected: every saved origin in the public document (finding 1); a deck-made wrapper document for each page (it puts a URL into server-made HTML and adds a framed same-origin document).
21. **A wall app declares `wall_apps.v1` in the query of its frame URL** (revision 2). Rejected: the header only (a frame navigation cannot carry it, finding 2); no declaration (an older bundle and a new bundle then get the same answer, and the deck cannot tell them apart).
22. **The mirror applies only polled frames, in `seq` order, and rebases on a first view, a new incarnation or a gap** (revision 2). Rejected: applying a control from its own answer (finding 6: another device's earlier control would then apply after it).
23. **`select` carries the absolute item, checked against the display scope.** Rejected: a relative `select { dir }` (its result depends on the order and on the item list of each receiver, so the deck cannot keep a checkpoint of it).

---

## 8. Owner questions (taste or policy)

| Id | Question | Recommended default |
| --- | --- | --- |
| Q1 | One list of wall pages for the whole deck, or one list for each display? | One list for the deck. |
| Q2 | Which hosts may use plain `http`? | Revision 2: loopback only (`localhost`, `*.localhost`, `127.0.0.0/8`, `[::1]`). The browser blocks every other `http` page inside the `https` wall. |
| Q3 | May a coordinator (an agent with the `desktop` class) show a wall page or send controls? | No, not in this track. |
| Q4 | How long does the wall wait after a remote control before auto-focus or ambient can take the screen? | 120 seconds. |
| Q5 | Add Omarchy hotkeys for the remote (for example `Super+Alt+J/K` items, `Super+Alt+Return` open, `Super+Alt+Escape` back)? | Not in this track. |
| Q6 | May the owner widen the feed of a wall app to the whole display scope? | No: the author's lineage only. |

---

## Decision log entries (to append)

## DL-470: B17 wall controls are a transient owner relay (architect, 2026-10-23)

- Context: the owner cannot scroll the wall or move between its items from the board or the phone (B17 want 1). Directives are durable "what the wall shows" state, and the wall keys act only at the kiosk.
- Decision: navigation from the remote ("Open", "Back", the next or previous view) is an owner directive (`POST /api/displays/{id}/remote/navigate`, the checks and audit of the directive route), followed by a `navigate` frame so that the wall reads it at once. The transient steps are a closed control enum (`scroll { area, step }`, `select { item }`); the only reference is the selected session, checked against the display's effective scope. The owner sends a control with `POST /api/displays/{id}/control` (Drive, owner only). The deck keeps the controls in memory (32 for each display, 30 s, 10 per second), keeps a checkpoint of their result for the current directive generation (the selection and the long-frame position), and pushes each frame (with a relay `incarnation`, its `seq`, its generation and the checkpoint after it) only to display sockets that declare `wall_control.v1`. A socket gets a `sync` frame when it attaches and after a lag. The wall applies a frame like its keys and holds auto-focus and ambient for 120 s.
- Rejected: new directive kinds for scroll and select; navigation as a transient control; delivery by the 2 s directive poll; coordinator, member, station or display senders.

## DL-471: B17 remote control is a board-side mirror (architect, 2026-10-23)

- Context: B17 want 3 asks for a remote desktop for the wall browser only.
- Decision: the board route `#/remote/<display_id>` renders `WallPage` from the owner's data, narrowed to the display's effective scope, at the bound monitor's logical size, with the display's directive and checkpoint (`GET /api/displays/{id}/remote`). The mirror applies only polled frames, in `seq` order; it rebases on its first view, on a new relay incarnation and on a gap. "Take a screenshot of the wall" reuses the P5a owner screenshot of the bound monitor. No pixels stream and no input reaches the wall.
- Rejected: P6 screenshots or a stream with P7 input into the wall window (P7 refuses wall windows on purpose; one dev1 still was 867 KB, above the 700 KiB MCP limit); a display→deck view report.

## DL-472: B17 wall pages are an owner allow-list in a credentialless frame (architect, 2026-10-23)

- Context: B17 want 2 asks for custom URLs on the wall. The kiosk holds the read-only display login.
- Decision: the owner keeps at most 50 "Wall pages" (name, URL, fit) on the deck, added with elevation. A `page` directive names an entry by id. The wall shows it in an `<iframe credentialless sandbox="allow-scripts allow-same-origin allow-forms" allow="" referrerpolicy="no-referrer">`, and refuses to show it without `credentialless` support or for a board origin. Only a `/display` request with the display's own login gets the origin of the page of its current directive in `frame-src`; the anonymous document holds no configuration data, and the wall reloads its document once for each page generation. The `frame-src` keeps the pane's own frame on that origin; it does not limit what the page embeds, so the owner adds only trusted sites. `https` anywhere; `http` only for loopback hosts (the `https` wall blocks other `http` frames as mixed content). Migration V48 adds `wall_pages` and rebuilds `display_directives` with the mode `page`. An older wall reads a `page` directive as `mission`.
- Rejected: a free URL in a directive; a normal frame; a separate window as the main path (it stays as Desktop › Open a link).

## DL-473: B17 scroll model (architect, 2026-10-23)

- Context: nothing on the wall can scroll from a key or a directive; the focus chat clips older lines; an opaque artifact frame cannot be scrolled from outside.
- Decision: named scroll areas (`needs`, `stream`, `chat`, `changes`, `artifact`, `page`) and discrete steps (10 % line, 90 % page, top, bottom). The focus chat becomes scrollable with a "Scrolled back. New lines are below." note. Pages with the long fit, and HTML artifacts while the position is above 0, use a frame 8 screens tall inside a scrolled container. The deck counts the position of such a frame in tenths of a screen (0 to 70) in its checkpoint, and the wall scrolls to that absolute position.
- Rejected: pixel offsets from the remote; scripts in the inert artifact frame.

## DL-474: B17 wall apps share the B15 app host (architect, 2026-10-23)

- Context: B17 want 4 asks for agent-made live UIs on the wall; B15 designs MCP Apps in the chat.
- Decision: phase C, after the B15 host. An `artifact` directive with `app: true` on a display with "Allow apps on this wall" (off by default, elevation to turn on) is served with the B15 app policy (scripts, `connect-src 'none'`) and rendered in the B15 `AppFrame` with the role `wall`. The frame URL declares `wall_apps.v1` in its query, because a frame navigation cannot carry a header; the declaration grants nothing. The only app request on a wall is `ui/initialize`. The wall sends the feed `notifications/fleetdeck/wall-data` with the cards of the author session and its spawn lineage inside the display scope. Coordinator `wall_show` gets an optional `app` flag.
- Rejected: a separate wall bridge; a feed of the whole display scope; app actions on the wall.

## DL-475: B17 backlog corrections (architect, 2026-10-23)

- The directive is a tagged enum with typed references, not `{ mode, artifact_url, … }`. The station sender uses `desktop_shell.v1` and two intents. A display reads artifacts through the inert display artifact route, not `/a/{token}/`, so scripts never run on a wall. Desktop › Open a link already opens any web page in its own window on a desktop monitor. P7 refuses wall windows. The wall cannot scroll at all today. No owner route reads a display's current directive. Side finding for the MCP track: `desktop_info` refuses a grant station given by name and accepts its id.

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. One list of wall pages for the whole deck, or one list per display? — **The recommended default: One list for the deck**
2. Which hosts may use plain http for a wall page? — **The recommended default: Loopback, private networks, the tailnet range, .local and .ts.net only** — *Revision 2 narrows this to loopback only, for a technical reason: the wall document is https, and the browser blocks every other http frame as mixed content (finding 4). The owner should know this change; an https address (for example `tailscale serve`) covers the tailnet case.*
3. May a coordinator (desktop class) show a wall page or send wall controls? — **The recommended default: No, not in this track**
4. How long after a remote control may auto-focus or ambient take the screen again? — **The recommended default: 120 seconds**
5. Add Omarchy hotkeys for the remote (items, open, back)? — **The recommended default: Not in this track**
6. May the owner widen the feed of a wall app to the whole display scope? — **The recommended default: No, the author's lineage only**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.

---

## Revision 2 (design review round 1)

Review: `~/fleetdeck-briefs/backlog/wall-control/adv-design-r1.md` (adversary, verdict FAIL, reviewed commit `00055138`). I checked each finding against the code before the change. All six findings are confirmed and fixed. No task id changed; A1, A2, A3, B1, B2, B3, C1 and C2 have new tests and owned files.

| # | Finding | Verified in the code | What changed |
| --- | --- | --- | --- |
| 1 | BLOCKER: the public `/display` document lists every saved page origin | `/display` is outside the guard (`crates/fleetdeck-deck/src/http/mod.rs:661-670`); `display_document.rs:10-11` says the document holds no data; revision 1 put every `wall_pages` origin into its CSP | §3.2.6 rewritten: an anonymous request (and an owner, member or other-display login) gets the base policy only. Only a request with the display's own `fd_display` login gets the one origin of the page of its current directive, with `no-store`. The wall reloads its document once for each page generation (`page_gen`). §3.2.7, §4, §5.2 item 3a, decision 20, DL-472. Tests: B2 `anonymous_display_document_has_no_page_origin`, `owner_and_member_logins_get_no_page_origin`, `a_display_document_has_only_its_own_page_origin`, `a_revoked_display_gets_no_page_origin`, `a_deleted_entry_gives_no_page_origin`; B3 `page.test.tsx` reload tests and browser case 6. |
| 2 | BLOCKER: a wall app frame cannot declare `wall_apps.v1` in a header | `DisplayArtifactPane.tsx:130-163`: the probe fetch has a header, the `<iframe src>` load has none | §3.4.2: the route reads the declaration from the header or the query `features=` (helper `declares`, A1, in `artifact_projection.rs`); the pane uses `artifactPath(...) + '?features=wall_apps.v1'` for the probe and for `AppFrame.src`. Every live check stays; the query alone grants nothing (§5.3 item 5). Tests: C1 `the_query_declares_wall_apps_without_the_header`, `the_query_alone_grants_nothing`; C2 unit tests and the browser test `wall-apps.spec.ts` (the app runs and gets its `ui/initialize` answer; inert without `allow_apps`). |
| 3 | BLOCKER: the parent `frame-src` does not limit what an allowed page embeds | Correct by the CSP rules: `frame-src` of the display document checks the navigations of its own child frame only; the page's own policy governs its nested frames | I revised the security contract (the second fix that the review offers). The allow-list chooses the site of the pane's frame and is not a content boundary; the owner adds only trusted sites. The protections that matter hold for nested frames too: credentialless, the sandbox flags and the empty permissions policy pass to descendants. §2.2 items 6 and 7, §3.2.8 settings text, §4, §5.2 items 2 and 3, decision 11 (CSP Embedded Enforcement rejected, with the reason), DL-472. Browser tests B3 4 and 5: a nested frame of another origin gets no deck cookie, cannot open a window and cannot move the wall. |
| 4 | MAJOR: `http` pages on private, tailnet and `.local` hosts are blocked as mixed content | The deck serves `https` only (`guard.rs:112`, every board origin is `https`) | §3.2.3: `http` only for loopback hosts (`localhost`, `*.localhost`, `127.0.0.0/8`, `[::1]`), with a new refusal text; a settings note. §2.2, §4, §5.2 item 4a (the Local Network Access residual), decision 12, Q2, DL-472, and a note under the architect's answer 2. Tests: B1 `http_is_refused_for_every_host_but_loopback`, `http_is_allowed_for_loopback`, `https_is_allowed_for_private_and_tailnet_hosts`; B3 browser cases 1 to 3 and a manual check on an Omarchy wall. |
| 5 | MAJOR: the mirror has no recoverable baseline | `useDirective.ts:21-25` applies each generation once; revision 1 kept navigation effects only in wall-local state and returned no resulting state | Navigation (open, back, views) is now an owner directive through `POST /api/displays/{id}/remote/navigate` (the checks and audit of the directive route), with a `navigate` frame so the wall reads it at once. `select` carries the absolute item. The deck keeps a checkpoint (`WallRemoteState`: selection and long-frame position) for the current generation, without expiry, and puts it into every frame and into `WallRemoteView`. Every frame and view carries the relay `incarnation`. A wall gets a `sync` frame on attach and after a lag; a mirror rebases on its first view, a new incarnation and `complete: false`. §2.1, §3.1.1 to §3.1.5, §3.3.2, §3.3.3, §3.5, §4, §5.1 item 6, decisions 1, 6 and 23, DL-470, DL-471. Tests: A1 `a_late_remote_sees_the_remote_navigation_and_the_checkpoint` (the review's case), `a_restart_gives_a_new_incarnation`, the hub checkpoint and `complete` tests; A2 `sync`, `reread` and generation tests; A3 rebase tests. |
| 6 | MAJOR: applying an acknowledged control at once breaks the global order | Revision 1 §3.3.3 applied the own control from the answer and skipped it in the poll | §3.3.3: the mirror applies nothing from an answer; it polls at once after each answer and applies frames only from polls, in `seq` order. The state comes from the deck's checkpoint, which the deck computes in `seq` order under the slot lock. §4, decision 22. Test: A3 `another_devices_earlier_control_is_applied_first` (the review's case), `the_mirror_applies_nothing_from_an_answer_and_polls_at_once`. |

**Findings not fixed:** none.

**Notes for the architect:**

- Finding 4 changes the architect's answer to Q2 (from "loopback, private networks, the tailnet range, `.local` and `.ts.net`" to "loopback only"). The reason is technical, not taste: the browser blocks the other `http` frames in the `https` wall, so the old answer gave entries that never load. Tell the owner.
- Remaining approximation (stated in §2.3): the relative scroll positions of the needs list, the activity stream, the chat, the change pane and a Markdown or image artifact are not in the checkpoint. After a rebase, the mirror shows them at their default position. The view, the selection and the long-frame position are exact.
- Residual (§5.2 item 4a): Chromium's Local Network Access rules can block an `http://localhost` page when the kiosk loads the deck by a non-loopback address. The B3 manual check records the result.

---

## Build answers

Pre-PR review `~/fleetdeck-briefs/backlog/wall-control/adv-prepr-final.md` (commit `955b66e0`). Each line is a build decision that the sections above now state.

1. **The mirror shows an artifact as a card** (BLOCKER). The owner's `#/wall` reads an `/a/` view link and shows it in `ArtifactFrame`, whose policy allows scripts; a wall shows the same file inert (fact 12). In artifact mode the mirror (`mirror` prop of `WallPage`) renders `ArtifactCard` (the id and the version, and the line "The wall shows this artifact. The remote does not open it.") before any view link is asked for. The rule "a mirror never loads a frame" (§3.3) now covers artifacts too. Tests: `page.test.tsx` `the_mirror_shows_an_artifact_as_a_card_and_asks_for_no_view_link`; browser `web/e2e/remote.spec.ts`.
2. **No older generation moves the relay back** (MAJOR). `push` refuses a generation below the slot's generation with `PushError::Stale` and records nothing (no `seq`, no rate entry, no frame). The control route and the navigate route take `hub.turn(id)`, one async lock for each display, from the read of the generation (control: `effective_directive` and the `select` check; navigate: `owner_set`) to the push; the audit runs after the turn. The control route answers a `Stale` push with `409 wall_changed`; the navigate route ignores it as it ignores `TooMany`. Tests: `wall_control_tests.rs` `hub_refuses_an_older_generation_and_keeps_the_newer_checkpoint`, `the_turn_of_a_display_serializes_its_requests_only`.
3. **The first `sync` reaches a wall that listens late** (MAJOR). The display socket sends its `sync` frame at once, possibly before `WallPage` subscribes. `Connection` keeps the newest `wall_control` frame (a frame of another incarnation replaces it; in the same incarnation only a frame with the same or a higher `seq` does) and calls a new listener with it at once as a `sync` frame (no event: no hold and no second scroll). Tests: `display.test.tsx` `a_sync_before_the_wall_listens_is_replayed_as_sync`, `the_kept_frame_never_moves_back_in_one_incarnation`.
4. **The Activity stream is one column** (MAJOR). `.fd-wall-stream` was `flex-flow: column wrap`: the older rows went into extra columns outside the width, where `scrollTop` cannot reach them. It is now `flex-direction: column` with no wrap; the older rows overflow at the bottom of the `overflow: hidden` list, and a remote scroll reaches them. Test: browser `web/e2e/wall.spec.ts` `a remote scroll reaches the oldest Activity row: one column, no wrap (B17)`.
