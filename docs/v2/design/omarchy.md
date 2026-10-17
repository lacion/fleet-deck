# Fleet Deck on Omarchy: the wall, desktop control and the desktop MCP (R13)

- **Status:** §4.5, §5 and §6.1-§6.4/§6.6 **accepted**, revision 5 (2026-10-16, DL-201: design review round 4 PASS; its two MAJOR findings fixed in §5C items 4 and 5); revision 6: §5D (P2b) and §6D (P3b) drafted from the open questions, owner decisions DL-221, DL-222, DL-223; DESIGN REVIEW pending; revision 7: §5E answers the DESIGN REVIEW round 1 of revision 6; revision 8: §5F answers the DESIGN RE-REVIEW round 2; revision 9: §5G answers the round 3 findings; **§5D/§6D with §5E-§5G accepted by the owner for the P2b/P3b build (DL-230 item 6, 2026-10-18)**; **P2b/P3b (§5D, §6D with §5E-§5G) are built on v2 (`cccaf3d0..5a01c9ca`, DL-231): PRE-PR round 1 FAIL fixed, round 2 PASS (§5H)**; revision 10: §6.5A (P5 capture) and §9A (P8 provisioning) answer the P5/P8 DESIGN REVIEW round 1 with the owner decisions DL-230 items 1-4, DESIGN REVIEW pending; revision 11: P5 split into P5a (owner screenshots, artifact deletion; `omarchy-p5a.md`) and P5b (recordings, later); P8 contracts in `omarchy-p8.md`; DESIGN REVIEW round 3 (last) pending; other parts still draft. §5C answers the round 3 review (`/tmp/adv-r13-p23-r3-report.md`, owner decision: apply the fixes and one more round) and overrides §5B where they differ; §5B answers the round 2 review (`/tmp/adv-r13-p23-r2-report.md`) and overrides §5A where they differ; §5A answers the DESIGN REVIEW round 1 of §4.5, §5 and §6 (`/tmp/adv-r13-p2-design-report.md`) and overrides them where they differ. Not frozen. Each high-risk part needs an adversary DESIGN REVIEW before its code (§13).
- **Owner request (2026-10-16):** "i have a server thats connected to a ultrawide screen, i use this server to run multiple ai agents as it has 64 cores and 128gb of ram … it would be awesome if i could control omarchy and the screen trough the fleet and maybe also have an mcp … imagine using the screen as a command control center and we could display and control everhting there." Then: "lets create a document for this work so we can implement everything".
- **Owner answers (DL-198):**

| Question | Answer |
| --- | --- |
| Screen | 3440×1440 (21:9), "but we should probably be flexible" |
| Approvals on the wall | No. The wall is read-only. |
| Screenshots by agents (Computer Use) | Acceptable. "lets remove the rule for no computer use" (DL-197) |
| Agents in a separate OS user | Not now. Agents run as the owner's user. |
| Omarchy on other machines | Yes, on the laptop too. |

- **Decisions:** DL-197 (Computer Use is allowed), DL-198 (this design, the owner answers).
- **Related:** R6.2 Watch page (DL-169), R11 shared fleet (DL-154), R12 artifacts (DL-155), ADR-006 and ADR-008 (coordinator MCP), DL-172 (elevation), DL-193 (station resources), `design/service.md`.

---

## 1. Facts

### 1.1 Omarchy

Sources:
- The manual at <https://omarchy.org/manual/>, all 51 pages, read on 2026-10-16.
- The repository `github.com/omacom/omarchy`, branch `quattro`, commit `51bfd1d` (2026-10-15), version `4.0.0.alpha`.

Omarchy can change between versions. Each item below names its source, so that a phase brief can check it again.

| Area | Fact | Source |
| --- | --- | --- |
| Base | Arch Linux, Hyprland, and one long-lived Quickshell process `omarchy-shell` that draws the bar, menu, notifications, OSD, lock screen and panels | manual "Welcome", `docs/omarchy-shell.md` |
| Paths | `OMARCHY_PATH` is the package tree (`/usr/share/omarchy`; updates overwrite it). User files are in `~/.config/omarchy/`, and state is in `~/.local/state/omarchy/` | manual "Dotfiles" |
| Hyprland config | Lua files: `~/.config/hypr/{hyprland,bindings,monitors,input,looknfeel,autostart}.lua`, with `hl.monitor`, `o.bind`, `o.window` and `o.launch_on_start` | manual "Dotfiles", "Monitors" |
| Shell IPC | `omarchy-shell <target> <method> …`. `shell` methods: `ping`, `summon/hide/toggle <id> <json>`, `call <id> <method> <arg>`, `rescanPlugins`, `reloadConfig`, `applyTheme`, `enablePlugin`, `putBarWidget`, `listPlugins`. The answer is `ok` on stdout, with exit 0. `OMARCHY_SHELL_IPC_TIMEOUT` limits the wait | `docs/omarchy-shell.md` |
| Plugins | `~/.config/omarchy/plugins/<id>/manifest.json` (`schemaVersion: 1`). Kinds: `bar-widget`, `panel`, `overlay`, `menu`, `service`, `bar`. An id is dotted (example `my.org.cool-clock`), and `omarchy.*` is reserved. A plugin hot-reloads on save | manual "Shell Plugins", `docs/omarchy-shell.md` |
| Plugin power | Third-party plugins get capability-scoped facades, but "facades are API boundaries, not same-process QML sandboxes". A plugin has the user's file and process access. Built-in services use Quickshell `Process` | `docs/omarchy-shell.md`, `shell/plugins/panels/tailscale/Service.qml` |
| Plugin CLI | `omarchy plugin list [--json] / add <git-url> / enable <id> [--section …] / disable / remove / validate <dir>` | manual "Shell Plugins" |
| Bar modules | Inline modules in `shell.json`: `"type": "command"` (a script prints text or Waybar-style JSON), and `"type": "qml"` | `docs/omarchy-shell.md` |
| Menu | Extra rows go in `~/.config/omarchy/extensions/omarchy-menu.jsonc` (dotted id → `{icon, label, action}`). `omarchy menu summon <dotted.id>` opens a node of the menu | manual "Dotfiles", "Omarchy CLI" |
| Hooks | `omarchy-hook <name>` runs `~/.config/omarchy/hooks/<name>` and every file in `<name>.d/` with `bash` (it skips `*.sample`). Events: `post-boot`, `post-update`, `pre-refresh-pacman`, `theme-set` ($1 theme), `font-set`, `battery-low` | `bin/omarchy-hook`, manual "Dotfiles" |
| Themes | `colors.toml` per theme. Templates `*.tpl` render into `~/.local/state/omarchy/current/theme/` on every theme switch, and a user template in `~/.config/omarchy/themed/` overrides a built-in one. Built-in templates exist for apps such as `t3code.json.tpl` and `claude.json.tpl`. `bin/omarchy-theme-set-t3code` copies the result atomically into T3 Code's theme folder | `default/themed/`, `bin/omarchy-theme-set-t3code`, manual "Making your own theme" |
| Notifications | The shell runs a freedesktop `NotificationServer`. `omarchy-notification-send [-u low\|normal\|critical] [-g glyph] [-t ms] [-r id] [-p] <headline> [description] [--exec <program> …]` sends one, and `--exec` runs on a click. Under DND, critical notifications still show | `shell/plugins/notifications/Service.qml:980`, `bin/omarchy-notification-send`, manual "Notices" |
| Web apps | `omarchy-launch-webapp <url>` runs the default Chromium-family browser with `--app=<url>` through `uwsm-app`. `omarchy-launch-or-focus-webapp <pattern> <url>` focuses a running one | `bin/omarchy-launch-webapp` |
| Toggles | `omarchy toggle <idle\|screensaver\|notification silencing\|bar\|…>`. State is in flag files in `~/.local/state/omarchy/toggles/` | manual "Toggles" |
| Screensaver | ASCII art from `~/.config/omarchy/branding/screensaver.txt`, one copy on each monitor. Idle times are in `shell.json` `idle.screensaver` and `idle.lock` | manual "Toggles", "Branding" |
| Capture | `omarchy capture screenshot [smart\|region\|windows\|fullscreen] [copy\|save]`, `omarchy capture screenrecording [--fullscreen …] [--stop-recording]` (gpu-screen-recorder) | manual "Screenshots & Recording" |
| AI | Agent CLIs are mise stubs in `~/.local/bin`. `omarchy default agent` accepts a fixed list (`bin/omarchy-default-agent`) that does not include Fleet Deck. `omarchy agent prompt "…"` runs the default agent **unattended and auto-approved**. An Omarchy skill is linked into `~/.claude/skills` and `~/.codex/skills` | manual "AI", `bin/omarchy-default-agent` |
| Accounts | `omarchy agent account add/use/list/mode auto`. The registry is `~/.local/state/omarchy/agents/accounts/<claude\|codex\|grok>.json`, and an account home becomes `CLAUDE_CONFIG_DIR` or `CODEX_HOME` | `bin/omarchy-agent-account-home` |
| Usage panel | `omarchy.agents` reads JSON records in `~/.local/state/omarchy/agents/usage/` and "picks up any record that lands in the directory regardless of who wrote it". It can merge snapshots of other machines through a synced folder | `shell/plugins/agents/README.md` |
| Crash watch | systemd-coredump raises a "Process crashed" notification. A click runs `omarchy agent crash <pid>`, which starts the default agent with the `diagnose-crash` skill | `bin/omarchy-agent-crash` |
| Remote | Tailscale (bar panel, `omarchy tailscale send`), and Sunshine for Moonlight streaming (`omarchy install service sunshine`). SSH is off by default, and ufw blocks inbound traffic | manual "Networking", "Security" |
| Provisioning | Unattended install from a `cidata` drive: `user_configuration.json`, `user_credentials.json`, optional `authorized_keys` and `tailscale_authkey` (the machine joins the tailnet at first boot) | manual "Unattended Installs" |
| Sudo | `omarchy-sudo-passwordless [1-1440 min]` gives passwordless sudo "for AI agents" for a limited time | manual "Security" |

### 1.2 Fleet Deck today

| Area | Fact | Evidence |
| --- | --- | --- |
| Browser login | Password login or a one-time link (`#login=<token>`, 5 minutes, single use). Both give the full owner session: `OWNER_SCOPES = [Read, Drive, Approve, Admin]`. There is no read-only browser login | `deck/src/http/auth_routes.rs:105-126`, `store/auth.rs:32-51` |
| Principals | `Owner`, `Token(ApiToken)`, `Member(MemberId, ShareGrant)`. `owner_only()` accepts only `Owner` | `store/auth.rs:91-122`, `http/guard.rs:201-211` |
| Tokens | Bearer tokens with scopes `read/drive/approve/admin`, made only with the local CLI. A `read` token can open the WebSocket with a ticket. The board web app uses cookies only | `http/methods_table_rest.rs:328-334`, `http/guard.rs:221-243`, `web/src/runtime/api.ts:28` |
| Members | `viewer < collaborator < approver`. A viewer with a `Board` share sees cards, timelines and the Watch page, but not activity, artifacts, tasks or station info | `store/members.rs:77-125`, `web/src/sharing/member.ts:107-120` |
| Watch page | `#/watch`: up to 24 live tiles (one subscription each; a socket holds 32), 4 lines of 140 characters, needs-you first. Focus mode cycles every 20 s. The tiles have answer buttons. The grid is `auto-fill, minmax(280px, 1fr)`, there is no wall breakpoint, and the URL holds no state | `web/src/watch/WatchPage.tsx`, `tiles.ts:11-125`, `styles/views.css:54-65` |
| Station info | `StationInfo` has optional fields (`#[serde(default)]`), and `resources` comes from DL-193 | `proto/src/link.rs:244-284` |
| Link features | Each optional capability is a feature string such as `coordinator.v2`, negotiated per link. A method row names its feature. The board gates controls on optional `StationInfo` fields | `proto/src/version.rs`, `link/src/methods.rs:703-710` |
| Service | A systemd user unit with `PATH`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `FLEETDECK_LOG_FORMAT` only. It does not import the graphical session environment (`WAYLAND_DISPLAY`, `HYPRLAND_INSTANCE_SIGNATURE`) | `cli/src/service/mod.rs:386-424`, `design/service.md:46` |
| Coordinator MCP | Classes `read, mail, drive, spawn, inspect, manage, answer` (and `own`). Fleet-control classes need elevation, `coordinator.v2`, and two kill switches (`fleet_control` on the deck, `allow_fleet_control` on the station) | `station/src/coordinator/bindings.rs:32-48`, ADR-008 §3 |
| Artifacts | Kinds html, md, image, svg, pdf. `POST /api/artifacts/{station}/{id}/view` gives `/a/{token}/` for 1 hour, and that URL needs no cookie | `deck/src/http/artifact_routes.rs:557-596` |
| OS integration | No code for Omarchy, Hyprland, Wayland, kiosks, OS notifications or screen capture | repository search, 2026-10-16 |

---

## 2. Goals and non-goals

Goals:
1. **The wall.** The 21:9 screen of the server shows the whole fleet as a read-only command center. The layout adapts to other screen shapes.
2. **Control of the screen through the fleet.** From the board (laptop or phone), the owner chooses what the wall shows, turns the screen on or off, locks it, and opens previews, terminals and artifacts on it.
3. **A desktop MCP.** An agent can show its work on a screen, send a notification, take a screenshot, record a demo and, with a separate grant, use the keyboard and the pointer (Computer Use, DL-197).
4. **Native Omarchy integration on every Omarchy machine** (server and laptop): a bar widget, a panel, notifications, menu rows, hotkeys, hooks, theme sync and an import of agent accounts.
5. **Omarchy stations with no manual steps.** A new machine installs, joins the tailnet and pairs with the deck by itself.

Non-goals:
- Approvals on the wall or from a desktop notification. Approvals stay on the board of a logged-in owner device (owner answer 2).
- A separate OS user for agents. This is not now (owner answer 4). §12 states what this costs.
- A change to Omarchy upstream. §8.10 lists the upstream changes that would help. They are owner decisions.
- macOS and other Linux desktops. The desktop adapter is Hyprland-first. A plain Hyprland desktop without Omarchy gets the Hyprland actions only (§6.1).

---

## 3. Architecture

```
                              ┌──────────────────── DECK ────────────────────┐
  laptop board (owner) ──────▶│ displays (§5)   wall directives (§4.5)       │
  phone board (owner)  ──────▶│ desktop.act relay (§6)   desktop MCP (§7)    │
                              │ fleet summary (§8.2)                         │
                              └───────┬─────────────────────────────┬────────┘
                                 link │ desktop.v1               link │ desktop.v1
                     ┌────────────────▼──────────┐   ┌──────────────▼────────────┐
                     │ STATION "srv" (server)    │   │ STATION "laptop"          │
                     │ desktop adapter (§6)      │   │ desktop adapter (§6)      │
                     │  hyprctl socket, omarchy  │   │  hyprctl socket, omarchy  │
                     │ fleetdeck desktop feed    │   │ fleetdeck desktop feed    │
                     └──┬─────────────┬──────────┘   └──┬────────────────────────┘
                        │             │                 │
               ┌────────▼───┐  ┌──────▼──────────┐  ┌───▼────────────────────────┐
               │ wall kiosk │  │ omarchy-shell   │  │ omarchy-shell plugin,      │
               │ #/wall     │  │ plugin (§8)     │  │ notifications, owner board │
               │ (display   │  │ bar, panel,     │  │ in the normal browser      │
               │  cookie)   │  │ notifications   │  │                            │
               └────────────┘  └─────────────────┘  └────────────────────────────┘
```

There are four channels. Each one has one owner:

| Channel | Path | Carries | Authority |
| --- | --- | --- | --- |
| A. Wall content | deck → display (WebSocket) | the read model, and directives (mode, focus, artifact) | the display principal can only read (§5). Directives come from the owner or from the wall's own station (§4.5) |
| B. Desktop control | board → deck → station `desktop.*` → Hyprland and Omarchy | typed actions (§6.3) | owner only, plus the kill switches |
| C. Local shell | station ↔ `fleetdeck desktop feed` ↔ omarchy-shell plugin | a compact fleet summary and notifications | same-UID local socket. Read-only, except wall navigation (§8.4) |
| D. Agents | MCP → station → deck → target station `desktop.*` | the same typed actions as B | grant classes `desktop`, `capture` and `input` (§7) |

