# Fleet Deck on Omarchy: the wall, desktop control and the desktop MCP (R13)

- **Status:** Draft, revision 2 (2026-10-16): §5A answers the DESIGN REVIEW round 1 of §4.5, §5 and §6 (`/tmp/adv-r13-p2-design-report.md`) and overrides them where they differ. Not frozen. Each high-risk part needs an adversary DESIGN REVIEW before its code (§13).
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

## 5A. Revision 2: binding rules after the design review of §4.5, §5 and §6

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

---

## 7. The desktop MCP for agents (HIGH RISK)

### 7.1 Classes

These are new `ToolClass` and `GrantClass` values:

| Class | Tools | Needs |
| --- | --- | --- |
| `desktop` | `desktop_info`, `wall_show`, `notify`, `preview_open`, `terminal_open`, `window_close` | `allow_desktop` |
| `capture` | `screenshot`, `record_demo` | `allow_desktop_capture` |
| `input` | `pointer_move`, `pointer_click`, `key_press`, `type_text`, `scroll` | `allow_desktop_input`, plus `capture` in the same grant (an agent that clicks must be able to see) |

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
| **P5 Capture** | §6.5 and the capture actions: screenshot, recording, the `video` artifact kind | **High:** DESIGN REVIEW (privacy), then PRE-PR | P3 |
| **P6 Desktop MCP** | §7.1 to §7.3: classes `desktop` and `capture`, caps, `desktops` | **High:** DESIGN REVIEW, then PRE-PR | P3, P5 |
| **P7 Computer Use** | spike: the pointer tool, `wtype` on Omarchy, and how reliable the focus check is. Then §7.4 | **High:** DESIGN REVIEW after the spike, then PRE-PR | P6 |
| **P8 Accounts and provisioning** | §8.8 and §9 | Normal for §8.8. **High** for §9 (an invitation in a drive): DESIGN REVIEW | P4 |
| **P9 Per-session load** | §10 | Normal: one Codex review | — (in parallel) |

Each phase starts with a DL entry. Each phase adds its rows to `PARITY.md` (the four proofs) and its new surfaces to `SECURITY-CHECKLIST.md` for the owner's final review (DL-158). P1 and P9 can start now and in parallel, because their files do not overlap.

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
| P1, P4, P9 | one Codex review of the final branch | before merge |

---

## 14. Open items

1. **The pointer click tool** on Omarchy (P7 spike): `ydotool` needs a root daemon and `uinput`, and `wlrctl` depends on wlroots protocols that Hyprland supports.
2. **The format of the Omarchy account registry** (P8 brief, from `bin/omarchy-agent-account-state`).
3. **Distribution of `fleetdeck` on Arch** (owner): a release tarball, an AUR package, or a pacman repository. §9 needs one of them.
4. **The Omarchy lock IPC target name** and the sudoers drop-in path (P3 brief, from the Omarchy source).
5. **Upstream changes** of §8.10 (owner).
6. **A Fleet Deck record for the Omarchy agents panel.** The panel reads any record in its usage folder, so Fleet Deck could add the fleet-wide usage of all stations. The record contract is in the `claude` and `codex` collectors. This is not planned yet.