The doctrine applies as follows:
- **Rule 1 (drive through the protocol):** the desktop is driven through the Hyprland IPC socket, the `omarchy-shell` IPC and the `omarchy` CLI. Input synthesis (Computer Use) exists only in the `input` class (§7.4), and DL-197 names it as the exception.
- **Rule 2 (the station owns execution):** every desktop action runs on the station of that desktop. The deck only relays.
- **Rule 5 (credentials stay on the station):** the display login never goes through argv, a URL in a process list, or a file that the station writes (§6.3, `wall.open`).
- **Rule 8 (capability-honest UI):** the board shows desktop controls only for a station whose `StationInfo.desktop` is present. A refused action shows its reason.
- **§4 "Editing another tool's config":** the installer writes only to drop-in places. It asks for consent and has an uninstall (§8.1).

---

## 4. The wall (board route `#/wall`)

### 4.1 Layout: flexible, with 3440×1440 as the main target

The wall is a full-screen board route with no sidebar and no top bar. One CSS grid uses named areas. A `ResizeObserver` on the root picks a **shape class** from the aspect ratio of the viewport:

| Shape | Ratio (w/h) | Example | Areas (columns) |
| --- | --- | --- | --- |
| `ultra` | ≥ 2.2 | 3440×1440 (2.39), 5120×1440 (3.56) | `needs` 20% · `tiles` 1fr · `side` 20%; `stream` under `tiles` |
| `wide` | 1.6 to 2.2 | 2560×1440, 1920×1080 | `needs` 24% · `tiles` 1fr; `side` and `stream` share one row under them |
| `standard` | 1.2 to 1.6 | 1920×1200, 2560×1600 | `needs` on top (one row), `tiles`, then `side` |
| `portrait` | < 1.2 | a screen turned 90° | one column: `needs`, `tiles`, `stream` |

Scale:
- The base font is `clamp(14px, viewport height / 80, 28px)`, which gives 18 px at 1440 px high. The CSS variable `--fd-wall-scale` multiplies it, and its URL parameter `scale` accepts 0.75 to 2.0.
- Tiles use `grid-template-columns: repeat(auto-fill, minmax(calc(22rem * var(--fd-wall-scale)), 1fr))`. On 3440×1440 with `ultra`, that gives 3 columns of 3 to 4 rows (9 to 12 tiles).
- The route takes the tile limit from the area size. The hard maximum stays at 24 (one subscription per tile, 32 per socket).

URL state: `#/wall?mode=<mode>&stations=<a,b>&repo=<r>&scale=<n>&shape=<auto|ultra|wide|standard|portrait>`. The route restores all of it on a reload.

### 4.2 Areas

| Area | Contents | Source |
| --- | --- | --- |
| `needs` | Open requests, oldest first: callsign, station, request kind, the tool name and a one-line summary, and the age. Each item has a QR code (§4.3). | card `open_requests` |
| `tiles` | Watch tiles (`WatchPage` tile component, read-only form) | Watch subscriptions |
| `side` | Usage (tightest window first, reset countdown), stations (link state, DL-193 resources, per-session load from §10), PR and check states of open sessions (R3) | cards, `stations.list`, `fleet.extras` |
| `stream` | The merged activity stream (DL-169), one line per item, the newest at the bottom | `activity.list` |

### 4.3 Read-only rules

- The wall shows no answer buttons, no composer and no write control. Its principal cannot write anyway (§5).
- Each needs-you item has a QR code for `<deck origin>/#/s/<station>/<session>?request=<id>`. The owner scans it with the phone, which must have its own owner login. The URL holds no token.
- Keyboard keys on the wall only navigate: `←/→` change the mode, `Tab` moves the focus between tiles, `Enter` focuses a tile, `Esc` returns to Mission. The keys work only when the kiosk window has the keyboard focus.

### 4.4 Modes

| Mode | What it shows | Enters |
| --- | --- | --- |
| `mission` | The four areas | default |
| `focus` | One session in full width: the chat on the left 60%, its live "Since the session started" diff on the right 40% | a directive; auto-focus |
| `review` | The whole-session diff of one finished session with its sent review comments (read-only) | a directive |
| `race` | The DL-170 compare view of one `Nx` batch, with up to 4 columns | a directive |
| `artifact` | One artifact, full screen, through its `/a/{token}/` URL in the sandboxed frame | a directive |
| `ambient` | A dark, low-motion view: counts, the tightest usage window, and the next needs-you. The content moves a few pixels every 5 minutes (burn-in on OLED screens) | after `ambient_after_min` (default 30) with no activity, or a directive |

**Auto-focus.** When a new needs-you request opens for a session in the wall's filter, the wall goes to `focus` on that session for `focus_hold_s` (default 60). It goes back to the previous mode when the request closes or the time ends. A directive with `pin: true` stops auto-focus until the next directive.

### 4.5 Directives

A directive is a small instruction from the deck to one display: `display.directive { display_id, mode, session?: {station, id}, artifact_url?, batch_id?, pin?: bool, filter?: {stations, repo} }`.
- The deck keeps the last directive of each display, so the wall restores it after a reload.
- **Who can send one:**
  - the owner, from the board (`POST /api/displays/{id}/directive`, scope Drive, `owner_only`);
  - a coordinator with the `desktop` class (§7);
  - the station that the display is bound to (§5.4), for the local hotkeys of §8.4 (link method `display.directive`, feature `desktop.v1`). That station can address only displays that are bound to it.
- For `artifact`, the deck makes the `/a/{token}/` URL when it accepts the directive. The display never calls an artifact route.
- Each directive is a row in the audit log.

### 4.6 Tests (four proofs)

- **Fail-open:** the deck restarts, and the wall shows "Reconnecting", keeps the last data and reconnects with no login. A failed tile subscription shows its tile as "unavailable" and the rest still render.
- **Determinism:** the shape class and the tile count are a pure function of the viewport and the URL (unit test with the four shapes). Auto-focus follows the request order only.
- **Exposure:** the wall renders no write control (DOM test). A QR URL holds no token (test).
- **Version:** an older deck without displays shows the wall from an owner login only, with the note "This deck has no wall displays".

---

## 5. The display principal: a read-only browser login (HIGH RISK)

### 5.1 Why a new principal

- An owner cookie in the kiosk would give full power to anyone who can read the kiosk profile, and on the server that includes every agent (§12).
- A `read` token does not work in a browser, because the board uses cookies only.
- A viewer member hides the surfaces that a wall needs (activity, station info, artifacts).

The wall therefore gets its own principal, which is read-only by construction.

### 5.2 Design

- **`Principal::Display(DisplaySession)`**, with a new `LoginMethod::Display`. `has(scope)` is true only for `Read`. `owner_only()` refuses it.
- **Deny by default.** A method row opts in with a new `.display()` marker, the same way `.member()` works today. The guard refuses a display on every other row with 403 `display_read_only`.
  - Rows that may opt in: the WS ticket and the WS, `sessions.list`, `stations.list` (without `key` and `dial_url`), `activity.list`, `fleet.extras`, the read rows that the review and race modes need (diff since spawn, review comments, the compare summary), and `displays.self`.
  - These never opt in: Admin rows, files, terminals, shells, settings, tokens, grants, shares, artifact routes, and every row that writes.
  - The phase brief lists the exact rows. The adversary reviews that list.
- **WebSocket.** A display may only subscribe and unsubscribe. Every other client message is refused, and a test covers each message kind.
- **Station filter.** A display has `stations: all | [names]`. It sees only cards, activity and resources of those stations. The default is `all`.
- **Cookie.** `fd_display`: `HttpOnly; Secure; SameSite=Strict`. The idle limit is 30 days and the maximum is 90 days, and each use extends it up to the maximum. The owner can revoke a display at any time.
- **Creation (owner only, with elevation, DL-172):**
  - On the board: Settings › Displays › "Add a wall display". The owner gives a name and a station filter. The deck makes a one-time display link (5 minutes, single use) as a URL and a QR code. The fragment is `#display=<token>` and it works only on `POST /api/auth/display-link`.
  - With `wall.open` (§6.3). The station opens the link for the owner.
  - On the deck host: `fleetdeck deck display link <name> [--stations a,b]` (local socket).
- **The board's Displays list** shows the name, the filter, the bound station and monitor, the last seen time and the last directive. It has a Revoke button. Every create, use and revoke is an audit row.

### 5.3 What a display shows and what it does not

| Shows | Does not show |
| --- | --- |
| Cards, timelines (tiles, focus mode), open requests (as items, without answer controls), activity, usage from cards, station names and resources, diffs and review comments for the review mode | Files, terminals, settings, tokens, grants, shares, mail, tasks, plans, audit, the content of artifacts (except through a directive's `/a/` URL), station keys and dial URLs |

### 5.4 Binding a display to a desktop

A display can be **bound** to one station and monitor: `binding: { station, monitor }`. The binding is set when `wall.open` launches it, or by the owner in the Displays list. The binding allows:
- that station's hotkey directives (§4.5);
- the wake rule (§6.6).

### 5.5 Tests (four proofs)

- **Exposure:**
  - A display cookie is refused on every row that has no `.display()` marker. This is a table-driven test over the whole method table, and it fails when a new row has no decision.
  - The WS refuses every write message.
  - A display never sees a station outside its filter.
  - A display link works once, and only within 5 minutes.
- **Fail-open:** revoking a display closes its WS within 5 s. A deck restart keeps the display logins.
- **Determinism:** no model call.
- **Version:** an older board build ignores the `display` login fragment and shows the normal login.

---

## 5A. Revision 2: binding rules after the design review of §4.5, §5 and §6 (round 1)

These rules override §4.5, §5 and §6 where they differ. The numbers name the review findings.

1. **Display activity without mail (1).** A display gets a separate activity projection: no `mail` items and no text of a person; the station filter applies before the pagination. Test: a mail with a sentinel secret never reaches a display.
2. **A display wire model (2).** The WebSocket of a display uses an **outgoing allowlist**, applied to snapshots, deltas, replay, live events and lag recovery: card summaries (title, callsign, status, station, repo name, counts, the open request kinds and tool names, the age), and timeline items reduced to: run start (the prompt's first line), assistant message text, tool item title and status (no input, no output chunk, no file list), request kind and tool name (no tool input). People-chat, mail, tool output, approval inputs, file lists and diffs are never sent to a display. A test sends every event kind with a sentinel secret and checks that no frame to a display holds it.
3. **The display ceiling and typed directives (3).** The owner sets each display's ceiling (§5.2 filter) as **station ids**; a directive's filter can only narrow it. A directive names typed references: `session: {station_id, session_id}`, `batch_id`, `artifact: {station_id, artifact_id, version}`, never a URL. The deck checks each reference against the ceiling and the sender's authority (owner: any inside the ceiling; the bound station: only its own sessions and artifacts; a coordinator with the `desktop` class: only its own task's sessions and artifacts). The deck stores identifiers, never a capability URL; audit rows hold identifiers.
4. **Station ids, not names (4).** The ceiling resolves names to immutable station ids when the owner saves the display. A station rename or a session move does not change what a display may see; a subscription whose session leaves the ceiling is closed at once.
5. **Diffs for a display (5).** A display gets **change summaries** only (the files and their added/removed counts, no content) from a new row `displays.session_changes` (filtered, read-only). Patch content needs a per-display owner switch `show_diffs` (default off), and then the server limits it to sessions inside the ceiling, 64 KiB per file and 1 MiB per answer. §5.3 and §12 change accordingly. The P1 focus, review and race panes use the summaries for a display login and the full diff for the owner login.
6. **A display-bound artifact view (6, 8).** A display opens an artifact only through `GET /api/displays/self/artifact/{station}/{id}/{version}/{*path}`, which checks the display cookie, the ceiling and the last directive on **every** request, and stops working when the display is revoked or the directive changes. No public `/a/{token}/` share is made for a display. The P1 artifact pane uses this route for a display login.
7. **Pairing instead of a token in the command line (7, 11).** `wall.open` carries **no credential**: the station starts the kiosk Chromium on `<deck>/#/display-pair` (no secret in the URL). The page asks the deck for a pairing (`POST /api/displays/pair`, no login), and the deck answers with a random 6-digit code and sets a pending `fd_display_pending` cookie bound to that pairing (5 minutes). The kiosk shows the code full screen. The board of the owner (logged in, with elevation) lists pending pairings with their code, IP and user agent; the owner picks the display (name, ceiling) and types the code. The deck then turns the pending cookie into the display cookie. An attacker who opens the pair page gets its own code, which the owner does not see on the wall screen. The one-time display link of §5.2 stays for other devices, made by the owner on the board. No display token travels through the station, so no relay can persist it.
8. **Arguments of desktop actions (10).** The station runs fixed executables with structured argv (never a shell, never Hyprland `exec`, never a concatenated batch). Grammars: monitor `^[A-Za-z0-9._-]{1,64}$`; workspace `1..=99` or `^[A-Za-z0-9_-]{1,32}$`; window address `^0x[0-9a-f]{1,16}$`; notification title ≤ 80 and body ≤ 300 characters after markup removal; notification `open` only a board route that starts with `#/`; `browser.open` only `http`/`https` URLs of at most 2048 bytes. Tests with injection strings for each argument.
9. **Window ownership (12).** The station records each window it opens as `(Hyprland instance signature, address, pid, class, opener session, opened_at)`. Before `window.close` or `wall.close` it reads `hyprctl clients -j` and closes only when instance, address, pid and class still match; a compositor restart forgets every record.
10. **Proofs added (13).** Atomic single-use redemption of a link and of a pairing; separate cookie names and token domains (`fd_session`, `fd_display`, `fd_display_pending`) with tests that swap them; CSRF on the pairing and logout routes, and logout clears the display cookie; revocation before the WS ticket is redeemed, before the snapshot and during a slow send; the comment filter of the review pane (drafts inside sent threads); the live gates of `desktop.act` at intake and at dispatch; the wake rule respects `wake_on_needs_you`, the quiet hours and both kill switches; an active kiosk cannot extend the 90-day absolute limit.
11. **The cost of the kiosk on the server (9).** §12 states it exactly: a same-UID process on the server can read the kiosk's display cookie and so read, until revocation and at most 90 days, the summaries of the stations in the ceiling (not mail, not tool output, not file content unless `show_diffs` is on). The ceiling default is **the stations chosen at creation**, never "all future stations".

## 5B. Revision 3: rules after the design review round 2

These rules override §5A, §4.5, §5 and §6 where they differ.

1. **One display payload policy for every route.** The same reduction applies to the WebSocket, `activity.list`, `sessions.list`, `fleet.extras` and every other row a display can read: a tool appears only as its kind, its safe name (the tool name the provider reports, cut at 40 characters) and its status; never `input_summary`, a command, MCP arguments, an edit input, a file list or approval details; a request appears only as its kind and tool name. The display activity is built from these fields only. Tests send sentinel secrets in a shell command, MCP arguments, an edit input and approval text, and read them through the real HTTP endpoints and the WebSocket of a display: no answer holds a sentinel.
2. **Pairing limits.** `POST /api/displays/pair`:
   - budgets: 3 creations per minute per source IP and 20 per minute for the deck; at most 10 pending pairings at a time (the oldest is not evicted: a new request gets 503 "Too many display pairings; try again later");
   - each pending record is a fixed-size row (id, code, a hash of the pending secret, the source IP, the user agent cut to 120 characters, the creation time) and expires by a timer at 5 minutes;
   - the 6-digit code is unique among the active pairings: the deck reserves it atomically and retries at most 5 times, then 503;
   - the kiosk polls its pairing at most once per second (429 above);
   - the owner's list shows at most the 10 pending records;
   - the owner approves by the pairing **id** and the code together; the approval consumes the pending secret and the kiosk's next poll gets a **new** display cookie (the pending cookie is cleared);
   - a code alone, a wrong pending cookie, an expired pairing and a second approval are refused (tests, also concurrent approve and claim).
3. **Directives, completed.** A `batch_id` is expanded to all its sessions, and every one is checked against the ceiling and the sender's authority; a directive restored after a deck restart is checked again against the current ceiling before the display gets it.
4. **Patches, completed.** With `show_diffs` on, the member file deny list (secrets, keys, `.env` and the other entries of DL-154) also applies to display patches; `show_diffs` is read live on every request.
5. **Desktop arguments, completed.** A workspace is only a number 1 to 99 (typed `u8`, no names, no relative values); a monitor is resolved against the live `hyprctl monitors -j` list and must equal one name there exactly; every argv puts the option terminator `--` before a value where the tool supports it.
6. **Window records, completed.** Records hold `(instance signature, address, pid, class, opener session)`; the station prunes the records that no longer appear in `hyprctl clients -j` at each info refresh and after each action; a close checks the record against a fresh `clients` read right before it closes. The residual race (a window that closes and a new one with the same address, pid and class in the same instant) is accepted and stated.
7. **The kiosk cost, completed.** §12 item 1 also states that a copied display cookie reads assistant message text and the current directive's artifact of the sessions in the ceiling.
8. **Proofs, completed.** Owner and display cookies in one browser at the same time (each route sees the right principal); revocation before an unused link is redeemed; the kiosk profile is cleared on `wall.close`; a terminal opened by `terminal.open` follows the shell policy (`allow_shell`, the roots); the pairing tests of item 2.

## 5C. Revision 4: rules after the design review round 3

These rules override §5B, §5A, §4.5, §5 and §6 where they differ.

1. **Pairing states (R3-1).** A pairing is `pending → approved → claimed`, or `expired`:
   - `pending`: made by `POST /api/displays/pair`; the deck sets the `fd_display_pending` cookie (a 256-bit random secret) and stores only its hash (the verifier); 5 minutes.
   - `approved`: the owner approves by pairing id and code (§5B item 2); the deck records the display (name, ceiling, the approving owner session) on the pairing and **keeps the verifier**; the approved state lasts at most 2 minutes, then `expired`.
   - `claimed`: the kiosk's poll (`GET /api/displays/pair/{id}`) with the pending cookie; in **one transaction** the deck checks the verifier, deletes it, issues a fresh `fd_display` cookie for the new display and clears `fd_display_pending`. A second claim, a claim without the right pending cookie, a claim of a `pending` or `expired` pairing are refused. Expired records are deleted by the timer. Tests: the whole flow; claim before approval; claim with a wrong cookie; two concurrent claims (one wins); approval after expiry.
2. **Directives, live authority (R1-3).** Before a stored or restored directive is delivered, the deck checks again the sender's **current** authority (the owner; the station binding still in force; the coordinator grant still active with the `desktop` class and the task still owning each referenced session and artifact) and each reference against the current ceiling. A change of a grant, a task membership, a binding or a ceiling deletes the stored directives that it affects (the display returns to `mission`).
3. **The display patch endpoint (R1-5).** `GET /api/displays/self/sessions/{station}/{session}/changes` returns the change summary; with the display's `show_diffs` on (read live per request), `?patch=1` returns the patch of the current session diff snapshot (the same snapshot as the owner's "Since the session started" view at that moment), with the station's private-path protection **and** the member deny list applied, the ceiling checked, 64 KiB per file and 1 MiB per answer; a path that is denied appears as "hidden" with no content.
4. **Argument positions per executable (R1-10).**
   - `hyprctl`: fixed argv `hyprctl dispatch <dispatcher> <arg>` with `<dispatcher>` from a closed list and `<arg>` built only from typed values (a number, an exact monitor name from the live list, `address:0x…`); no free text reaches `hyprctl`.
   - Notifications: on both desktops the station runs `notify-send [--urgency=<low|normal|critical>] [--app-name=Fleet Deck] -- <title> <body>` (`notify-send` stops option parsing at `--`; on Omarchy it reaches the same notification daemon over D-Bus); `omarchy-notification-send` is not used (it has no option terminator; round 4). The `open` action of a notification is handled by the station's own D-Bus action listener only when the station has one; else no action. Tests: ordinary text, a title and a body that start with `-` or look like an option.
   - `chromium --app=<url>`: the URL is one argument that starts with `--app=` and is checked (`http`/`https`, ≤ 2048 bytes, no whitespace); nothing else from the request reaches its argv.
   - `grim`, `gpu-screen-recorder` (P5): geometry from typed values only.
5. **Window lifecycle from Hyprland events (R1-12).** Every window the station opens gets a **unique class** `fleetdeck-<kind>-<launch id>` (`kind` = `wall`, `preview`, `term`; the launch id is a fresh random id), and its own process: `browser.open` starts Chromium with a fresh profile `--user-data-dir=$XDG_RUNTIME_DIR/fleetdeck/browser-<launch id>` (deleted when its window closes), the wall with its display's profile (a second launch for the same display first closes the first), and `terminal.open` starts the terminal program directly (not a launcher that can reuse a process) with the class set by its option. The station reads Hyprland's event socket (`.socket2.sock`): the first `openwindow>>ADDRESS,WORKSPACE,CLASS,TITLE` with that class is the launch's window (the pid is not used, because a program can reuse a process; round 4), and `closewindow>>ADDRESS` deletes the record. A launch whose window does not appear in 15 s is reported as failed and its process is stopped. A gap in the event stream (a reconnect, a Hyprland restart, a new instance signature) clears **all** records, so the station owns no window it might confuse. The check right before a close (§5B item 6) stays and also checks the class; the remaining race is only between that check and the close.
6. **Responses and the terminal (R1-13).**
   - `GET /api/auth/session` and the logout route are typed per principal: with both cookies in one browser, a board route answers the owner and a display route answers the display; logout of one clears only its cookie. Tests for each combination.
   - `terminal.open` uses the existing handover path unchanged (`shell/handover.rs`), with its live checks of `allow_shell`, the roots **and** the provider mode and lifecycle rules; the desktop action only chooses where the window opens.
7. **Owner-facing text (R1-9).** The Displays page shows the §12 cost in one line under "Add a wall display": "A wall can be read by programs that run as you on its machine: it shows the summaries, the assistant messages and the shown artifact of the stations you choose."

## 5D. Revision 6: P2b rules (payload policy, directives, changes, artifacts)

These rules answer the 19 open questions of P2b (the fact sheet `r13-p2b-facts.md`, section 8). They override §5C, §5B, §5A, §4.5 and §5 where they differ. The item numbers equal the question numbers (P2b-1 to P2b-19); item 20 lists the risks. The owner decisions are DL-221 (item 1) and DL-222 (items 9 and 12). No item adds a library dependency. The build plan is in §11.

1. **P2b-1 Artifacts (DL-221, owner).** The display artifact route serves a version only when the version holds exactly one self-contained file: one HTML file, Markdown, an image, SVG or PDF. For a multi-file artifact the wall shows the card "Open this artifact on your own device". The route keeps cookie authentication (`fd_display` only), the opaque-origin sandbox and the live directive checks of §5A item 6. The deck makes no capability URL, no share row and no new token type.
2. **P2b-2 Senders.** P2b builds the owner sender. A directive carries a typed provenance: `Owner`, `Station` or `Coordinator`. The `Station` and `Coordinator` variants return `not_granted` until their authenticated entry points exist (station hotkeys in P4, the `desktop` grant class in P6). This gives one authorization model without a premature grant of authority. The activation of the coordinator sender moves from P2 to P6 (§11).
3. **P2b-3 Delivery.** A new row `GET /api/displays/self/directive` returns the validated directive and its generation, or `mission`. The wall polls it every 2 seconds. The deck checks the sender authority and every reference again on each delivery. P2b adds no directive WebSocket message.
4. **P2b-4 Storage.** Migration V25 stores one validated directive for each display: the sender identity, the expanded references and the creation time. A monotonic generation counter continues across a deletion. The deck deletes the directive in one transaction (the display returns to `mission`) when:
   - a new directive replaces it;
   - the display is revoked;
   - the binding or the ceiling of the display changes;
   - an affected grant or task changes.

   The deck validates a restored row again before it uses the row.
5. **P2b-5 Body.** `POST /api/displays/{id}/directive` has `deny_unknown_fields` and one variant for each mode:
   - `focus` and `review` need one session;
   - `race` needs one batch;
   - `artifact` needs one artifact with an explicit positive version;
   - `mission` and `ambient` have no content reference;
   - a field that does not belong to the mode is refused.

   References are immutable identifiers. The filter has at most 64 stations and a repository basename of at most 255 characters. The body is at most 8 KiB. The deck expands and authorizes **every** member of a batch before it selects the four columns of a race. Errors: `400 invalid_directive`, `404 display_unknown` (owner-facing), `403 outside_ceiling`, `403 not_granted`. The deck writes the audit row `display.directive` with identifiers only.
6. **P2b-6 Station parameters.** The new display routes accept station ids only, never a station name. A name would add ambiguity to a ceiling that uses ids.
7. **P2b-7 Timeline policy.** The deck builds a dedicated display event type from scratch. It does not strip fields from the owner event. The display event type holds only these items:
   - run and turn boundaries, with structural ids and status;
   - at the run start: the first line of the prompt, at most 120 characters, only for an ordinary prompt origin that is established; the fixed text "Run started" for the origins Shell, Mail and Coordinator and for an ambiguous origin;
   - the assistant message text;
   - tool kind, tool name (at most 40 characters) and status;
   - request kind, the correlated safe tool name and fixed lifecycle labels.

   The display event type has no author, input id, option id, provider reference, file list, exit code, checkpoint or `raw_ref`. The deck drops `session.started`, reasoning, output, plans and **every other event that is not listed or not known**. A dropped event still advances the cursors: the server and the client both advance through an explicit high-water mark, so that a suppressed event is never replayed again. One function serves every send path (snapshot, delta, replay, live event and lag recovery).
8. **P2b-8 Activity.** A display gets a separate activity projection that holds only:
   - assistant message excerpts;
   - the safe tool name and status;
   - fixed approval and lifecycle labels (no free-form end reason);
   - no mail rows and no plan rows.

   The deck applies the ceiling and the filter before the pagination. The query `repo=` matches the displayed basename. The query `session=` must be inside the effective scope. The deck never filters or describes owner activity after the fact, because that activity can hold secrets.
9. **P2b-9 Mail count (DL-222, owner).** The wall shows no mail at all. For a display, `fleet.extras` returns `mail_pending` 0 and an empty `files` list.
10. **P2b-10 Resources.** A display gets a reduced resource DTO: numeric host metrics and the load of the sessions that the display may see. It has no disk path, no process detail and no unrelated station information. This keeps §5.3 ("station names and resources") without a return to the full `StationInfo`.
11. **P2b-11 Card fields.** The display card gets back only the validated batch id, index and size, for the grouping of a race. The display card never gets `base` and `review_rev`; the panes are adapted. P2b checks each retained P2a field again against an explicit allowlist. A P2a field is not allowed only because P2a sends it.
12. **P2b-12 Review (DL-222, owner).** Review mode on a display shows change summaries. It shows patches only when `show_diffs` is on. A display never gets review comments or drafts: there is no review-comment endpoint for a display. `show_diffs` allows code, not the text of a person.
13. **P2b-13 Change source.** The station gets the method `files.display_changes`, carried by the new link feature `display_changes.v1` (a station without it answers no changes: the deck route returns `409 changes_unsupported`). It reuses the session-diff snapshot engine. It returns the per-file statistics, the scope, the truncation flag and, on request, the patches. The station applies the private-path protection and the member deny list **before** it builds the answer. A denied entry is an anonymous `hidden` marker with no path and no content. Binary counts are unknown, not zero. A deck-side default or an owner-mode fetch does not meet this contract.
14. **P2b-14 Limits.** The deck omits a whole patch section above 64 KiB and marks it `too_large`. The stricter station cap of 256 KiB for a patch stays. The serialized answer is at most 1 MiB. `patch=1` returns `403 diffs_off` when `show_diffs` is off. The deck joins identical computations and allows one outstanding computation for each display. A wall polls at most every 5 seconds. The deck checks the policy again after the station await.
15. **P2b-15 Exact rows.** The list of display rows after P2b is:
    - the seven P2a rows;
    - `.display()` added to `activity.list`, `fleet.extras`, `displays.session_changes`, `displays.self_artifact` and `displays.self_directive`.

    The new owner-only Drive row is `displays.directive`. The ordinary artifact, file, review and compare rows stay closed to a display. Every row `/api/displays/self/...` authenticates with `fd_display` only, also when an owner cookie is present. The test `exactly_these_methods_are_open_to_a_display` lists the new rows.
16. **P2b-16 Subscriptions.** A display can Subscribe and Unsubscribe, and only for a session that it can see now. The 32-subscription cap stays, and so does the 24-tile limit of the UI. Timeline visibility is separate from social visibility: a display gets no Presence, Chat, Driver or DriveRequest frame. The deck refreshes the authorization before each replay, live and catch-up send. It removes a subscription at once when the scope changes.
17. **P2b-17 Directive filter.** The deck enforces the filter on the server for cards, activity, extras, timelines, changes, artifacts and wake eligibility. The effective scope is the ceiling, narrowed by the stations of the directive and by the repository match. An artifact without a provable repository association fails a repository filter.
18. **P2b-18 Documents.** One documentation owner adds the P2a and P2b rows with the four proofs to `PARITY.md` and the new surfaces to `SECURITY-CHECKLIST.md`. A surface that is pending stays listed as pending while the build runs. The statement "every designed surface is built" in the checklist is corrected.
19. **P2b-19 Web.** The web work stays in explicit P2c tasks: directive polling, display timeline decoding, reduced resources, race grouping, the change and patch routes, and the artifact fallback card. The completion of the P2b backend is not reported as a working wall. P2c stays in the backlog until it is done.
20. **Risks (binding mitigations).**
    - Allowed text can hold personal data or repeat a forbidden input: assistant text, prompt excerpts, titles, filenames, patches and artifacts. The policy is a field and provenance policy, not secret detection. Sentinel tests must tell the prohibited fields from the content that is allowed on purpose.
    - The stricter display types, the station change method and the dispatch handshake add scope and version combinations. An unsupported peer fails closed with an explicit reason. The deck never falls back to an owner payload.
    - Authorization must survive asynchronous waits, cache hits and streaming. The deck checks the generation and the live authority before each response emission and cancels the remaining artifact streaming after an invalidation. Bytes that were already delivered cannot be recalled.
    - A remote authority check leaves a check-to-dispatch interval. The DESIGN REVIEW states when an execution counts as started and assesses the interval. The documents do not claim an instant remote cancellation.
    - A single-file HTML or SVG page can still fetch external resources or send its own content out. The display route sets a display-specific CSP that blocks external networking, frames, forms and popups, and keeps the sandbox isolation.
    - Basename filters collide, the wake freshness depends on clocks, and the baseline at startup misses some requests on purpose. These are visible behavior choices, not substitutes for authorization.
    - Re-pairing, binding changes and late desktop replies need generation checks and a serialization for each display. An open that timed out can have succeeded on the station.
    - The return to `mission` removes the narrowing filter of a directive and returns to the ceiling that the owner set. The ceiling stays the durable privacy boundary.

## 6. Desktop control on the station: feature `desktop.v1` (HIGH RISK)

### 6.1 Detection

The desktop adapter is a module of the station, Linux only. It looks for the desktop at each call, because Hyprland gets a new instance signature when it restarts:
1. **Hyprland:** a live socket `$XDG_RUNTIME_DIR/hypr/<signature>/.socket.sock`, found by a successful connect. If there is more than one live instance, the adapter picks the newest and logs the others.
2. **Wayland:** the `wayland-*` socket in `$XDG_RUNTIME_DIR` that the Hyprland instance names (from `hyprctl instances -j`).
3. **Omarchy:** `$OMARCHY_PATH`, or else `/usr/share/omarchy`, with an executable `bin/omarchy`. The version comes from its `version` file.

The service file does not change (`design/service.md`: no other variable). The adapter gives `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, `HYPRLAND_INSTANCE_SIGNATURE` and a `PATH` with `$OMARCHY_PATH/bin` only to the child processes that it starts. Provider sessions do not get them.

Result: `kind = omarchy` (Hyprland and Omarchy), `kind = hyprland` (Hyprland only, so the Omarchy actions are refused with `unsupported`), or no desktop (the station reports no `desktop` field).

### 6.2 `StationInfo.desktop`

An additive, optional field:

```rust
pub struct DesktopInfo {
    pub kind: DesktopKind,                 // omarchy | hyprland
    pub omarchy_version: Option<String>,
    pub monitors: Vec<MonitorInfo>,        // name, description, width, height, scale, refresh_hz, focused, dpms_on
    pub locked: Option<bool>,
    pub dnd: Option<bool>,
    pub idle_inhibited: Option<bool>,
    pub walls: Vec<WallWindow>,            // display_id, monitor, window address, state
    pub allowed: DesktopAllowed,           // desktop, capture, input: the live station settings
    pub tools: DesktopTools,               // wtype, a pointer tool, gpu-screen-recorder, grim: present or missing
}
```

The station refreshes it at most every 10 s, and also after each `desktop.act`. "Unknown is never zero" applies: a value that the adapter cannot read is `None`, and the board shows "unknown".

### 6.3 Link methods

`desktop.info` returns `DesktopInfo`. `desktop.act { action }` takes a closed, typed enum. It never takes a free shell command.

| Action | Effect (Omarchy implementation) | Setting | Result |
| --- | --- | --- | --- |
| `screen.power { monitor?, on }` | `hyprctl dispatch dpms on\|off [monitor]` | `allow_desktop` | ok |
| `screen.lock` | the Omarchy lock (`omarchy-shell` lock target; to check in P3) | `allow_desktop` | ok |
| `screen.dnd { on }` | `omarchy toggle notification silencing` to the wanted state (read the flag file first) | `allow_desktop` | ok |
| `screen.idle_inhibit { on }` | `omarchy toggle idle` to the wanted state | `allow_desktop` | ok |
| `workspace.show { monitor, workspace }` | `hyprctl dispatch focusmonitor` + `workspace` | `allow_desktop` | ok |
| `wall.open { display_id, monitor, workspace? }` | see below | `allow_desktop` | the window address |
| `wall.close { display_id }`, `wall.reload { display_id }` | `hyprctl dispatch closewindow address:…`; a reload is a new launch | `allow_desktop` | ok |
| `notify { title, body, urgency, open? }` | `omarchy-notification-send` (Omarchy) or `notify-send` (Hyprland). The text is plain: markup is removed, the title is cut at 80 characters and the body at 300. `open` is a board route that `--exec` opens with `xdg-open` | `allow_desktop` | the notification id |
| `browser.open { url, monitor?, session? }` | Chromium `--app=<url>` with the class `fleetdeck-preview`. Only `http` and `https` are allowed. The window is recorded as owned by `session` (§7.4) | `allow_desktop` | the window address |
| `terminal.open { session }` | the TUI handover of that session in the default terminal (the existing handover command, built by the station). The window is recorded as owned by `session` | `allow_desktop` | the window address |
| `window.close { address }` | only for a window that the station opened | `allow_desktop` | ok |
| `capture.screenshot { target }` | `grim` (monitor or window geometry from `hyprctl clients -j`). The result is an image artifact (§6.5) | `allow_desktop_capture` | the artifact id |
| `capture.record { target, seconds ≤ 300 }` | `gpu-screen-recorder`, with no audio. The result is a video artifact (§6.5) | `allow_desktop_capture` | the artifact id |
| `input.*` | §7.4 | `allow_desktop_input` | ok |

*Inactive (revision 11): P5a uses only `screen_capture` (monitor, owner); P5b and P6 define their own names in their reviews.* The rows `capture.screenshot` and `capture.record` are historical (see [omarchy-p5a.md](./omarchy-p5a.md)).

**`wall.open` and the display login (rule 5):**
1. The deck makes a one-time display link for the display and sends the token to the station inside the `desktop.act` request, in memory. This is the credential relay pattern, so the token is never stored or logged.
2. The station opens a one-shot HTTP listener on `127.0.0.1:<random port>` with a random path. It accepts one request, answers `303 See Other` with `Location: <deck>/#display=<token>`, and then closes. If no request comes in 30 s, it closes anyway.
3. The station starts Chromium with `--app=http://127.0.0.1:<port>/<path> --user-data-dir=$XDG_DATA_HOME/fleetdeck/wall-<display_id> --class=fleetdeck-wall`, through `uwsm-app --` when it is present. The argv holds only the loopback URL. The token is in no process list and in no file.
4. The station moves the window to the monitor and the workspace, and makes it full screen (`hyprctl dispatch movetoworkspacesilent`, `fullscreen`).
5. The display is then bound to the station and the monitor (§5.4).

The kiosk has its own browser profile, so the display cookie is not in the owner's normal browser, and the owner's cookies are not in the kiosk.

### 6.4 Settings and gates

| Setting | Where | Default | Apply | Danger | Off → on |
| --- | --- | --- | --- | --- | --- |
| `desktop_control` | deck | `false` | Live | yes | elevation |
| `allow_desktop` | station | `false` | Live | yes | elevation |
| `allow_desktop_capture` | station | `false` | Live | yes | elevation |
| `allow_desktop_input` | station | `false` | Live | yes | elevation |
| `capture_monitors` | station | `[]` (all) | Live | no | — |

The rules follow ADR-008 §3:
- The deck reads its own value live at each decision and before each send.
- The station reads its own value live at intake and at dispatch.
- A switch that turns off stops every accepted action that has not started yet.

**Authorization on the board:** `owner_only` with scope Drive for every action, and scope Admin for `capture.*` and `input.*`. Tokens, members and displays are refused.

### 6.5 Artifacts from the desktop

- A screenshot is an `image` artifact (PNG).
- A recording needs a new artifact kind, **`video`** (`mp4`, `webm`). The viewer shows it in a `<video>` element with no autoplay. This is an additive change to R12.
- The publisher is the owner (from the board) or the session (from the MCP).
- The artifact gets the label "desktop capture · <station>/<monitor>".

### 6.5A Revision 10: P5 capture rules (owner decisions DL-230 items 1 and 2; answers the DESIGN REVIEW of §6.5)

These rules replace §6.5 where they differ. They answer the findings A1-A11 of the P5/P8 DESIGN REVIEW round 1 (`adv-omarchy-p5-p8-design-report.md`). The owner answered the two consent and audience findings (DL-230 items 1 and 2); the other answers are architect decisions.

1. **Consent (A1, owner decision DL-230 item 1).** No prompt for each capture. The gates are, all at once:
   - the station setting `allow_desktop_capture` (default off; turning it on needs the owner's elevation, §6.4);
   - the deck setting `desktop_control` (§6D item 7);
   - for an agent: the grant class `capture` (§7.1), which needs the owner's elevation at grant creation;
   - for the owner: the board Desktop panel (owner login, scope Admin).
   A display login, a wall binding, a wall directive and the wake rule give no capture authority. Visible signs instead of a prompt:
   - a screenshot shows a desktop notification on the captured station: "Fleet Deck took a screenshot of <monitor> for <caller>" (`<caller>` is the session callsign, or "the owner");
   - a recording shows "Fleet Deck is recording <monitor> for <caller>" at start and "Fleet Deck stopped the recording of <monitor>" at the end.
   Disclosure (stated in the grant dialog under the `capture` class): "A screenshot that an agent takes goes into the conversation of its provider." Audit rows: `desktop.capture` (start, end, refusal) with the station, the monitor, the caller id and a result code only.
2. **Target (A2).** P5 captures **monitors only**. There is no window target in P5. The target is one monitor name that the station reports now and that `capture_monitors` allows (`[]` = every monitor present at the time of the capture, also a monitor attached later). The capture is the composed pixels of the whole monitor, with every window and notification on it; the grant dialog and the Desktop panel say so: "A capture shows everything on that monitor." A screen that is locked, or whose lock state is unknown, is refused (`screen_locked`, "The screen is locked."). A monitor that is gone at the start is `not_found`; a monitor that goes away during a recording ends the recording (item 5).
3. **Audience (A3, owner decision DL-230 item 2).** A capture is an ordinary R12 artifact: the same read access, view links and share links as any artifact, kept until deleted. It carries immutable provenance in its version metadata: `capture: { station_id, monitor, kind: "screenshot" | "recording", caller: "owner" | <session id>, caller_station, at }`; the viewer shows "Desktop capture · <station>/<monitor> · <time>". The owner accepted that a principal that may read artifacts may read captures.
4. **Deletion (A4).** "Kept until deleted" needs a delete. P5 adds one owner-only operation for **every** artifact: `DELETE /api/artifacts/{station}/{id}` (owner, scope Admin, elevation, CSRF, audited `artifacts.delete`):
   - the deck writes a tombstone row `(station, artifact_id, at)` and in the same transaction deletes its cache rows, view links and share rows of that artifact; every read path checks the tombstone first (404 `unknown_artifact`); a stream in flight ends at its next chunk;
   - the deck sends `artifacts.delete` to the station (feature `artifacts_delete.v1`); the station deletes the index row and the version folders by identity (DL-150 pattern) and answers `deleted`; an offline station gets the call at its next link up (the tombstone stays until the station answered; retries with the backoff of the land outbox);
   - the board shows "Delete artifact" in the viewer menu with the confirm text "Delete this artifact and all its versions? Copies that someone downloaded and text that an agent read stay where they are.";
   - deletion cannot recall downloaded bytes or provider context (stated).
5. **Active recordings (A5).** A recording runs as a child process group that the station owns (one group per recording; no global recorder kill). The station checks every 1 s and stops it on: `allow_desktop_capture` off, `desktop_control` off (the deck sends `desktop.capture_stop`), the grant revoked or expired (the deck sends `desktop.capture_stop`), the screen locked, the target monitor gone, the deck link down, or the end of the duration. Stop: `SIGINT` to the group, 3 s for the recorder to write its file, then `SIGKILL` of the group. A stopped recording keeps its file when the recorder ended cleanly within the grace, else the station deletes the partial file. Maximum cancellation latency: 1 s for the check plus 3 s grace.
6. **Budget (A6).** Screenshot: PNG, at most 32 MiB. Recording: 1 to 120 s (`u16` seconds, the same range for the owner and for an agent), at most 30 frames per second, H.264 in MP4, at most 256 MiB per file (the station stops the recorder at the size limit). One capture at a time per station (`busy`, "A capture runs already."). A free-space reserve of 2 GiB on the station data disk (`disk_full`, "The station has too little free space for a capture."). At most 4 GiB of captures per station (sum of the capture artifacts' bytes; `capture_quota`, "The captures of this station use 4 GiB. Delete some first."). Ordinary R12 artifact policy is unchanged.
7. **Execution (A7).** Screenshot: `grim -o <monitor> <file>`. Recording: `wf-recorder -o <monitor> -r 30 -c libx264 -f <file>` (native Wayland, no privileged helper; `gpu-screen-recorder` is not used). Missing tool: `unsupported` with the tool name. The station runs them through the desktop `Runner` (structured argv, the desktop PATH, the gate) with the start check of the P3b authority. Files: a private folder `<station data>/captures/` (0700), each file created `O_CREAT|O_EXCL|O_NOFOLLOW` 0600; no fallback to the workspace or `~/Pictures`; the folder is swept at station start; every failure path deletes its file.
8. **Import (A8).** A new station-internal `ArtifactService::import_capture(path, metadata)` moves the staged file into the artifact store; it is not reachable from the MCP publish path and keeps the private-path rules of the publish path for everything else. The artifact's station is the captured station. `session_id` is the caller session only when it runs on the same station; else none (no repository association). The result of the action is `DesktopCaptureResult { artifact_id, version, bytes }`. A capture whose import fails is deleted and answers `failed` with code `capture_publish_failed`.
9. **Duration and completion (A9).** A screenshot is one `desktop.act` action (`screen_capture { monitor }`, total deadline 30 s). A recording is one action `screen_record { monitor, seconds }` whose total deadline is `seconds + 30` s; the deck waits `seconds + 35` s. The P3b authority makes each request single use; a timeout is an unknown outcome and is never retried (§6D item 9). The MCP tools return when the action ends.
10. **Video on the board (A10).** The board document CSP gets `media-src 'self'`. The artifact raw route answers `Range` requests for a `video` file (`206`, `416`, `Accept-Ranges: bytes`, `Content-Length`; the authorization runs for every request). Above the 64 MiB cache limit the stream path serves ranges from the station in 700 KiB chunks. The viewer uses `<video controls preload="metadata">` with no autoplay. The wall keeps DL-221: a video is "Open this artifact on your own device".
11. **Negotiation (A11).** New features: `desktop_capture.v1` (a station that can capture) and `artifacts_video.v1` (a peer that knows the `video` kind). The MCP lists `desktop_screenshot` and `desktop_record` only for a station that negotiated `desktop_capture.v1` and reports the tools present. A deck sends a `video` artifact to a board or peer without `artifacts_video.v1` as kind `other`. The MCP screenshot answer is an image content block: the PNG scaled down to at most 1568 px on the long edge and at most 1 MiB, plus the artifact id of the full file.
12. **Tests (four proofs).** Behavior: a screenshot and a recording on the fake desktop (fake `grim`/`wf-recorder` scripts with argv logs). Refusals: each gate off, locked screen, gone monitor, busy, disk full, quota, missing tool, old peer. Exposure: a capture never written outside `captures/`; no capture authority from a display, a binding or the wake. Failure: every stop trigger during a recording; import failure deletes the file; deletion through every read path (cache, raw, view link, share, display, MCP).

### 6.5B Revision 11: P5a and P5b

P5 is split. The detailed contracts of P5a are in the annex [omarchy-p5a.md](./omarchy-p5a.md).

- **P5a** (owner screenshots and artifact deletion): one owner action `screen_capture` for one monitor from the board Desktop panel, and the owner deletion of any artifact. The annex gives the wire contract, the deletion barriers (A4), the budgets (A6), the process and files (A7), the durable import and result (A8), and the names and deadlines (A9).
- **P5b** (recordings, the `video` kind, `Range` playback, the recording stop): later, with its own DESIGN REVIEW. P6 (MCP tools) depends on P3 and P5a.
- Deferred to P5b and P6, and **not active rules**: §6.5A item 5 (active recordings), item 6 (the video budget), item 7 (`wf-recorder`), item 9 (the recording duration), items 10 and 11 (video, MCP).
- The annex wins over §6.5A where they differ.

### 6.6 The wake rule

Each bound display has `wake_on_needs_you` (default `true`) and optional `quiet_hours` (`HH:MM-HH:MM` in the station's time zone). When a request opens for a session in the display's filter, the bound monitor's `dpms_on` is `false`, and the time is not in the quiet hours, the deck sends `desktop.act screen.power { monitor, on: true }` to the bound station. This counts as an owner-configured automation and has its own audit row.

### 6.7 Tests (four proofs)

- **Fail-open:**
  - Hyprland restarts, and the next action finds the new instance.
  - Without a desktop, `StationInfo.desktop` is absent and the board hides the controls.
  - A failed action shows its redacted error on the board.
  - Sessions never depend on the desktop.
- **Determinism:** the action enum is closed. An unknown action is refused with `unsupported`, and the argument limits are tested.
- **Exposure:**
  - The `wall.open` token is not in argv (`/proc/<pid>/cmdline` test), not in a file, and not in logs or audit rows.
  - The loopback listener answers once and then refuses.
  - Notification text is plain text.
- **Version:** a station without `desktop.v1` gets no desktop methods, and the board shows "This station's build has no desktop control". A deck without the feature never sends one.
- **Live smoke (owner machine, not CI):** `make desktop-smoke` on the Omarchy server runs `desktop.info`, `notify`, `screen.power` off and on, `wall.open` and `wall.close`.

## 6D. Revision 6: P3b rules (desktop relay, binding, wake rule)

These rules answer the 13 open questions of P3b (the fact sheet `r13-p3b-facts.md`, section 7). They override §6 and §5C where they differ. The item numbers equal the question numbers (P3b-1 to P3b-13); item 14 lists the risks. The owner decisions are P3b-1, P3b-2, P3b-3 and P3b-5 (DL-223). No item adds a library dependency. The build plan is in §11.

1. **P3b-1 Pairing (DL-223, owner).** `wall.open` targets an existing live display. The kiosk still pairs with a code (`pending → approved → claimed`, §5C item 1). The elevated approval selects that existing display and does not create a new one. On a successful claim, the deck replaces the earlier sessions of the display in one transaction and invalidates its unused login links. The identity of the kiosk then equals the identity that the station uses for the window, and a forgotten kiosk loses its access.
2. **P3b-2 Binding (DL-223, owner).** The deck commits the binding (station and monitor) after a successful open, with a checked operation revision. The binding:
   - stays when a window report is missing or stale;
   - is cleared by a successful explicit close, or by a revoke;
   - blocks an open on another station with `409 display_bound` (a move needs an explicit unbind and rebind);
   - can be edited in the owner settings, with elevation to set or change it, because it gives station authority and the default wake automation.

   A revoke takes effect before the best-effort close of the window.
3. **P3b-3 Quiet hours (DL-223, owner).** `HH:MM-HH:MM` is in the local time of the station.
   - The station reports its current UTC offset with its desktop info, together with the sample time: `DesktopInfo.utc_offset_minutes: Option<i32>` and `DesktopInfo.offset_sampled_at: Option<i64>` (Unix ms); both `None` when `date +%z` fails or prints anything other than `[+-]HHMM`. It reads the offset with one fixed call, `date +%z`, in the system time zone. The deck uses no time zone library.
   - The interval is half-open: the start is inside, the end is outside.
   - A range with a start later than the end wraps over midnight.
   - Equal endpoints mean no quiet hours.
   - When quiet hours are set and the offset is unknown, the deck sends no wake.
4. **P3b-4 Monitor state.** The wake rule asks for a fresh `desktop.info` and bypasses the station cache of 10 seconds. It wakes only the exact bound monitor, and only when `dpms_on` is `Some(false)`. An unknown value means no wake (§6.6).
5. **P3b-5 Trigger (DL-223, owner).** The deck compares the `open_requests` of the cards on each `watch_board()` change, snapshots included.
   - A request that exists when the deck starts is the baseline and never wakes.
   - Only a newly observed needs-you request wakes: an approval or a question that is at most 60 seconds old. A future skew of 5 seconds is tolerated. A replay does not wake.
   - The deck checks that the request is still open right before it sends.
   - Deduplication is by station, incarnation, session and request. The wake is joined for each bound monitor, with a cooldown of 30 seconds.
   - After a missed notification the deck rescans, and it applies the effective filter of P2b (§5D item 17).
6. **P3b-6 API.** The relay has two owner-only routes:
   - `GET /api/stations/{station}/desktop` (scope Read) returns the desktop info;
   - `POST /api/stations/{station}/desktop/act` (scope Drive, mutating, CSRF protected) runs an action.

   The HTTP action enum has no wall URL. A wall action needs an advertised `endpoint_origin` that is not loopback, and the deck builds `/#/display-pair` on the server. The deck checks that `browser_open.session` belongs to the target station. Bracketed IPv6 addresses are parsed by a typed address parser, and the station grammar (P3a) is changed to accept them. After a successful answer the deck updates only the cached desktop data, in one atomic step.
7. **P3b-7 Switch and dispatch authority.** The setting `desktop_control` is `DeckLive.desktop_control`, default `false`, with danger and elevation when it turns on. There is no atomic mirror in the store. The label is "Desktop control". The help text is "Allow desktop actions and configured wall wakes. Off blocks actions that have not started." When the switch is off, the route returns `409 desktop_control_off`. The parsing helpers of the deck move from the full `config.rs` into `config_deck.rs`. The dispatch is also extended: a queued station action checks the deck permission again, through a negotiated and action-bound authority check, immediately before dispatch. A check before the send alone does not meet §6.4.
8. **P3b-8 Errors.** The answers use fixed, redacted messages:
   - `unsupported` or a feature mismatch: `409 desktop_unsupported`;
   - `forbidden`: `403 desktop_forbidden`;
   - invalid parameters: `400`; missing target: `404`; conflict: `409`;
   - a failed or malformed answer: `502`; overload: `503`; timeout: `504`;
   - a lost link: `409 station_unreachable`.

   The deck does not claim that an ambiguous `forbidden` means `allow_desktop = false`, because the link scope refusal uses the same code.
9. **P3b-9 Deadlines.** A wall open, a wall reload and a browser launch get 90 seconds. Every other action and `desktop.info` get 30 seconds. The station enforces explicit total deadlines. The wake runs on at most four workers with a bounded queue. The deck discards an expired candidate. It never retries an action automatically when the outcome of its execution is unknown.
10. **P3b-10 Audit.** An owner action writes `desktop.<action>` with the actor `owner` and the station id as target. A wake writes the action `desktop.wake` with the actor `wake`, the identifiers of the display, the station and the monitor, and the outcome code. The deck audits every attempted mutation, refusals included. A routine skip (quiet hours, display already on) is a counter, not an audit row. The deck never stores a URL, a notification text or a request content.
11. **P3b-11 Kiosk and TLS.** P3b proves the relay, the pairing identity and the binding with fake-link tests. A working wall also needs the pair page of P2c and a certificate that the kiosk Chromium trusts. The deck uses trusted HTTPS or an explicitly provisioned trust. It never turns certificate verification off. These Linux integration tasks stay in the backlog until they are done.
12. **P3b-12 Directive deletion.** A binding change calls the real invalidation helper of P2b in the same transaction (§5D item 4). A helper that does nothing until later is not acceptable in the combined revision.
13. **P3b-13 Documents.** DL-222 is reserved for P2b and DL-223 for P3b. The same documentation owner as in §5D item 18 backfills the P2a and P3a rows and adds the P2b and P3b rows to `PARITY.md` and `SECURITY-CHECKLIST.md`. The DESIGN REVIEW includes the authority handshake and the station protocol additions. A PRE-PR REVIEW follows.
14. **Risks (binding mitigations).** The list is the same as §5D item 20, and it applies to this set as a whole:
    - Allowed text can hold personal data or repeat a forbidden input: assistant text, prompt excerpts, titles, filenames, patches and artifacts. The policy is a field and provenance policy, not secret detection. Sentinel tests must tell the prohibited fields from the content that is allowed on purpose.
    - The stricter display types, the station change method and the dispatch handshake add scope and version combinations. An unsupported peer fails closed with an explicit reason. The deck never falls back to an owner payload.
    - Authorization must survive asynchronous waits, cache hits and streaming. The deck checks the generation and the live authority before each response emission and cancels the remaining artifact streaming after an invalidation. Bytes that were already delivered cannot be recalled.
    - A remote authority check leaves a check-to-dispatch interval. The DESIGN REVIEW states when an execution counts as started and assesses the interval. The documents do not claim an instant remote cancellation.
    - A single-file HTML or SVG page can still fetch external resources or send its own content out. The display route sets a display-specific CSP that blocks external networking, frames, forms and popups, and keeps the sandbox isolation.
    - Basename filters collide, the wake freshness depends on clocks, and the baseline at startup misses some requests on purpose. These are visible behavior choices, not substitutes for authorization.
    - Re-pairing, binding changes and late desktop replies need generation checks and a serialization for each display. An open that timed out can have succeeded on the station.
    - The return to `mission` removes the narrowing filter of a directive and returns to the ceiling that the owner set. The ceiling stays the durable privacy boundary.

## 5E. Answers to the DESIGN REVIEW of revision 6 (round 1)

These rules answer the report `adv-omarchy-r6-design-report.md` (VERDICT: FAIL, 3 BLOCKER, 3 MAJOR). They override §5D and §6D where they differ. The item numbers follow the finding numbers (1 to 6). Items 7 and 8 update the build plan and the status line. The decision DL-221 (single-file artifact) stays. The build plan is in §11.

1. **Dispatch authority (finding 1).**
   - **Feature.** The new link feature is `desktop_authority.v1`.
   - **Parameter.** `desktop.act` gets the additive param `authority: { action_id: String (ULID), action_hash: String (hex SHA-256 of the canonical JSON of the action plus the station id and the target monitor), deadline_ms: i64 (deck time + 10 000) }`.
   - **Fail closed on the deck.** The deck relays `desktop.act` (owner actions and wakes) only to a station that negotiated `desktop_authority.v1`. To any other station it answers `409 desktop_unsupported`.
   - **Fail closed on the station.** A station with the feature refuses a `desktop.act` without a valid authority. The answer is `forbidden` with the text "The deck did not authorize this desktop action."
   - **Single use.** The station keeps each used `action_id` until its deadline passes. It refuses a second use.
   - **Execution start.** "Execution starts" is the first side effect: the spawn of the first program of the action.
   - **Checks.** Right before that spawn, the station checks three items:
     - the deadline has not passed (station clock, 5 s skew allowance);
     - the hash matches the action that the station will run;
     - its live `allow_desktop`.
   - **Waits count.** All waits before that point (the per-display lock, the info reads) count against the deadline. An expired authority gives `failed` with the text "The authorization of this desktop action expired." and no side effect.
   - **Residual interval (accepted).** The deck stops issuing authorities at once when `desktop_control` turns off. An action that was authorized before can still start within its remaining deadline, at most 10 s. This interval is accepted and stated. The documents do not claim an instant remote cancellation.
2. **Display authentication generation (finding 2).**
   - **Column.** The new column is `displays.auth_generation INTEGER NOT NULL DEFAULT 0`. It is separate from the directive generation.
   - **Stored value.** Every display link, approved pairing and display session stores the generation at its creation.
   - **Fence.** Link redemption, pairing approval, pairing claim and display session creation each check, in the same transaction as their write, that the stored generation equals the current one. The write has the form `INSERT ... SELECT ... WHERE displays.auth_generation = ?`. When the check fails, the answer is `401 login_required`.
   - **Replacement.** A replacement (re-pairing an existing display, DL-223 item P3b-1) increments the generation. In the same transaction it deletes every session, every unused link and every approved-but-unclaimed pairing of that display.
   - **Revoke.** A revoke does the same as a replacement.
   - **Tests.** A delayed redemption: redeem, then a replacement, then the session issuance fails. Two competing approved pairings for one display: the first claim wins and the second fails.
3. **Wall artifact rendering (finding 3; keeps DL-221).**
   - **(a) Own document.** The deck serves the board document also at `/display/` (the same `index.html`) with a stricter CSP. Its `frame-src` allows only the display artifact path (`'self'` path `/api/displays/self/artifact/`). Every display link, pair page and kiosk URL uses `/display/#/...`. The board at `/` keeps its CSP.
   - **(b) Inert response.** The display artifact response is inert. It has the header `Content-Security-Policy: sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src data:; font-src data:`. The header has no `allow-scripts`. The wall frame has `sandbox` without `allow-scripts`.
   - **(c) Browser proofs (Playwright, P2c).** An artifact that tries to navigate its own frame to `/a/<token>/` is blocked. A script in an artifact does not run.
4. **Expected display in `wall.open` (finding 4).**
   - **Launch URL.** A managed launch carries the non-secret query `?expect=<display_id>&op=<operation_revision>` in the `/display/#/display-pair` URL.
   - **Pairing record.** The pending pairing stores `expected_display_id` and `operation_revision`.
   - **Approval.** The approval dialog preselects that display and refuses another one (`409 display_mismatch`).
   - **Claim.** The claim refuses when the operation revision of the binding has moved on (`409 superseded`).
   - **Authorization.** The code and the pending cookie stay the authorization.
5. **Offset sample (finding 5).**
   - **Validity window.** The deck uses a `DesktopInfo` offset only when `offset_sampled_at` is within [deck now − 15 s, deck now + 5 s]. Otherwise the offset is unknown, and no wake is sent while quiet hours are set.
   - **Station re-check.** The wake action carries the `quiet_hours` string of the display. The station reads `date +%z` again right before the first side effect. It refuses with `failed` and the text "Quiet hours are active on this station." when its own local time is inside the interval.
6. **Wake replay (finding 6).**
   - **Boundary.** For each station link, the deck records an initialization boundary: the `station_seq` of the first snapshot after the link came up.
   - **Baseline.** A request seen in that snapshot, or in an event with `station_seq` at or below the boundary, is baseline and never wakes.
   - **Wake condition.** Only an `approval.requested` (or question) event with `station_seq` above the boundary can wake. It must be first observed live and at most 60 s old (5 s future skew).
   - **Deduplication.** The key is `(station_id, request_id)`. The deck keeps keys in memory for 24 h (at most 10 000 keys, the oldest out first). The keys stay across a disappearance and across station incarnation changes.
   - **Owner startup rule.** The startup rule of DL-223 (P3b-5) is this boundary for every station link after the deck starts.
   - **Tests.** A delayed initial sync, a reconnect replay, and a restored request under a new incarnation (no wake).
7. **Build plan (§11).** The foundation task also owns the `desktop_authority.v1` and `auth_generation` contracts and the `/display/` document route. The P3b station dispatch task owns the station side of item 1 and the station quiet-hours re-check of item 5.
8. **Status line.** The status line at the top of this document names revision 7.

## 5F. Answers to the DESIGN RE-REVIEW round 2 (revision 7)

These rules answer the report `adv-omarchy-r7-design-report.md` (VERDICT: FAIL, 4 BLOCKER). They override §5E, §5D and §6D where they differ. The item numbers 1 to 4 follow the finding numbers B1 to B4. Items 5 and 6 update the build plan and the status line. The build plan is in §11.

1. **Authority clock contract (finding B1; replaces the 5 s allowance of §5E item 1).**
   - **New field.** The authority also carries `deck_now_ms: i64`. It is the deck wall clock at the time the deck made the authority.
   - **Receipt check.** At receipt, the station records a monotonic instant. It compares `deck_now_ms` with its own wall clock. When they differ by more than 5 000 ms in either direction, it refuses with `forbidden` and the text "The station and the deck clocks differ too much for a desktop action."
   - **Start check.** Right before the first side effect, the action may run only when all of these are true:
     - (a) the station wall clock is at or before `deadline_ms` (exact comparison, no grace);
     - (b) the monotonic time since receipt is at most 10 s;
     - (c) the receipt clock check passed.
   - **No wall-clock extension.** Rule (b) does not depend on the wall clock. A change of the wall clock cannot extend an authority.
   - **Residual interval (accepted).** After `desktop_control` turns off, an action can still start within the link delivery time plus at most 10 s. The documents state this interval. They do not claim an instant remote cancellation.
2. **Single use across restarts (finding B2).**
   - **Consumption.** The station consumes the `action_id` before the first side effect, in one atomic step (one lock, insert-if-absent). A second use is refused.
   - **Retention.** The station keeps each consumed id for 30 s of monotonic time. This is longer than any acceptance interval of item 1.
   - **Link epoch.** Each authority also carries `link_epoch: String`. The station gives a random `link_epoch` to each link connection at its handshake. It is a new additive field of the link handshake reply under `desktop_authority.v1`.
   - **Epoch check.** The station refuses an authority whose `link_epoch` is not the epoch of the connection that carried it.
   - **Restart.** A station restart or a new link connection gives a new epoch. Every earlier authority is then invalid. The design needs no durable store.
3. **The display document path (finding B3; replaces the `/display/` prefix of §5E item 3a).**
   - **Route.** The deck serves the board `index.html` at exactly `/display` (no trailing slash). The relative `./assets/...` URLs of the bundle then resolve to `/assets/...`.
   - **Trailing slash.** `/display/` answers `308` to `/display`.
   - **CSP.** The `/display` response has the stricter CSP of §5E item 3. Its `frame-src` allows only `/api/displays/self/artifact/`. It also has the board `script-src` and `style-src` for `/assets/`.
   - **URLs.** Every display link, pair page and kiosk URL is `/display#/...`. This replaces `/display/#/...` in §5E items 3 and 4.
   - **Browser proof (P2c, `web/e2e-real`).** A test with the production bundle proves four things: pairing, display-link redemption, wall navigation and a reload at `/display`.
4. **Restart-safe wake deduplication (finding B4; replaces the memory-only deduplication of §5E item 6).**
   - **Table.** The deck store gets the table `wake_seen(station_id TEXT, request_id TEXT, first_seen_ms INTEGER, PRIMARY KEY(station_id, request_id))`.
   - **Recording.** The table records every request that the wake rule observes, baseline or live.
   - **Rule.** A request whose key exists never wakes.
   - **Age limit.** The deck prunes rows older than 24 h on each insert.
   - **Size limit.** The table holds at most 10 000 rows. When an insert would pass that limit, the deck prunes the oldest rows first. If the table still cannot take the row, the deck sends no wake (fail closed) and logs a warning.
   - **Test.** A request wakes. The deck restarts. The station restores the request under a new incarnation after the initial snapshot. Result: no wake.
5. **Build plan (§11).**
   - The foundation task owns the `link_epoch` handshake field, the `/display` document route and the `wake_seen` table.
   - The P3b station dispatch task owns items 1 and 2 on the station.
   - The P3b wake task owns the logic of item 4.
6. **Status line.** The status line at the top of this document names revision 8.

## 5G. Answers to the DESIGN RE-REVIEW round 3 (revision 9, accepted by the owner, DL-230)

Round 3 of 3 failed with 2 open blockers (report `adv-omarchy-r8-design-report.md`). These answers are proposed. They become binding only when the owner approves a 4th review round and it passes. P2b and P3b are not built until then.

1. **Single use for the whole connection (round 3 finding 1; replaces the 30 s retention of §5F item 2).**
   - **Retention.** The station keeps every consumed `action_id` for the whole connection epoch. The set holds at most 4 096 ids for each epoch.
   - **Full set.** When the set is full, the station refuses new authorities with `failed` and the text "Too many desktop actions on this link; it reconnects." The station then closes the link connection. The next connection has a new epoch and an empty set.
   - **Connection check.** A queued action checks that its connection is still the current one. It checks right before it consumes an id. It checks again right before the first side effect. When the connection is not current, the action refuses with `forbidden`.
   - **Handshake field.** The station's `Hello` gets a new optional field: `desktop_epoch: Option<String>`. It is a random 128-bit hex id for each connection. It is distinct from the existing `Hello.link_epoch: u64`.
   - **Ownership.** The foundation task owns this field. It also owns the propagation of the connection context to the desktop dispatch.
2. **Commit before wake, no eviction of live keys (round 3 finding 2; replaces the eviction rules of §5F item 4).**
   - **One operation.** The store has one operation: `wake_reserve(station_id, request_id, now_ms) -> Reserved | Seen | Suppressed | Error`. It runs in one transaction.
   - **Steps.** The operation does these steps in order:
     - (a) It checks the suppression state.
     - (b) It prunes the rows older than 24 h.
     - (c) It prunes the rows older than 120 s, oldest first, while the table holds 10 000 rows or more.
     - (d) It runs `INSERT ... ON CONFLICT DO NOTHING`.
     - (e) It commits.
   - **Wake rule.** Only `Reserved` (one row inserted and committed) may lead to a wake. `Seen`, `Suppressed`, a write error and an unknown outcome never wake.
   - **No live eviction.** A row younger than 120 s is never evicted. The 120 s is longer than the 60 s window plus the 5 s skew.
   - **Saturation.** When the table holds 10 000 rows that are all younger than 120 s, the operation sets a persisted suppression state: `wake_suspended_until = now + 120 s`. It is a deck settings row. The operation returns `Suppressed`. No wake is sent while the state is set, also after a restart.
   - **Baseline.** Baseline observations use the same operation, because they must block later wakes. They are bounded in the same way.
   - **Tests.**
     - A crash after the commit and before the dispatch. After the restart: no wake.
     - Two concurrent observations of one key. One result is `Reserved`.
     - A baseline overflow. Suppression starts, then the wake recovers after 120 s.
3. **Ownership (round 3 note).** The web completion task (P2c) also owns `web/e2e-real/**`. It needs these files for the production-bundle proof of §5F item 3.
4. **Status line.** The status line at the top of this document adds: "revision 9: §5G proposes answers to the round 3 findings; awaiting the owner (4th review round)".

5. **Names.** The authority field that §5F item 2 calls `link_epoch: String` is `desktop_epoch: String` (the value of the station's `Hello.desktop_epoch`); `Hello.link_epoch: u64` is unrelated. When §5G is approved, §11 adds: the foundation row owns `Hello.desktop_epoch` and the `wake_reserve` store operation with the `wake_suspended_until` settings row; the P3b wake row owns its use; the web completion row owns `web/e2e-real/**`.


## 5H. Build record of P2b/P3b (DL-231)

This section records what the build of §11 made. It adds no decision. The full text of each decision is in the architect's brief folder: `r13-p23b-decisions.md` (R1-R17) and the worker proposals `r13-*-round1-questions.md`. The range on v2 is `cccaf3d0..5a01c9ca`.

**Step 1: foundation (R1, merge `2c558589`).** The task made the deck setting `desktop_control` (`crates/fleetdeck-core/src/config_deck.rs`, `settings/fields.rs`), the proto contracts (`crates/fleetdeck-proto/src/desktop_authority.rs`, `display_directive.rs`, `display_changes.rs`, `display_event.rs`), the link handshake field and the authority hash (`crates/fleetdeck-link/src/hello.rs`, `desktop_authority.rs`), the V25 store (`crates/fleetdeck-deck/src/store/display_v25.rs`, `displays_binding.rs`, `displays_auth.rs`, `wake_seen.rs`), the `/display` document (`http/display_document.rs`) and the stub routes (`http/not_built.rs`). R1 fixed these points:
- The handshake field is `Hello.desktop_epoch` (§5G), not `link_epoch`.
- The action hash binds the current node fingerprint of the station.
- `quiet_hours` is a field of `DesktopAuthority`. The `desktop.act` row has the feature `desktop_authority.v1`.
- The `/display` route allows `frame-src https://<allowed request host>/api/displays/self/artifact/`, else `'none'`.
- Every unbuilt stub answers `501 not_built` or a link refusal code.
- Migration order: V25 (displays, directives, wake) keeps its list position on v2. At the merge of v2 into `fleet-git`, the Fleet Git deck migrations move after it.

**Step 2, P2b directives (R2, merge `fd0acab4`).** Files: `crates/fleetdeck-deck/src/store/display_directives.rs`, `display_directives_check.rs`, `http/display_directive_routes.rs`. The owner route `POST /api/displays/{id}/directive` takes `{"directive", "refs"}` (8 KiB limit). The display route `GET /api/displays/self/directive` is a pure read: a failed live re-check reads `mission` with the stored generation and deletes nothing (R2 Q2). One transaction bumps `directive_generation`. The station and coordinator senders return `not_granted` (DL-222). The audit rows hold identifiers only.

**Step 2, P3b station dispatch (R3, R4, merge `4211e779`).** Files: `crates/fleetdeck-station/src/desktop/authority.rs`, `deadline.rs`, `quiet.rs`, `link/intake_desktop.rs`. `admit(control, station_node, authority, action)` returns an `Admitted` value that `Desktop::act` needs. A process-global registry keeps every consumed `action_id` for the connection epoch (4 096 at most; a full registry gives `failed` and closes the link). R4 changed the order of the start check: the hash, then the quiet hours (an async `date` read), then the connection, the wall deadline and the 10 s monotonic limit in one sync step. §6D item 3 wins over the advisor: equal endpoints mean no quiet hours, and an unknown offset fails closed. The offset comes from a strict parse of `date +%z`.

**Step 2, P3b relay (R5, R6, merge `19a63512`, fix `bae32a2d`).** Files: `crates/fleetdeck-deck/src/desktop_relay.rs`, `http/desktop_routes.rs`, `link/desktop_authority.rs`, and `DeckStore::update_station_desktop` in `store/mod.rs` (R5 Q11 B). The owner path is `act_body`; the public `act` refuses the wall actions. A wall open sends the pair page `{origin}/display#/display-pair?expect=<id>&op=<rev>` (R5 Q4) and commits the binding after the station answers. A second wall action of one display gets `Conflict` (a per-display `try_lock`). A deck with no reachable board address gets `409 desktop_no_endpoint`. R6 added one process-global wall lock for open, reload and close. Fix `bae32a2d`: a successful `wall_close` clears the binding only when the display has no binding or a binding on the target station.

**Step 2, P3b wake (R7, R8, merge `7d5077f2`).** Files: `crates/fleetdeck-deck/src/wake/mod.rs`, `scan.rs`, `job.rs`, `quiet.rs`. The task scans the cards on each `watch_board()` change. A scan is baseline mode when the `snapshot_mark` of the station changed since the last scan. Only a request that is at most 60 s old (5 s future skew) and has the key `Reserved` wakes. There is one job for each `(station, monitor)`, 4 workers and a queue of 32 (a full queue drops and counts the job). R8 added `skip_raced`: the worker drops a job when the mark differs from the mark of its scan. The 30 s cooldown started at the enqueue; R17 F4 moved it to the send.

**Step 2, P2b payload (R9, R10, merge `f49b8f3f`).** Files: `crates/fleetdeck-deck/src/store/display_timeline.rs`, `display_activity.rs`, `http/ws_display.rs`, `display_view.rs`. One projection serves every path (R9 Q4). A display subscription has no Presence, Driver or Chat frame. The directive filter narrows cards, stations and sessions, and a 2 s re-check sends a delta on a scope change. The extras are all zeros. The resources use an allowlist. A request title is a fixed label. R10: `DisplayBatch.high_water` is an `Option`, and a revoked display gets an empty view until the re-check closes the socket with 4401.

**Step 2, P2b changes (R11, R12, merge `1d9e5247`).** Files: `crates/fleetdeck-station/src/workspace/display_changes.rs`, `crates/fleetdeck-deck/src/http/display_changes.rs`. The route is `GET /api/displays/self/sessions/{station}/{session}/changes`. The station always applies the member deny list. One anonymous marker stands for each hidden section, and a private path has no marker. The station caps a file at 64 KiB and the deck caps the answer at 1 MiB. Patches need `show_diffs`. Identical requests join one station call. One display may have one outstanding computation, else `429 busy`. Every miss of a session gives the same `404 unknown_session`.

**Step 2, P2b artifacts (R13, R14, merge `97f36a13`).** Files: `crates/fleetdeck-deck/src/http/display_artifact.rs`, `artifact_common.rs`, `artifact_stream.rs`. The route serves one file of one version. Every miss before the station work is one `404 unknown_artifact`. The answer is inert: the CSP `INERT_CSP` has `sandbox` with no `allow-scripts`, and the headers are `nosniff`, `no-referrer` and `no-store`. The directive, the scope and the login are checked again after every await and before each streamed chunk (R13 Q7, R14). R15 Q1 A (built on v2 as `2903d206`): `{*path}` = `_` names the one file, and the content type comes from the real file name.

**Step 3, web completion (R15, R16, merge `76cff37b`, fix `04a61495`).** Files: `web/src/display/**` (the display runtime, the pair page, the directive poll), `web/src/displays/**` (the Displays tab, Send to wall data), `web/src/desktop/**` (the Desktop panel), `web/src/wall/**` (the artifact pane, change summaries, Send to wall). The wall renders each artifact kind inert (R15 Q2): HTML in `<iframe sandbox="">`, SVG and images in `<img>`, Markdown read with a 1 MiB cap and drawn with `react-markdown` and `remark-gfm` (`skipHtml`; links and images as plain text), and PDF or any other kind as the card "open it on your own device". The pending row of a pairing never shows the code: the owner types the code that the wall shows (R15 Q9). Send to wall pins by default (R15 Q11). R16: `WallPage` keeps `owner` and takes an optional `display?: DisplayEnv`. Fix `04a61495`: the Displays page keeps the "Approved" status at the section level.

**PRE-PR fixes (R17, merge `5a01c9ca`).** Round 1 of the adversary failed (2 BLOCKER, 2 MAJOR). One round fixed all four:
- F1: `scope_of` checks the display session first (expiry, idle, revoke, auth generation). Every display route and every streamed artifact chunk re-checks the login.
- F2: every display projection reads the scope after its data reads, right before the filter.
- F3: a wall close clears only the binding that it saw (`display_clear_binding_at` with station and revision, one statement).
- F4: the 30 s wake cooldown is enforced at the send (`Shared.last_wake`).

**Proof status.**
- `make check` on `5a01c9ca`: PASS (3 953 Rust tests, 11 skipped; 1 660 web tests).
- Real e2e `web/e2e-real/display.spec.ts`: 4 of 4 after the fix `04a61495`. The first run had 2 passed; the pairing case failed on the missing "Approved" text and the revoke case did not run.
- Mock e2e `web/e2e/display.spec.ts`: 5 of 5. `web/e2e/wall.spec.ts`: 6 of 6.
- The first full `make check` on v2 (`04a61495`) failed in an environment race: the test `station_pairing invite_prints_an_invitation_that_pairs_the_deck_and_the_listener_survives_a_restart` got "Address already in use" on a station restart port (open item 24 of §14).
- Reviews (§13): PRE-PR round 1 FAIL (2 BLOCKER, 2 MAJOR, fixed in `5a01c9ca`), RE-REVIEW round 2 PASS. The review reports are `adv-r13-p23b-prepr-r1-report.md` and `adv-r13-p23b-prepr-r2-report.md` in the architect's brief folder.
- The live smoke on the owner's Omarchy machine (DL-201 asks for it): result not recorded.

**Open items.** They are items 7 to 24 of §14.

---

## 7. The desktop MCP for agents (HIGH RISK)

### 7.1 Classes

These are new `ToolClass` and `GrantClass` values:

| Class | Tools | Needs |
| --- | --- | --- |
| `desktop` | `desktop_info`, `wall_show`, `notify`, `preview_open`, `terminal_open`, `window_close` | `allow_desktop` |
| `capture` | `screenshot`, `record_demo` | `allow_desktop_capture` |
| `input` | `pointer_move`, `pointer_click`, `key_press`, `type_text`, `scroll` | `allow_desktop_input`, plus `capture` in the same grant (an agent that clicks must be able to see) |

*Inactive (revision 11): P5a uses only `screen_capture` (monitor, owner); P5b and P6 define their own names in their reviews.* The tools `screenshot` and `record_demo` of the `capture` class are historical.

### 7.2 Gates

- Every session can get these classes, not only a coordinator: the grant of a normal session can hold `desktop` or `capture`. `input` needs a coordinator grant or a session started with "Allow Computer Use" in the spawn form.
- Any of the three classes needs the owner's elevation at grant creation (DL-172). It also needs `desktop.v1` negotiated on the links of the caller's station and the target station.
- The deck switch `desktop_control` and the target station's `allow_desktop*` settings are checked live (§6.4).
- **Target desktop.** The default is the caller's own station. A grant field `desktops: [station]` names other desktops, for example an agent on the laptop that shows its preview on the server wall. A target outside the list is refused with `not_granted`.
- **Caps.** `desktop_cap` is 0 to 5000, with a default of 300 actions per grant over its lifetime. It is counted like `answer_cap` (ADR-008). `notify` is also limited to 6 per minute for each session.
- Each call is an audit row with the session, the action and the target, and never the screenshot content.

### 7.3 Tools

| Tool | Arguments | Wire |
| --- | --- | --- |
| `desktop_info` | — | `desktop.info` (monitors, walls, allowed) |
| `wall_show` | `what: artifact \| session \| preview`, `artifact_id?`, `url?`, `display?` | a display directive (§4.5). `preview` makes a `browser.open` on the bound monitor |
| `notify` | `title`, `body`, `urgency: low \| normal` (`critical` is for the owner only) | `notify`. The title gets the callsign as a prefix: `lynx-12: …` |
| `preview_open` | `url` (`http(s)://localhost`, `127.0.0.1` or the station's own tailnet name), `monitor?` | `browser.open` with `session` = the caller |
| `terminal_open` | — (the caller's own session only) | `terminal.open` |
| `window_close` | `address` (only a window that the caller opened) | `window.close` |
| `screenshot` | `target: window <address> \| monitor <name>` | `capture.screenshot`. The image goes back to the agent as MCP image content, and it is also kept as an artifact |
| `record_demo` | `target`, `seconds ≤ 120` | `capture.record`. The tool returns the artifact id |
| `pointer_* / key_press / type_text / scroll` | §7.4 | `input.*` |

*Inactive (revision 11): P5a uses only `screen_capture` (monitor, owner); P5b and P6 define their own names in their reviews.* The rows `screenshot` and `record_demo` (and the names `desktop_screenshot` and `desktop_record` of §6.5A item 11) are historical.

### 7.4 Computer Use: scoped input

DL-197 allows Computer Use. The design still gives input a scope, because of one concrete attack. An agent with input could click "Approve" in the owner's own board window on the laptop and approve its own dangerous tool call. That would remove supervision (rule 6), not only add a capability.

Rules:
- **`input_scope` per grant:** `own_windows` (default) or `any`.
  - `own_windows`: the target must be a window that the station opened for the caller session (§6.3 records `browser.open` and `terminal.open` per session). Before each injection, the station focuses that window and checks `hyprctl activewindow -j`. If the focused window is a different one, the action is refused with `focus_changed`.
  - `any`: any window. This needs the owner's elevation and a red warning in the spawn form: "This agent can click and type in every window, also the Fleet Deck board. It can approve its own requests."
- **Board windows are never targets.** The station refuses input when the active window's class or title matches the board (the Fleet Deck kiosk classes and a window whose URL host is the deck origin, as far as the title shows it). For `own_windows` this is a second barrier. For `any` it is the only barrier, and the warning states that.
- **Tools.**
  - Keys and text: `wtype` (Wayland virtual keyboard). If it is missing, the action is refused with `input_tool_missing`, and `doctor` shows it.
  - Pointer: `hyprctl dispatch movecursor` for the position. The click tool is chosen in the P7 spike (candidates: `ydotool` with `ydotoold`, `wlrctl`). Until then, `pointer_click` is `unsupported`.
- `type_text` takes up to 2,000 characters and `key_press` takes one chord. The audit row records the action kind and the length, not the text.

### 7.5 Tests (four proofs)

- **Exposure:**
  - Each class without elevation is refused (stored rejection).
  - A target outside `desktops` is refused.
  - Input on a board window is refused.
  - Input with `own_windows` on another window is refused.
  - A focus change between the focus step and the injection is refused.
  - Audit rows hold no screenshot bytes and no typed text.
- **Fail-open:** a switch that turns off refuses the next call. Sessions without desktop classes see no change.
- **Determinism:** the tool list is fixed at process start (ADR-006 §5.3), and calls fail while a switch is off.
- **Version:** a station without `desktop.v1` never lists the tools. The grant is refused with `unsupported` at creation.

---

## 8. Omarchy integration: `fleetdeck omarchy install` (normal risk)

### 8.1 The installer

`fleetdeck omarchy install [--yes] [--screensaver]` runs on a station machine with Omarchy (§6.1 detection). It prints the list of files and asks for consent, unless `--yes` is given. It writes only these:

| File | Contents |
| --- | --- |
| `~/.config/omarchy/plugins/fleetdeck.fleet/` | the QML plugin (§8.3). The files are embedded in the binary |
| `~/.config/omarchy/hooks/post-boot.d/fleetdeck` | §8.6 |
| `~/.config/omarchy/hooks/post-update.d/fleetdeck` | §8.6 |
| `~/.config/omarchy/hooks/theme-set.d/fleetdeck` | §8.5 |
| `~/.config/omarchy/themed/fleetdeck.json.tpl` | §8.5 |
| `~/.config/fleetdeck/omarchy/hypr.lua` | window rules for `fleetdeck-wall` and `fleetdeck-preview`, and the hotkeys of §8.4 |
| `~/.config/omarchy/extensions/omarchy-menu.jsonc` | only if the file does not exist. Otherwise the installer prints the rows to add |
| `~/.config/fleetdeck/omarchy-install.json` | the installed files, each with its SHA-256 |

Then:
- The installer runs `omarchy-shell shell rescanPlugins` and `omarchy plugin enable fleetdeck.fleet --section right`.
- It prints one line for the owner to add to `~/.config/hypr/hyprland.lua`: `dofile(os.getenv("HOME") .. "/.config/fleetdeck/omarchy/hypr.lua")`. It never edits the owner's Hyprland files.
- It turns on the station setting `fleet_summary` (§8.2).

`fleetdeck omarchy uninstall` removes each file whose hash is still the installed hash. It keeps a changed file and names it. It also disables the plugin and restores the screensaver backup (§8.7). `fleetdeck omarchy status` prints what is installed and what changed. `doctor` adds an `omarchy` line.

### 8.2 The fleet summary and `fleetdeck desktop feed`

A plugin on the laptop must see the whole fleet without a new credential on disk. The station is already authenticated to the deck, so the summary comes over the link:
- A new link push, `fleet.summary` (feature `desktop.v1`). The deck sends it to each station whose setting `fleet_summary` is on (default `false`), debounced to at most one push every 2 s.
- **Contents (no chat text, no file content):**

```json
{"v":1,"at":"2026-10-16T12:04:00Z",
 "counts":{"working":7,"idle":3,"needs_you":2,"blocked":1,"lost":0},
 "needs_you":[{"station":"srv","session":"…","callsign":"lynx-12","title":"Fix cart","kind":"approval","tool":"Bash","age_s":95}],
 "usage":{"label":"claude A · 5 h","pct":63,"resets_in_s":4320},
 "stations":[{"name":"srv","up":true,"cpu_load_pct":41},{"name":"laptop","up":true,"cpu_load_pct":null}],
 "deck":"up"}
```

- `needs_you` holds at most 10 items. `usage` holds the tightest window, or `null`.
- `fleetdeck desktop feed` connects to the local station socket (0600, peer UID check) and prints one JSON line on each change and one heartbeat line every 30 s. If the station stops, it prints `{"v":1,"deck":"unknown"}` and tries again with backoff.
- `fleetdeck desktop feed --notify` also sends one notification for each new needs-you item (`-u normal`, the title "lynx-12 needs you", the body "Bash · Fix cart"). The click action runs `fleetdeck desktop open <station>/<session>`, which opens the board URL of that session with `xdg-open`. **The notification never answers a request.**

### 8.3 The shell plugin `fleetdeck.fleet`

The manifest declares the kinds `service`, `bar-widget` and `panel`.
- **Service:** it runs `fleetdeck desktop feed --notify` with Quickshell `Process`, parses the lines, and restarts the process if it ends.
- **Bar widget:** a glyph and `7 · ⚠2`. It uses the urgent color when `needs_you > 0` and a dim color when the deck is `down` or `unknown`. Left click opens the panel, right click opens the board (`fleetdeck desktop open`), and middle click shows the wall workspace.
- **Panel:** the needs-you list, then the counts per station and the usage line.
  - `j/k` and the arrows move, `Enter` opens the item on the board, and `w` sends "focus on the wall" (§8.4). `Esc` closes.
  - The panel has no approval control.
- **IPC:** `omarchy-shell fleetdeck.fleet <open|close|toggle|refresh>`.
- It uses `import qs.Commons as Commons`, as Omarchy requires, so it gets the theme colors.

### 8.4 Hotkeys and wall navigation

`hypr.lua` (§8.1) binds:

| Keys | Action |
| --- | --- |
| `Super+Ctrl+F` | toggle the `fleetdeck.fleet` panel |
| `Super+Alt+W` | show or hide the wall workspace on its monitor |
| `Super+Alt+N` | `fleetdeck desktop wall next`: focus the next needs-you item on the bound wall |
| `Super+Alt+M` | `fleetdeck desktop wall mode <next>` |

- `fleetdeck desktop wall …` goes to the local station, which sends `display.directive` for a display bound to it (§4.5). It is the only write in channel C. It can only change what a wall shows.
- On the laptop (no bound wall), `wall next` names the server's wall, and the deck refuses it with `not_bound`. The laptop uses the board instead.

### 8.5 Theme sync

- `fleetdeck.json.tpl` maps the Omarchy colors (`{{ background }}`, `{{ foreground }}`, `{{ accent }}`, `{{ color0..15 }}`) to the board's CSS variables. Omarchy renders it to `~/.local/state/omarchy/current/theme/fleetdeck.json` on every theme switch.
- The `theme-set` hook runs `fleetdeck omarchy theme-sync`. The command checks the file the same way `omarchy-theme-set-t3code` does (no `{{` left), and sends the palette to the deck over the link (`station.palette`, feature `desktop.v1`).
- The deck keeps one palette for each station. A board device can choose "Use the Omarchy theme of <station>" under Prefs, and a display uses the palette of its bound station.

### 8.6 Hooks

- **`post-boot`:**
  - If the station service is installed and not running, run `fleetdeck service start`.
  - Then run `fleetdeck desktop wall restore`: the station opens again the walls that were bound to it before the reboot, with a new one-time link for each (§6.3).
  - Then run the one-time join of §9 if its file is there.
- **`post-update`:** run `fleetdeck doctor --brief`. On a `fail` line, send a notification "Fleet Deck: doctor found a problem after the Omarchy update".

### 8.7 Ambient screensaver (opt-in)

`--screensaver` turns on a station job:
- At the first write, the job copies `~/.config/omarchy/branding/screensaver.txt` to `screensaver.txt.fleetdeck-backup`.
- Every 60 s, it writes a small ASCII fleet status: the counts, the next needs-you item and the tightest usage. It uses `omarchy ascii` for the large digits when that command is there.
- Uninstall restores the backup.

### 8.8 Omarchy accounts → provider instances

`fleetdeck omarchy accounts` reads the Omarchy account registry (`~/.local/state/omarchy/agents/accounts/{claude,codex}.json`; the P8 brief reads its format from `bin/omarchy-agent-account-state`). It proposes one provider instance for each account, with `config_home` = the account home and the label = the account name. The owner confirms each one on the board (station settings › providers). It only reads Omarchy files.

### 8.9 Crash diagnosis in the fleet

The menu gets a row "Fleet › Diagnose the last crash". It runs `fleetdeck desktop crash --last`:
1. Read the last core dump from `coredumpctl list --json=short`.
2. Build the same facts as `bin/omarchy-agent-crash`.
3. Start a **supervised** session on the local station (`fleetdeck station run`), in `~`, with the prompt that names the `diagnose-crash` skill.

The session shows on the board like any other.

### 8.10 Upstream changes that would help (owner decision, not in this repository)

1. Add `fleetdeck` to the default-agent list in `bin/omarchy-default-agent`. Then `Super+Shift+Ctrl+A`, the "Make something" prompts and the crash click start a supervised Fleet Deck session.
2. Ship a built-in `fleetdeck.json.tpl` and a `omarchy-theme-set-fleetdeck` script, the same as T3 Code has.
3. List the plugin on plugins.omarchy.org.

### 8.11 Tests

- **Installer:** install, uninstall and install again on a fake `$HOME` with a fake `omarchy` and `omarchy-shell` on PATH. A changed file is kept. An existing `omarchy-menu.jsonc` is never written. `status` reports drift.
- **Feed:** the summary fixture prints the expected lines. With the station down, the feed prints `unknown`. With a new needs-you item, the feed sends one notification, which is checked with a fake `omarchy-notification-send` that records its argv.
- **Plugin:** `omarchy plugin validate` passes on the embedded plugin (owner machine smoke). A QML unit test is out of scope.
- **Exposure:** the summary holds no chat text and no file content (a schema test).

### 8A Revision 12: P4 rules

The detailed contracts of P4 are in the annex [omarchy-p4.md](./omarchy-p4.md) (proposed, revision 12; decision P4-1). **`omarchy-p4.md` overrides §8 where they differ.** The main cases:

- **D1, feature:** `fleet.summary`, `display.directive` and `station.palette` use the new feature `desktop_shell.v1`, not `desktop.v1` (§8.2, §8.4, §8.5). No new link scope.
- **D3, wall restore (§8.6):** `fleetdeck desktop wall restore` is not built. After §5A item 7 and §6D item 1, a wall needs a deck-authorized `wall.open` and a pairing code, so a station cannot reopen it alone. The owner reopens the wall from the board.
- **D4, the join (§8.6):** the `post-boot` hook does not run the join of §9. §9A and [omarchy-p8.md](./omarchy-p8.md) replaced it. The hook starts the service and runs the theme sync.
- **D5, notifications (§8.2, §5C item 4):** the feed uses `omarchy-notification-send` with fixed words and checked ids. It sends D-Bus calls with `busctl`; only the headline position can be read as an option. The exact argv is in the annex.
- **D6, Hyprland rules (§8.1, §8.4):** the rules match the unique classes `fleetdeck-wall-<id>` and `fleetdeck-preview-<id>` by prefix, in the Lua syntax `o.window` and `o.bind` of Omarchy.
- **Plugin (§8.3):** kinds `bar-widget` and `panel`; the panel opens with `omarchy-shell shell toggle fleetdeck.fleet '{}'` or the binding `{ panel = "fleetdeck.fleet" }`. The IPC target `fleetdeck.fleet` is dropped.
- **Hotkeys (§8.4):** `wall show` runs `hyprctl` in the CLI. `wall next` and `wall mode next` use the new station-to-deck method `display.directive`, and the `Station` sender reaches only the sessions of its own station.
- **Theme (§8.5):** the template and the palette use the keys that the Omarchy template proves (`mode`, `background`, `foreground`, `accent`, `muted`, `dark_background`, `dark_foreground`, `bright_foreground`, `selection`, `red`, `yellow`), not `color0..15`.
- **Screensaver (§8.7):** `omarchy ascii` draws letters only. The job draws digits with its own table and runs as a process of the plugin, not as a station job.
- **Doctor (§12 item 2):** the passwordless sudo check runs `omarchy-sudo-passwordless --active`.

---

## 9. Omarchy stations with no manual steps

The flow uses Omarchy's unattended install (§1.1):
1. The owner runs `fleetdeck deck invite --for-provisioning <name> --ttl 24h`. The command prints a single-use station invitation.
2. The `cidata` drive holds the Omarchy files (`user_configuration.json`, `user_credentials.json`, `authorized_keys`, `tailscale_authkey`), plus a `fleetdeck/` folder with the invitation and a first-boot script. The way to get the script and the `fleetdeck` binary onto the new machine is an open item (§14.3).
3. At the first login, the `post-boot` hook finds `~/.config/fleetdeck/join.invite`. It runs `fleetdeck station join --invite-file …` and deletes the file. Then it runs `fleetdeck service install` and `fleetdeck omarchy install --yes`.
   - The hook runs in the graphical session, so its `PATH` has the mise stubs. This matters, because `service install` writes the PATH of the shell that runs it.
4. The board shows the new station as **Link up**.

**Caution:** the `cidata` drive holds the LUKS passphrase, the Tailscale key and the invitation. Treat the drive as a secret, and keep the invitation TTL short. Each invitation works once.

The same flow works for Omarchy VMs on the server (Proxmox `qm create` with a `cidata` ISO). Each VM is its own station with its own workspaces.

## 9A. Revision 10: provisioning rules (owner decisions DL-230 items 3 and 4; answers the DESIGN REVIEW of §9)

These rules replace §9 where they differ and answer the findings B1-B6 of the P5/P8 DESIGN REVIEW round 1.

1. **Enrollment by fingerprint approval (B1, owner decision DL-230 item 3).** The provisioning medium holds **no Fleet Deck secret**. A new machine pairs with the existing station-initiated pairing (DL-178, `pair_request.v1`): `fleetdeck station join --tailnet <deck> --deck-fingerprint <hex> --name <name>`. The station shows its own key fingerprint at first login (a desktop notification and the new command `fleetdeck station fingerprint`, which prints the link key fingerprint of the running station: "Approve this fingerprint on the board: <first 16 hex> …"). The owner opens the pairing window on the board (elevation, 10 minutes) and approves the exact fingerprint that the machine shows. A copied medium gives an attacker nothing: the attacker's own key has another fingerprint, and the owner approves only the fingerprint on the new machine's screen.
2. **What the medium holds.** `fleetdeck/provision.toml` (0600): `deck = "<host>[:<port>]"`, `deck_fingerprint = "<64 hex>"`, `name = "<station name>"`. `fleetdeck deck provision-file <name> --out <path>` writes it (refuses an existing file, creates it 0600, prints nothing secret). The Omarchy secrets on the same medium (LUKS passphrase, Tailscale key) are outside Fleet Deck; the operations guide says: detach the ISO after install, and rotate the Tailscale key if the medium leaves your control.
3. **Bootstrap order (B2).** The first-login hook runs, and stops at the first failure:
   1. install the package (item 6) if `fleetdeck` is missing;
   2. `fleetdeck service install station` (the station service only, not `both`);
   3. wait at most 60 s for the station socket (`fleetdeck station fingerprint` exits 0 once the station answers on its socket);
   4. `fleetdeck station join --tailnet <deck> --deck-fingerprint <hex> --name <name>` (it keeps its retry file and asks again until the owner approves; the existing retry file rules apply);
   5. after the link is up: `fleetdeck omarchy install --yes`.
   Every step is idempotent: a rerun skips a done step (`service install` of an installed service, a station already paired with this deck). A station paired with another deck stops with the existing `up.rs` refusal text.
4. **Recovery (B3).** There is no invitation to consume, so there is no half-redeemed state. A request that the owner never approves expires with the window; the retry file asks again. The owner revokes a wrong station with the existing station removal on the board.
5. **CLI (B4).** Two commands are new: `fleetdeck station fingerprint` (item 1) and `fleetdeck deck provision-file <name> --out <path>` (name: the station name grammar; no TTL, because the file holds no secret). The deck keeps no provisioning record. The audit row of the pairing approval is the existing one.
6. **Distribution (B6, owner decision DL-230 item 4).** AUR package `fleetdeck-bin` (the release tarball with its SHA-256 in the PKGBUILD) and a Homebrew tap formula. The hook installs from AUR with the package manager's checks; the service PATH rules of `service install` apply unchanged, and `fleetdeck doctor` checks that the provider binaries run under the installed service environment. The release and package build is a separate task of the owner's release (it needs the owner's release keys); P8 code only uses the installed binary.
7. **Tests.** A clean home through all steps with a fake deck; each interruption point and a rerun; a second machine with a copied medium gets a different fingerprint and is not approved; `provision-file` refuses an existing file and writes 0600.

## 9B. Revision 11: provisioning contracts

The detailed contracts of P8 are in the annex [omarchy-p8.md](./omarchy-p8.md). **`omarchy-p8.md` overrides §9A where they differ.**

- The annex gives the fingerprint format (B1), the two commands with their local protocol and file rules (B4), the hidden `provision-bootstrap` with its durable state and retry table (B2), the custody procedure (B5), and the package and hook trust (B6).
- The annex corrects the B3 claim of §9A: a remotely committed, locally uncommitted pairing state does exist, and `pair-pending.json` handles it.
- The custody procedure is in [operations.md](../operations.md) section 13 "Provisioning media custody (P8)". P8 acceptance requires it.
- Production P8 acceptance waits for the release task of the owner (§14).

---

## 10. Per-session load (the 64-core view)

- Each station adds `resources { cpu_pct, rss_bytes, procs }` to the summary of each live session. It gets the values by adding up `/proc/<pid>/stat` and `statm` over the session's process tree: the provider child or the shim, and all of their descendants. It does this every 10 s.
- `cpu_pct` is relative to one core, so 6400% is the whole 64-core machine.
- The wall's `side` area shows a heatmap: one cell per session, sized by `cpu_pct`, with a tooltip for RSS.
- The station card shows the top 5 sessions.
- A value that the station cannot read is `None` ("unknown").

Tests: a fixture `/proc` tree gives the expected sums. A session that ends drops out at the next sample.

---

## 11. Phases

| Phase | Contents | Risk and review | Depends on |
| --- | --- | --- | --- |
| **P1 Wall route** | §4 with an owner login (no display principal yet): shapes, areas, modes, URL state, auto-focus, QR, a read-only tile form | Normal: one Codex review | — |
| **P2 Display principal** | §5: principal, `.display()` rows, cookie, links, the Displays list, the station filter, directives (owner and coordinator) | **High:** adversary DESIGN REVIEW of §5 and §4.5, then PRE-PR | P1 |
| **P3 Desktop control** | §6.1 to §6.4 and §6.6: detection, `StationInfo.desktop`, `desktop.act` (screen, workspace, wall, notify, browser, terminal, window), settings, the wake rule, the board Desktop panel on the station page, "Send to wall" on a session and on an artifact | **High:** DESIGN REVIEW of §6, then PRE-PR | P2 (`wall.open`) |
| **P4 Omarchy integration** | §8: `fleet.summary`, `desktop feed`, plugin, installer, hooks, hotkeys, theme sync, screensaver, crash row | Normal: one Codex review | P3 (feature `desktop.v1`) |
| **P5a Screenshots and deletion** | §6.5B and [omarchy-p5a.md](./omarchy-p5a.md): the owner `screen_capture` action, the capture result lookup, the owner deletion of any artifact | **High:** DESIGN REVIEW, then PRE-PR | P3 |
| **P5b Recordings** | recordings, the `video` artifact kind, `Range` playback, the recording stop (§6.5A items 5, 6, 7, 9, 10, 11 as historical input) | **High:** own DESIGN REVIEW later | P5a |
| **P6 Desktop MCP** | §7.1 to §7.3: classes `desktop` and `capture`, caps, `desktops` | **High:** DESIGN REVIEW, then PRE-PR | P3, P5a |
| **P7 Computer Use** | spike: the pointer tool, `wtype` on Omarchy, and how reliable the focus check is. Then §7.4 | **High:** DESIGN REVIEW after the spike, then PRE-PR | P6 |
| **P8 Accounts and provisioning** | §8.8 and §9 | Normal for §8.8. **High** for §9 (an invitation in a drive): DESIGN REVIEW | P4 |
| **P9 Per-session load** | §10 | Normal: one Codex review | — (in parallel) |

Each phase starts with a DL entry. Each phase adds its rows to `PARITY.md` (the four proofs) and its new surfaces to `SECURITY-CHECKLIST.md` for the owner's final review (DL-158). P1 and P9 can start now and in parallel, because their files do not overlap.

### P2b/P3b build plan

The order is: a serial foundation task, then the parallel tasks, then the web completion, then the documentation. The foundation owner supplies the shared helper interfaces before the parallel wave and does the final wiring after it. Each worker owns its new tests. No worker edits a shared fixture during the parallel wave. Each changed source module stays below 800 lines. Targeted checks run through the heavy-command queue. The coordinator runs the full `make check`. The coordinator sender moves from P2 to P6 (§5D item 2), so the P2 row above reads "directives (owner sender; the station and coordinator senders return `not_granted` until P4 and P6)".

| Step | Task | Exclusive owned files | Deliverable |
|---|---|---|---|
| 1 (serial) | Foundation and integration | Core config, settings and tests; deck `live.rs`, `cert_task_tests.rs`, `tests/common/board.rs`; proto contracts, exports and TS generation; link method tables; deck HTTP module, method tables and guard; store `mod.rs`, `schema.rs`, `displays.rs`, `displays_pairing.rs`; HTTP `display_routes.rs`; station intake routing; deck `lib.rs`, `serve.rs` | Settings, contracts (including the `desktop_authority.v1` feature and the `desktop.act` authority param, §5E item 1), migrations (including `displays.auth_generation`, §5E item 2), binding and pairing transactions, the `/display` document route (§5E item 3 as changed by §5F item 3), the `link_epoch` handshake field (§5F item 2), the `wake_seen` table (§5F item 4), invalidation hooks and the final wiring |
| 2 (parallel) | P2b payload | Deck `http/{display_view,ws,read_routes,activity_routes}.rs`, new display projection helpers, `store/activity.rs`; new timeline and activity tests | One projection policy across HTTP and every WS path |
| 2 (parallel) | P2b directives | New deck `store/display_directives*.rs`, `http/display_directive_routes.rs`; new directive tests | Validation, storage, live sender checks and the polling endpoint |
| 2 (parallel) | P2b changes | Station `workspace/session_diff.rs` and new display-change helpers and tests; new deck `http/display_changes.rs` and tests | Station-produced protected statistics and patches, and a bounded display route |
| 2 (parallel) | P2b artifacts | Deck `http/{artifact_routes,artifact_stream}.rs`, new shared artifact helpers and `display_artifact.rs`; new artifact tests | The nearly full module is factored; single-file route, CSP and revocation |
| 2 (parallel) | P3b station dispatch | Station `desktop/**`, `link/intake_desktop.rs`, new station authority helper and tests | Fresh info, local offset, IPv6 grammar, total deadlines, the station side of the dispatch authority (§5E item 1 with §5F items 1 and 2: clock contract, single use, link epoch) and the station quiet-hours re-check (§5E item 5) |
| 2 (parallel) | P3b relay | New deck `desktop_relay.rs`, `http/desktop_routes.rs`, `link/desktop_authority.rs`; new relay tests | Owner relay, action authority, binding orchestration and redacted outcomes |
| 2 (parallel) | P3b wake | New deck `wake/**`; new wake tests | Request detection, restart-safe deduplication (§5F item 4), quiet hours, joining and bounded execution |
| 3 | Web completion (P2c, P3c) | `web/src/**`, without the generated types that step 1 owns | Pairing, display management, wall panes, the Desktop panel and "Send to wall" |
| 4 | Documentation | Design, decisions log, `PARITY.md` and `SECURITY-CHECKLIST.md` only | The revision, the worker briefs, and a truthful proof and review status |

All rows are built (DL-231, §5H). The worker briefs and the decisions are in the architect's brief folder (`r13-p23b-decisions.md`, R1-R17).

---

## 12. Security notes

1. **Same UID (owner answer 4).** On the server, agents run as the owner. ADR-006 §3 already accepts that a same-UID process controls the station. For this design, that means:
   - **The kiosk profile on the server** holds the display cookie, and an agent can read it. The result is **read** access to the stations in the display's filter, from any machine that reaches the deck. This is new, because without a wall an agent on the server cannot read the laptop's sessions through the deck. Mitigation: the station filter (§5.2), the revoke button, and short idle limits. The owner accepts this with answer 2 (read-only).
   - **The desktop of the server** (the Hyprland socket, `omarchy-shell`, `wtype`) is reachable by any agent, with or without the MCP. The MCP classes add audit, caps and kill switches. They do not add a boundary against a same-UID agent that calls `hyprctl` directly.
   - **The laptop:** an agent that runs on the laptop as the owner can drive the laptop's own desktop, including the owner's board window. §7.4 blocks this for the MCP. A direct `wtype` call is outside Fleet Deck's control.
   - A separate OS user for agents stays the only real boundary. It is a later owner decision.
2. **Omarchy's passwordless sudo** (`omarchy-sudo-passwordless`) gives root to every same-UID process for its window. `fleetdeck doctor` shows a `warn` line while it is on (it checks the sudoers drop-in that the command writes; the P3 brief names the file).
3. **`omarchy agent prompt`** runs agents unattended and auto-approved. Fleet Deck never uses it. §8.9 and §8.10 replace it with supervised sessions.
4. **Third-party text on the desktop.** Notification text and wall text from agents are plain text. Markup is removed, and the session callsign is a prefix. This follows rule 6.
5. **Screenshots** can show secrets from other windows (a password manager, mail). `capture_monitors` limits the monitors, and the owner accepted the remaining risk (owner answer 3).

---

## 13. Reviews

| Part | Review | When |
| --- | --- | --- |
| §5 display principal, §4.5 directives | adversary DESIGN REVIEW | before P2 code |
| §6 desktop control (`wall.open` login path, actions, settings) | adversary DESIGN REVIEW | before P3 code |
| §6.5 capture, §7 MCP classes | adversary DESIGN REVIEW | before P5 and P6 code |
| §7.4 Computer Use | adversary DESIGN REVIEW after the P7 spike | before P7 code |
| §9 provisioning | adversary DESIGN REVIEW | before P8 §9 code |
| P5a, P8 (revision 11) | adversary DESIGN REVIEW round 3 (last) | before P5a and P8 code |
| P1, P4, P9 | one Codex review of the final branch | before merge |
| P2b/P3b code | adversary PRE-PR | round 1 FAIL (2 BLOCKER, 2 MAJOR, fixed `5a01c9ca`), round 2 PASS |

---

## 14. Open items

1. **The pointer click tool** on Omarchy (P7 spike): `ydotool` needs a root daemon and `uinput`, and `wlrctl` depends on wlroots protocols that Hyprland supports.
2. **The format of the Omarchy account registry** (P8 brief, from `bin/omarchy-agent-account-state`).
3. **Distribution of `fleetdeck` on Arch** (owner): answered: AUR and Homebrew packages (DL-230 item 4). §9 uses them.
4. **The Omarchy lock IPC target name** and the sudoers drop-in path (P3 brief, from the Omarchy source).
5. **Upstream changes** of §8.10 (owner).
6. **A Fleet Deck record for the Omarchy agents panel.** The panel reads any record in its usage folder, so Fleet Deck could add the fleet-wide usage of all stations. The record contract is in the `claude` and `codex` collectors. This is not planned yet.
7. R6: the unit variants of `DesktopActBody` (for example `screen_lock`) accept unknown fields (serde internally tagged unit variants). Harmless; open.
8. R8: a wake scan reads every card on each board change (no cache, no debounce).
9. R8: a request that the cooldown or a full queue skips stays reserved and never wakes later (accepted, fail closed).
10. R10: the 3-try scope check (`409 scope_changed`) and the lag catch-up path have no direct test.
11. R10: a scope change between the send-time check and the socket write is accepted.
12. R12: the join map and the busy map of the display change route are process-global (keyed by the board address).
13. R12: the display change route has no server-side 5 s limit and no cache.
14. R12: `summary.files_changed` can be lower than `files.len()` by the number of hidden markers.
15. R14: no per-display limit on cache fills of the display artifact route.
16. R14: bytes that the deck already sent cannot be recalled.
17. R14: the `sandbox` CSP stops the PDF viewer in a frame and Markdown may download. R15 Q2 decided the wall rendering: PDF and other kinds show the card, Markdown is drawn as text.
18. R15: `GET /api/displays` returns `PendingPairing.code` to the owner board. This weakens the proof "the owner types the code from the wall". Consider removing the field.
19. R15, R16: `DisplayView` has no last directive, so the Displays list cannot show it (§5.2 asks for it).
20. R15, R16: with the owner cookie and the display cookie in one browser, the owner wins on `/api/ws`. The display page shows "This page is for a wall display. Open the board at /." for an owner answer.
21. R15: there is no `fleetdeck deck display link` CLI command.
22. R16: built (P9): the wall `side` area shows the per-session load heatmap (§10).
23. R16: the CSP and the sandboxed frame are proved only in the real e2e (`web/e2e-real/display.spec.ts`).
24. R17: the test `station_pairing invite_prints_an_invitation_that_pairs_the_deck_and_the_listener_survives_a_restart` failed once with "Address already in use" on a station restart port (an environment race; fail-fast stopped the lane at 1080 of 3943). The full check on `5a01c9ca` passed.
25. P8 production acceptance waits for the owner's release task (pinned `fleetdeck-bin` package, trusted manifest, installed hooks).
