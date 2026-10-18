# P4: Omarchy integration (revision 12)

- **Status:** built on v2 (DL-235); PRE-PR review: round 1 found 3 MAJOR installer defects, fixed in `831868f6`; round 2 PASS (P4-31). Live smoke pending. The gaps of this version that had no board path (the install on a station that P8 did not provision, DL-235 item b, and the screensaver switch, DL-235 item f) are closed by DL-243 (`omarchy-admin.md`). Decision P4-1 (architect): the split and the answers D1 to D29 are accepted; Q1 A, Q2 A, Q3 A; D19 (`ring` in the CLI crate) and D28 accepted. Section 2 lists the values that the Omarchy source files corrected. The adversary DESIGN REVIEW of tasks A, B1, E and the display route of F1 (section 3.7) runs before their code.
- **Parent:** [omarchy.md](./omarchy.md) §8 and §8A. This annex overrides §8 where they differ (§8A names the main cases).
- **Siblings:** [omarchy-p5a.md](./omarchy-p5a.md), [omarchy-p8.md](./omarchy-p8.md).
- **Omarchy facts:** read on 2026-10-18 from the source copy of commit `51bfd1d` (branch `quattro`): `bin/omarchy-hook`, `omarchy-notification-send`, `omarchy-theme-set-t3code`, `omarchy-agent-crash`, `omarchy-sudo-passwordless`, `omarchy-ascii`, `omarchy-plugin-enable`, `omarchy-plugin-validate`, `config/hypr/{bindings,hyprland}.lua`, `config/omarchy/extensions/omarchy-menu.jsonc`, `default/hypr/apps/{browser,pip}.lua`, `default/themed/t3code.json.tpl`, `default/agents/skills/omarchy/plugins.md`, `default/agents/skills/diagnose-crash/*`, `docs/omarchy-shell.md` and `shell/plugins/panels/tailscale/*`. Omarchy can change between versions. Section 5 lists what only the live smoke on the owner machines can confirm.
- **Implementation boundary:** no new third-party library. `ring` (already in the workspace) is added as a dependency of `fleetdeck-cli` for SHA-256 (D19). Each changed or new source file stays below 800 lines. The files `link/src/methods.rs` (779), `deck/src/link/mod.rs` (764), `station/src/link/intake.rs` (756) and `cli/src/cli.rs` (653) are near the limit: put new code in new modules and add only a routing line to these files. The new clap types go in `cli/src/cli_omarchy.rs`.

## 1. Scope and split

P4 builds §8 of `omarchy.md` except §8.8 (P8) and §8.10 (owner). Already built and reused: the P3 desktop adapter and its fake rig (`station/src/desktop/fake.rs`), the station local socket pattern (`station/src/local.rs`), the station settings pattern (`allow_desktop`), the deck board-watch pattern (`deck/src/wake/**`), the owner directive route, `DirectiveSender::Station` (an enum variant that returns `NotGranted` today, `store/display_directives.rs:173`), `StationResources`.

At most two workers run at the same time (machine limit). Tasks in the same step have no common owned file.

| Step | Task | Owned files (new modules unless marked) | Depends on |
| --- | --- | --- | --- |
| 1 | **A. Foundation** | `proto/src/fleet_summary.rs`, `proto/src/palette.rs`; feature and exports in `proto/src/{version,lib,link}.rs`; rows in `link/src/methods.rs`; the table of ADR-004 §9.6; station setting `fleet_summary` in `core/src/config.rs`, `core/src/settings/{fields,holder}.rs`, `station/src/manager/policy.rs`; `StationInfo.fleet_summary` and its fill in `station/src/link/intake.rs`; routing lines in `station/src/link/intake.rs` and `deck/src/link/{mod,requests}.rs`; `pub fn omarchy_root` in `station/src/desktop/detect.rs`; TS generation | none |
| 2 | **B1. Deck summary** | `deck/src/fleet_summary/{mod,build,send}.rs` and tests | A |
| 2 | **B2. Station receive and feed** | `station/src/link/intake_summary.rs`, `station/src/fleet_summary.rs`, the local socket op (`station/src/local.rs` plus `station/src/local_summary.rs`), `cli/src/desktop_feed.rs`, `cli/src/desktop_open.rs`, `cli/src/notify.rs` | A |
| 3 | **C. Installer** | `cli/src/omarchy/{mod,install,uninstall,status,record,fsx,rows}.rs`, `cli/src/cli_omarchy.rs`, `cli/src/doctor_omarchy.rs`, the `--brief` flag in `cli/src/doctor.rs`, `cli/Cargo.toml`, one match arm each in `cli/src/{cli,main}.rs` | B2 |
| 3 | **E. Hotkeys backend** | link method `display.directive` (deck side `deck/src/link/display_directive.rs`), `deck/src/store/display_directives_station.rs` (the `Station` sender), the arm in `store/display_directives.rs`, `station/src/local_wall.rs`, `cli/src/desktop_wall.rs` | A |
| 4 | **D. Static assets** | `cli/assets/omarchy/**` (plugin files, `hypr.lua`, hooks, `fleetdeck.json.tpl`, menu rows) and golden tests | C (file list) |
| 4 | **F1. Theme backend** | method `station.palette` (station `link/intake_palette.rs`, deck `link/palette.rs`), deck migration `store/palette.rs` (next free V number), routes `http/palette_routes.rs`, `cli/src/omarchy/theme_sync.rs` | A |
| 5 | **G. Screensaver** | `cli/src/omarchy/{screensaver,digits}.rs` | B2, C |
| 5 | **H. Crash diagnosis** | `cli/src/desktop_crash.rs` | A |
| 6 | **F2. Web Prefs** | `web/src/**` (Prefs choice; display palette) | F1 |
| 6 | **I. Docs** | `PARITY.md`, `SECURITY-CHECKLIST.md`, `omarchy.md` status, `Makefile` target `desktop-smoke` | all |

**Review class.** The design says "normal". Tasks A, B1, E and the display route of F1 touch authorization or export fleet data. They get an adversary DESIGN REVIEW of this annex before code, and one PRE-PR REVIEW. The other tasks get one Codex review of the final branch.

## 2. Values corrected by the Omarchy files

| Item | Before (§8 or the plan) | Correct value | Source |
| --- | --- | --- | --- |
| Notification command | The feed uses `omarchy-notification-send -u normal`. §5C item 4 says it has "no option terminator" and is unsafe. | It calls D-Bus with `busctl`, never `notify-send`. Options are parsed first. The **headline** is the first word that is not a known option, so a headline that starts with `--glyph` or `-u` would be read as an option. The **description** is always text, also when it starts with `-`. `--` is **not** a terminator: it would become the headline. `--exec <program> [args…]` takes the rest of the line as the argv of the click command and must come last; one single word with a space is refused. It needs `busctl` and `jq`. The default app name is `omarchy-action`. | `bin/omarchy-notification-send` |
| Doctor check for passwordless sudo | Check a sudoers drop-in file. | A user cannot read `/etc/sudoers.d` files. Run `omarchy-sudo-passwordless --active`: exit 0 means a grant is active. The files are `/etc/sudoers.d/99-omarchy-nopasswd-<uid>` (timed) and `99-omarchy-permanent-nopasswd-<uid>`. | `bin/omarchy-sudo-passwordless` |
| `omarchy ascii` | Large digits for the screensaver. | The font draws **letters and spaces only**. Digits and punctuation are skipped. The screensaver draws its digits with its own table (D28). | `bin/omarchy-ascii` |
| Hooks | `post-boot`, `post-update`, `theme-set` run files in `<name>.d/`. | `omarchy-hook <name> [args]` runs `~/.config/omarchy/hooks/<name>` first and then every file of `<name>.d/` in glob order. It skips `*.sample` and runs each with `bash "$file" "$@"`. A failed hook prints "Hook failed: <path>" and the loop goes on. The source names only `post-update`, `theme-set` and `font-set`. `post-boot` is not in these files (section 5). | `bin/omarchy-hook` |
| Plugin manifest | Kinds `service`, `bar-widget`, `panel`. | Required fields: `schemaVersion` (the number 1), `id`, `name`, `version`, `kinds`, `entryPoints`. The id matches `^[A-Za-z0-9][A-Za-z0-9._-]*$`, has no `..` and is not `omarchy.*`. Entry points: `bar`, `barWidget`, `menu`, `overlay`, `panel`, `service`; each value is a safe relative file that exists; no symlink in the folder. One manifest may declare several kinds. `barWidget.defaultSection` is one of `left`, `centerBegin`, `center`, `centerEnd`, `tray`, `right`. | `bin/omarchy-plugin-validate`, `docs/omarchy-shell.md` |
| Plugin IPC | `omarchy-shell fleetdeck.fleet <open\|close\|toggle\|refresh>` | A panel opens with `omarchy-shell shell summon\|hide\|toggle <id> '<payload json>'`, or with a binding `{ panel = "<id>" }`. A panel entry point exposes `open(payloadJson)` and `close()`. The target `fleetdeck.fleet` of §8.3 is dropped. | `docs/omarchy-shell.md`, `config/hypr/bindings.lua` |
| Plugin enable | `omarchy plugin enable fleetdeck.fleet --section right` | The section `right` is valid. A hand-placed plugin is enabled after `omarchy-shell shell rescanPlugins`; the answer `unknown` means a missing rescan. A new enable **places** the widget; a rerun would move a widget that the owner moved. The installer enables only once (D22). | `bin/omarchy-plugin-enable` |
| Plugin safety | none | Plugin text sinks use `textFormat: Text.PlainText`; data is bounded; theme colors come from `Commons.Color` through `import qs.Commons as Commons`. | `docs/omarchy-shell.md` |
| Theme template | Template maps `{{ background }}`, `{{ foreground }}`, `{{ accent }}`, `{{ color0..15 }}`. | Keys that the Omarchy template proves: `mode`, `background`, `foreground`, `accent`, `muted`, `dark_background`, `dark_foreground`, `bright_foreground`, `selection`, `red`, `yellow`, and `{{ mix a b N% }}`. `color0..15` are not proven and are dropped. The T3 script **skips** a file that still holds `{{` (`grep -q '{{'`) and exits 0 when the file is missing. The generated file is `~/.local/state/omarchy/current/theme/<name>.json`. | `default/themed/t3code.json.tpl`, `bin/omarchy-theme-set-t3code` |
| Hypr window classes | Rules for `fleetdeck-wall` and `fleetdeck-preview`. | The P3 classes are `fleetdeck-wall-<id>`, `fleetdeck-preview-<id>`, `fleetdeck-term-<id>` (`station/src/desktop/actions.rs:376,495`). The rules match by prefix. | P3 code |
| Hypr Lua syntax | `hypr.lua` with "window rules and hotkeys". | `o.window(<regex string or table>, { prop = value })` and `o.bind("SUPER + CTRL + F", "<description>", <command string or table>)`. Key names are upper case joined with ` + `. A panel binding is `{ panel = "<plugin id>" }`. | `default/hypr/apps/*.lua`, `config/hypr/bindings.lua` |
| Menu file | "dotted id → {icon, label, action}" | JSONC object. Fields: `icon`, `label`, `action` (a shell command; without it the row is a submenu), `target`, `provider`, `aliases`, `description`, `when`, `checked`. The parent is inferred from the dotted id. The file ships in `config/omarchy/extensions/` as a template with only comments, so on a real machine it **exists**; the installer almost always prints the rows. | `config/omarchy/extensions/omarchy-menu.jsonc` |
| Crash facts | "the same facts as `bin/omarchy-agent-crash`" | Facts: process (`comm`), PID, binary (`exe`), signal, time. The time is `coredumpctl list <pid> --no-pager --no-legend \| tail -1 \| cut -d' ' -f1-4`. The prompt text is quoted in section 3.9. Omarchy starts the agent with `omarchy-agent --prompt`; Fleet Deck starts a supervised session instead. The skill is `$OMARCHY_PATH/default/agents/skills/diagnose-crash/SKILL.md`; it tells the agent to write a core file to a `mktemp` path and delete it (a core can hold secrets). | `bin/omarchy-agent-crash`, `diagnose-crash/SKILL.md` |
| Wall workspace hotkey (from the code) | `Super+Alt+W` uses the station action `workspace.show`. | `desktop.act` needs the authority of the deck (P3b); a station cannot start it alone. `fleetdeck desktop wall show` runs `hyprctl` itself (section 3.6). | `station/src/link/intake_desktop.rs`, `desktop/authority.rs` |
| Station sender scope (from the code) | D25: `next_needs_you` goes to the oldest open request in the ceiling. | §5A item 3 says the bound station may reference only its own sessions. The Station sender resolves only sessions of the calling station. A wall that shows a session of another station uses the board. | `omarchy.md` §5A item 3 |

## 3. Contracts

### 3.1 Task A: foundation

- **Feature `desktop_shell.v1`** (D1). Constant `feature::DESKTOP_SHELL` in `proto/src/version.rs`, added to `ALL`, not to `REQUIRED`. It carries `fleet.summary`, `display.directive` and `station.palette`. ADR-004 §9.6 gets the three rows and the feature in the feature table.
- **Method rows** (D2). No new `Scope` variant.

| Method | Callers | Scope | Feature | Mutating |
| --- | --- | --- | --- | --- |
| `fleet.summary` | deck → station | `Read` | `desktop_shell.v1` | no |
| `display.directive` | station → deck | `Admin` | `desktop_shell.v1` | no |
| `station.palette` | station → deck | `Admin` | `desktop_shell.v1` | no |

  The two station-to-deck rows use `Admin`, the scope that every paired station already holds (`deck/src/link/runtime.rs` `STATION_SCOPES`); no scope is added to any station, because a `Read` grant would also open every other station-to-deck row that needs `Read` (the same reason as `fleet_git.artifact_authorize`, Fleet Git). The handlers authorize by the caller's own station identity: `display.directive` again by the binding in the store (3.6), `station.palette` only for the caller's own palette (3.7). (Revision 12c, decision P4-6.) `tests/method_table.rs` compares the table with ADR-004.
- **Station setting `fleet_summary`:** `bool`, default `false`, apply `Live`, danger (off to on needs the owner's elevation), label "Fleet summary for the desktop shell", help "Send a summary of the whole fleet (counts, the callsigns of sessions that need you, usage) to this station, for the Omarchy bar. Programs that run as you on this machine can read it." The local socket counts as elevated (like the other station settings).
- **`StationInfo.fleet_summary: Option<bool>`**, `#[serde(default, skip_serializing_if = "Option::is_none")]`, `#[ts(optional)]`. The station fills it from the live policy at each `station.info`.
- **Types** in `proto/src/fleet_summary.rs` (ts-rs exported to `fleet_summary/`):

```json
{"v":1,"at":"2026-10-18T12:04:00Z","board":"https://deck.example.ts.net:7420","deck":"up",
 "counts":{"working":7,"idle":3,"needs_you":2,"blocked":1,"lost":0},
 "needs_you":[{"station":"srv","station_id":"…","session":"…","request":"…","callsign":"lynx-12",
               "kind":"approval","age_s":95}],
 "usage":{"label":"claude · 5 h","pct":63,"resets_in_s":4320},
 "stations":[{"name":"srv","up":true,"cpu_load_pct":41},{"name":"laptop","up":true,"cpu_load_pct":null}]}
```

  - `v` is 1. `deck` is `up` when the deck sends it. `board` is an origin (`scheme://host[:port]`, scheme `http` or `https`, at most 2048 bytes, no whitespace, no path) or `null`.
  - `callsign` is `null` when the session has none or when it does not match `^[a-z0-9-]{1,32}$`. `kind` is the serde name of `RequestKind` (a closed set). `usage` is `null` without a window with a known use.
  - **No free-form text (revision 12, DESIGN REVIEW round 1, finding 2):** the summary has no session title, no tool name, no request title and no label that a person or an agent wrote. A title can repeat a path or a secret, so filtering it cannot prove the guarantee; it is left out. The only texts are: station names (the owner names them), the checked callsign, the closed `kind` words, the provider instance id and the fixed window words of `usage.label`, and ids.
  - Limits: `needs_you` at most 10, `stations` at most 32, `callsign` at most 32, `station` at most 64. The whole JSON is at most 16 KiB. The builder removes control and bidirectional characters from every text and cuts at the limit.
  - The summary holds no chat text, no tool input, no file path, no mail and no free-form title or tool name. The sentinel test puts a sentinel into every free-form source field (session title, tool name, request title, request details, chat text, input, mail, file path, a rate-limit `scope`, a Codex `limitId`, a provider instance id outside the grammar) and checks that the sentinel is in no summary.
- **Station-to-deck and deck-to-station answer codes:** the existing wire errors `invalid_params`, `forbidden`, `unsupported`, `failed`.
- **`omarchy_root`:** `pub fn omarchy_root(base: &ChildEnv) -> Option<(PathBuf, Option<String>)>` in `station/src/desktop/detect.rs` (the current private `omarchy` function made public, behavior unchanged). The installer and doctor use it.

### 3.2 Task B1: the deck builds and sends the summary

- `deck/src/fleet_summary/build.rs` is a pure function `build(cards, stations, now_ms, board) -> FleetSummary`.
  - **Counts** from `card_status`: `working` = Working + Verifying + Queued; `idle` = Idle; `needs_you` = NeedsYou; `blocked` = Unreachable; `lost` = Lost. Ended cards are not counted.
  - **`needs_you`:** one item for each open request of a card with status NeedsYou, oldest `requested_at` first, at most 10. `station_id` and `session` are ids. `age_s` = `(now - requested_at) / 1000`, not below 0. A card on an unreachable station gives no item.
  - **`usage`:** a Rust port of the web `rateMeters` and `tightest` (`web/src/board/usage.ts`): the latest window for each (station, instance, scope) from the most recently updated session; the highest `used_percent` wins; a window with unknown use is skipped. `label` = `<instance> · <scope label>` with a closed scope mapping (revision 12b, RE-REVIEW round 2): `five_hour` → `5 h`, `seven_day` → `7 d`, every other scope (a provider's `RateLimit.scope` or Codex `limitId` is free text) → `other`; `<instance>` is the provider instance id when it matches `^[a-z0-9_-]{1,32}$`, else `provider`. No provider string reaches the label. `pct` is rounded. `resets_in_s` = `(resets_at - now) / 1000`, not below 0, or `null`.
  - **`stations`:** name, link state (`up`), `cpu_load_pct` = `round(load1_x100 / cores)` (not capped) from `StationInfo.resources.cpu`, else `null`.
- `deck/src/fleet_summary/send.rs` is a task that watches `watch_board()`, built like `wake/`:
  - A station is eligible when its link is up, `desktop_shell.v1` is negotiated and the last `StationInfo.fleet_summary` is `Some(true)`.
  - At most one push for each station every 2 s (the latest state is sent at the end of the window). A push also goes out when a link comes up, when a `station.info` turns the setting on, and every 60 s as a keepalive.
  - One request in flight for each station; 5 s timeout. A failure is counted and the next change sends again. No audit row (not an owner action).
  - The builder takes `board` from the first advertised non-loopback endpoint origin of the deck (the source of `endpoint_origin` in `desktop_relay`); `null` when there is none.
  - A deck without a station that wants the summary builds nothing.
- **Tests:** counts and mapping; ordering and limits; `usage` choice (unknown skipped, ties, a window that reset); the sentinel test (chat text, tool input, a file path and mail never appear); debounce with a fake clock; a station without the feature gets nothing; a setting that turns off stops the next push.

### 3.3 Task B2: the station receives it; `desktop feed`, `desktop open`, notifications

- **Intake `fleet.summary`** (`station/src/link/intake_summary.rs`): the request is refused with `forbidden` ("The fleet summary is off on this station.") when the live setting is off; with `invalid_params` when the JSON does not parse, is above 16 KiB, has `v` other than 1, breaks a limit of 3.1 or a text grammar (`station_id`, `session` and `request` match `^[A-Za-z0-9_-]{1,64}$`). The station keeps the last valid summary and the receive time **in memory only** and never writes it to disk. It answers `{"ok":true}`.
- **Local socket op** (`station/src/local_summary.rs`, a new `LocalRequest::FleetSummary { after: u64 }`):
  - The station keeps a counter `seq` that grows when a stored summary changes (the JSON differs) or the link state changes.
  - The station holds the request until `seq > after` or 25 s pass.
  - Response `LocalResponse::FleetSummary { seq, link, held_s, summary }`: `link` is `up` or `down` (the live deck link); `held_s` is the seconds since the summary arrived; `summary` is the stored summary or `null`. When the setting is off the answer is `LocalResponse::FleetSummaryOff {}`.
- **`fleetdeck desktop feed [--notify]`** (`cli/src/desktop_feed.rs`) prints one compact JSON line (newline, flushed) on each change and one at each timeout of the long poll (so at least one per 30 s):
  - The line is the summary with these corrections: `deck` = `down` when `link` is `down`; `unknown` when `held_s` is above 150 (a stale summary); `up` otherwise. `age_s` and `resets_in_s` are corrected by `held_s` (`age_s + held_s`, `resets_in_s - held_s`, not below 0).
  - No summary yet or the setting off: `{"v":1,"deck":"unknown"}`.
  - The socket is missing or closes, or the answer is invalid: print `{"v":1,"deck":"unknown"}` and try again with backoff 1, 2, 4 … 30 s (reset on the first good answer).
  - The command ends with exit 0 when stdout is closed. It never ends for another reason.
- **`--notify`** (`cli/src/notify.rs`):
  - The first good summary is only a baseline. A key is `(station_id, session, request)`; a new key in a later summary notifies once. A new key whose `age_s` (corrected) is above 60 does not notify.
  - At most 5 notifications for each summary. For more, one more notification: headline `Fleet Deck`, description `<n> more sessions need you`.
  - **argv** of one notification (exact; `--` is never used):

```text
omarchy-notification-send --app-name "Fleet Deck" -u normal <headline> <description> --exec <exe> desktop open <station_id>/<session> --request <request>
```

  - `headline` = `<callsign> needs you`. The callsign must match `^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$`; else the first 8 characters of the session id if they match it; else `session`. The headline therefore never starts with `-`.
  - `description` = `<Kind> on <station>` where `Kind` is `Approval`, `Question` or `Request` (fixed words) and `<station>` is the station name; no title and no tool name (revision 12, finding 2). Markup characters (`<`, `>`, `&`) and control characters are removed; the text is cut at 300 characters.
  - `--exec` is added only when `station_id`, `session` and `request` match the grammar above. `<exe>` is `std::env::current_exe()`, absolute. The ids are separate words, so no text can become a command.
  - A missing `omarchy-notification-send`, `busctl` or `jq` gives one warning on stderr; the feed goes on. A notification never answers a request.
- **`fleetdeck desktop open [<station_id>/<session>] [--request <id>]`** (`cli/src/desktop_open.rs`):
  - It calls the local op once (no wait) and reads `board`.
  - URL: `<board>/` (no argument), or `<board>/#/s/<station_id>/<session>`, plus `?request=<id>`. The ids must match the grammar; the URL holds no token.
  - It starts `xdg-open <url>` detached and exits 0. Errors, exit 1: no board address ("No board address is known. Run the feed first." when there is no summary; "The deck did not tell its board address." when `board` is `null`); a bad argument ("Use <station_id>/<session>."); `xdg-open` missing ("xdg-open is not installed.").
- **Tests:** a fake station socket (long poll, `after`, timeout, close, invalid line); feed lines for each case above including a stale summary; the baseline, deduplication, the age rule, the burst of 6; the argv of the fake `omarchy-notification-send` (log with one argument for each line, as the P3 rig does) for a normal case, a callsign that starts with `-`, a title that looks like an option, and an id outside the grammar (no `--exec`); `desktop open` with a fake `xdg-open`; stdout closed.

### 3.4 Task C: the installer

`fleetdeck omarchy install [--yes] [--screensaver]`, `uninstall`, `status [--json]`, and (task F1) `theme-sync`. Exit codes: 0 success, 1 error (one line `fleetdeck: <text>` on stderr), 2 a usage error from clap.

**Detection.** `omarchy_root` (3.1). Without Omarchy: exit 1, "Omarchy was not found on this machine." The installer needs no running Hyprland.

**Paths.** The config home is `$XDG_CONFIG_HOME` when it is set and absolute, else `$HOME/.config`. `<omarchy>` = `<config home>/omarchy`, `<fd>` = `<config home>/fleetdeck`. The state home for Omarchy is `$HOME/.local/state/omarchy`.

**Files** (embedded in the binary with `include_str!` from `cli/assets/omarchy/`):

| File | Mode | Source |
| --- | --- | --- |
| `<omarchy>/plugins/fleetdeck.fleet/manifest.json` | 0644 | 3.5 |
| `<omarchy>/plugins/fleetdeck.fleet/{Widget,Panel,Service}.qml`, `Model.js`, `i18n/en.json` | 0644 | 3.5 |
| `<omarchy>/hooks/post-boot.d/fleetdeck` | 0755 | 3.5 |
| `<omarchy>/hooks/post-update.d/fleetdeck` | 0755 | 3.5 |
| `<omarchy>/hooks/theme-set.d/fleetdeck` | 0755 | 3.5 |
| `<omarchy>/themed/fleetdeck.json.tpl` | 0644 | 3.7 |
| `<fd>/omarchy/hypr.lua` | 0644 | 3.5 |
| `<omarchy>/extensions/omarchy-menu.jsonc` | 0644 | only when the file does not exist (3.5) |
| `<fd>/omarchy-install.json` | 0600 | the record |

Directories are created with mode 0755. Every write is atomic: an exclusive temporary file in the same directory, `fsync`, rename. The installer follows no symlink: a parent that is a symlink or belongs to another user is refused ("`<path>` is a link or is not yours; change nothing.").

**The record** (`<fd>/omarchy-install.json`, rewritten atomically after each file):

```json
{"v":1,"fleetdeck_version":"…","files":[{"path":"plugins/fleetdeck.fleet/manifest.json","base":"omarchy","sha256":"<64 hex>","existed_before":false}],
 "plugin_enabled":true,"fleet_summary_set_by_install":true,"menu_file_written":false,
 "screensaver":{"enabled":false,"had_original":false,"backup_sha256":null,"last_written_sha256":null}}
```

`base` is `omarchy` or `fleetdeck`; `path` is relative to it. SHA-256 comes from `ring`.

**Install steps, in order:**
1. Detect Omarchy. Compute the file set.
2. Print the list (paths, modes, one line for each of: the plugin enable, the `fleet_summary` switch, the printed lines). Ask `Continue? [y/N]` on a terminal. With no terminal and no `--yes`: exit 1, "Run with --yes to confirm." `--yes` skips the question.
3. For each file: an existing target that is **not** in the record and differs from the new content is a refusal (`conflict: <path>`; nothing after it is written). An identical file is adopted. A recorded file whose hash equals the recorded hash is overwritten. A recorded file that changed is kept and named ("kept (changed): <path>").
4. Run `omarchy-shell shell rescanPlugins` (the answer must be `ok`). Then, only when the record says `plugin_enabled` is not true, run `omarchy plugin enable fleetdeck.fleet --section right`. A failure of either is a **warning** with the exact command to run later; `plugin_enabled` stays false and the exit code stays 0.
5. Turn on `fleet_summary` through the station local socket (`SettingsSet`). If the station is not running: warn, print `fleetdeck settings set --station fleet_summary true`, exit code stays 0. `fleet_summary_set_by_install` is true only when this call changed the value from false.
6. With `--screensaver`: record `screensaver.enabled = true` (the job of 3.8 does the rest).
7. Print the line to add to the Hyprland file, and the menu rows when the menu file was not written:
   - Revision 12d (decision P4-7, owner rule remote first): the installer appends exactly one managed line at the end of `<config home>/hypr/hyprland.lua`: `pcall(dofile, "<absolute path of the installed hypr.lua>") -- fleetdeck: managed line` (`pcall`, so a missing file never breaks Hyprland). It adds the line only when it is not present; it records the file hash before and after; uninstall removes exactly that line when the file still holds it unchanged. A `hyprland.lua` that is a symlink, is not a regular file or is owned by another user is not edited: the installer then prints the line (the old fallback) and `status` shows "Hyprland line missing".
   - The rows of 3.5.

**`status [--json]`:** exit 0 only when a record exists and no file is `changed` or `missing`. Text: one line for each file `ok|changed|missing <path>`, then `plugin: enabled|not enabled`, `summary: on|off|unknown`. JSON: `{"installed":bool,"files":[{"path":…,"state":"ok|changed|missing"}],"plugin_enabled":bool,"summary":"on|off|unknown"}`. P8 uses the exit code.

**`uninstall`:** removes each recorded file whose hash is still the recorded hash; keeps a changed file and names it; removes only empty directories that the install created. It runs `omarchy plugin disable fleetdeck.fleet` (a failure is a warning), turns `fleet_summary` off only when the record says the install turned it on, and restores the screensaver (3.8). It removes the record last, and only when no file was kept.

**`doctor`** (file `cli/src/doctor_omarchy.rs`):
- `ok omarchy: no Omarchy on this machine`, or `ok omarchy: Omarchy <version>; the integration is not installed (run fleetdeck omarchy install)`, or `ok omarchy: installed, no drift`, or `warn omarchy: <n> changed or missing files (run fleetdeck omarchy status)`.
- `warn omarchy-sudo: passwordless sudo is on for this user` when `omarchy-sudo-passwordless --active` (5 s limit) exits 0; `ok omarchy-sudo: off` when it exits with another code; `warn omarchy-sudo: could not check` on a timeout or spawn error; nothing when the script is not on the Omarchy `PATH`.
- New flag `--brief`: print only the `warn` and `fail` lines and the final `doctor:` line. The exit code is unchanged (1 only with a `fail`).

**Tests:** a fake `$HOME` and a fake `PATH` with logging scripts `omarchy`, `omarchy-shell`, `omarchy-sudo-passwordless`: install, uninstall and install again; a changed file is kept and named; a conflicting existing file refuses before any write; an existing menu file is never written; the rerun does not enable the plugin again; failure of the rescan and of the enable are warnings; the station not running; no terminal without `--yes`; a symlink parent is refused; `status` reports drift and its exit code; the record after an interrupted install; doctor lines for each case and `--brief`.

### 3.5 Task D: the embedded assets

**`manifest.json`** (no `omarchy.*` id; `omarchy plugin validate` must pass):

```json
{
  "schemaVersion": 1,
  "id": "fleetdeck.fleet",
  "name": "Fleet Deck",
  "version": "1.0.0",
  "author": "Fleet Deck",
  "description": "Fleet counts, the list of sessions that need you, and wall navigation from Fleet Deck.",
  "kinds": ["bar-widget", "panel"],
  "keepLoaded": true,
  "entryPoints": { "barWidget": "Widget.qml", "panel": "Panel.qml" },
  "barWidget": {
    "displayName": "Fleet Deck",
    "description": "Counts of sessions and the ones that need you.",
    "category": "Agents",
    "allowMultiple": false,
    "defaultSection": "right",
    "defaults": {},
    "schema": []
  }
}
```

**QML files** (the model is `shell/plugins/panels/tailscale/`):
- `Service.qml` starts `fleetdeck desktop feed --notify` with Quickshell `Process` and a line reader, parses each line as JSON, bounds every field (the limits of 3.1), and restarts the process 5 s after a non-zero exit. Exit code 0 is final (no restart). `Widget.qml` owns one `Service.qml`.
- `Widget.qml` (bar widget): a glyph and the text `<working> · ⚠<needs_you>` (the warning part only when `needs_you` > 0). Urgent color when `needs_you > 0`; dim color when `deck` is `down` or `unknown`. Left click: `omarchy-shell shell toggle fleetdeck.fleet '{}'`. Right click: `fleetdeck desktop open`. Middle click: `fleetdeck desktop wall show`. Run with `Quickshell.execDetached`.
- `Panel.qml` (panel kind: `open(payloadJson)` and `close()`): on open it starts its own `fleetdeck desktop feed` (no `--notify`) and stops it on close. It lists the `needs_you` items, then the counts for each station and the usage line. Keys: `j`/`k` and the arrows move; `Enter` runs `fleetdeck desktop open <station_id>/<session> --request <request>`; `w` runs `fleetdeck desktop wall next`; `Esc` closes. **No approval control.**
- Every text sink uses `textFormat: Text.PlainText`. Colors come from `Commons.Color` (`import qs.Commons as Commons`). Strings come from `i18n/en.json` through `Commons.Translations`.
- `Model.js` holds the pure parsing and bounding code of the feed lines. Rust tests check only its text; the live smoke runs it.

**`hypr.lua`** (the file `<fd>/omarchy/hypr.lua`; `o` is the Omarchy helper):

```lua
-- Fleet Deck (installed by `fleetdeck omarchy install`). Reinstalling rewrites this file.
o.window("^fleetdeck-wall-[A-Za-z0-9_-]+$", { border_size = 0 })
o.window("^fleetdeck-preview-[A-Za-z0-9_-]+$", { float = true, size = { 1280, 800 } })
o.bind("SUPER + CTRL + F", "Fleet Deck panel", { panel = "fleetdeck.fleet" })
o.bind("SUPER + ALT + W", "Fleet Deck wall", "fleetdeck desktop wall show")
o.bind("SUPER + ALT + N", "Fleet Deck: next session that needs you on the wall", "fleetdeck desktop wall next")
o.bind("SUPER + ALT + M", "Fleet Deck: next wall mode", "fleetdeck desktop wall mode next")
```

The installer checks that `fleetdeck` is on its own `PATH` and warns when it is not (the binding runs with the session `PATH`).

**Hooks** (`#!/bin/bash`, mode 0755; `omarchy-hook` runs them with `bash`; each ends with `exit 0` so that Omarchy never prints "Hook failed" for a Fleet Deck problem; a problem goes to stderr):
- `post-boot.d/fleetdeck`: if `fleetdeck` is not on `PATH`: print "Fleet Deck: fleetdeck is not on PATH." to stderr and stop. Else run `fleetdeck service start` (default: every installed service; with none installed it does nothing), then `fleetdeck omarchy theme-sync`. A failure prints one line to stderr. **No wall restore (Q2 A), no join (P8 owns provisioning).**
- `post-update.d/fleetdeck`: run `fleetdeck doctor --brief`. When its exit code is not 0, run `omarchy-notification-send --app-name "Fleet Deck" -u normal "Fleet Deck" "Fleet Deck: doctor found a problem after the Omarchy update"`.
- `theme-set.d/fleetdeck`: run `fleetdeck omarchy theme-sync` (the theme name in `$1` is not needed).

**Menu rows** (written only into a new file; otherwise printed to add by hand inside the existing object):

```jsonc
  "fleet": {"icon": "", "label": "Fleet"},
  "fleet.board": {"icon": "", "label": "Open the board", "action": "fleetdeck desktop open"},
  "fleet.crash": {"icon": "", "label": "Diagnose the last crash", "action": "fleetdeck desktop crash --last"}
```

The icons are Nerd Font code points U+F0E8, U+F0AC and U+F188. A new file holds one comment line, `{`, the rows and `}`.

**Tests:** golden-text tests of each file; every hook runs under `bash -n`; the manifest parses, passes the schema rules of `omarchy-plugin-validate` as a Rust test (required fields, entry points exist, no symlink, id grammar); `hypr.lua` has the six lines. QML is not unit-tested; the live smoke checks it (section 5).

### 3.6 Task E: hotkeys backend

- **`display.directive`** (station → deck, `desktop_shell.v1`). Params: `{"display_id":"…","intent":"next_needs_you"|"next_mode"}` (`deny_unknown_fields`). The deck authorizes in `display_directives_station.rs`:
  1. the display exists and is **bound to the calling station** (else `not_bound`);
  2. the deck's `desktop_control` is on (else `desktop_control_off`);
  3. the result is a directive of the owner's typed model (`DirectiveBody`), validated and stored with `DirectiveSender::Station(station_id)` and the existing checks (ceiling, references, live authority, audit row `display.directive` with identifiers only).
  - `next_needs_you`: the deck takes the open requests of the **calling station's own sessions** that are inside the display's ceiling, oldest first. The target is the first request after the one that the current directive shows (a `focus` on a session with an open request), wrapping around; with no current focus, the oldest. No open request: `nothing_to_show`. The directive is `focus` for that session, `pin: false`.
  - `next_mode`: the cycle is `mission` → `focus` (only when a request exists, else skipped) → `ambient` → `mission`, from the stored directive (none = `mission`).
  - Limit: 2 calls per second for each station (`too_many_requests`).
  - Errors: `not_bound`, `desktop_control_off`, `nothing_to_show`, `invalid_params`, `too_many_requests`; each with a fixed text.
- **CLI:** `fleetdeck desktop wall next`, `fleetdeck desktop wall mode next` and `fleetdeck desktop wall show`.
  - `next` and `mode next` call the station local socket (`LocalRequest::WallDirective { intent }`). The station finds the displays bound to it from its own `DesktopInfo.walls` and sends one `display.directive` for each. A deck error is printed as `<code>: <text>` and gives exit 1. On a machine with no bound wall the deck answers `not_bound`.
  - `show` runs in the CLI process, with `hyprctl` from the desktop `PATH`: `hyprctl clients -j`; the first window whose class matches `^fleetdeck-wall-[A-Za-z0-9_-]+$`; if the active workspace already holds it, `hyprctl dispatch workspace previous`; else `hyprctl dispatch focuswindow address:<address>` (the address matches `^0x[0-9a-f]{1,16}$`). It changes no Fleet Deck state and needs no authority of the deck. No wall window: exit 1, "No Fleet Deck wall window is open."
- **Tests:** the deck store: a bound station can send both intents; an unbound one cannot; a station cannot reach a session of another station; the ceiling; a revoked display; `desktop_control` off; the audit row; the rate limit; the exact-rows tests of the display methods are unchanged (a station call is not a display row). CLI: a fake local socket; `show` with a fake `hyprctl` (the argv log).

### 3.7 Task F1: theme sync

- **Template** `<omarchy>/themed/fleetdeck.json.tpl`:

```json
{
  "mode": "{{ mode }}",
  "background": "{{ background }}",
  "foreground": "{{ foreground }}",
  "accent": "{{ accent }}",
  "muted": "{{ muted }}",
  "dark_background": "{{ dark_background }}",
  "dark_foreground": "{{ dark_foreground }}",
  "bright_foreground": "{{ bright_foreground }}",
  "selection": "{{ selection }}",
  "red": "{{ red }}",
  "yellow": "{{ yellow }}"
}
```

- **`fleetdeck omarchy theme-sync`** reads `$HOME/.local/state/omarchy/current/theme/fleetdeck.json`.
  - A missing file: stderr "Fleet Deck: no generated theme file; nothing was sent.", exit 0 (the T3 script also exits 0).
  - A file that still holds `{{`, is not valid JSON, is above 4 KiB, has `mode` other than `dark` or `light`, or has a color that does not match `^#[0-9A-Fa-f]{6}$`: stderr "Fleet Deck: the theme file is not usable; nothing was sent.", exit 0, previous palette stays.
  - Else it calls the station local socket (`LocalRequest::PaletteSet`). The station keeps the palette in memory, sends `station.palette` to the deck, and sends it again at each link up. A deck that cannot be reached: exit 1, "The deck is not connected. The palette is sent when the link is up again."
- **`station.palette`** (station → deck): params `{"v":1,"mode":"dark|light","colors":{"background":"#rrggbb", …}}` with exactly the ten color keys of the template; unknown keys, other values and a body above 2 KiB are `invalid_params`. The deck keeps one row for each station (table `station_palettes(station_id PRIMARY KEY, mode, colors, updated_at)`, the next free migration number; the row goes when the station is removed).
- **Routes:** `GET /api/stations/{station}/palette` (owner login, scope Read; station ids only) answers `{"mode":…,"colors":{…},"updated_at":…}` or 404 `no_palette`. `GET /api/displays/self/palette` (a display login only, a new `.display()` row; the exact-rows test `exactly_these_methods_are_open_to_a_display` lists it) follows the R13 display rules (revision 12, DESIGN REVIEW round 1, finding 1): (1) `scope_of` first (the live display session: an ended login is `401 login_required`); (2) the display's bound station must be in its current effective scope (`scope.stations`); (3) read the palette of that station; (4) read the binding and the scope again and check that the same station is still bound and still in scope; (5) answer the palette, else the same `404 no_palette` for an unbound display, a bound station outside the scope, a binding or scope that changed during the read, and a station without a palette. `Cache-Control: no-store`.
- **Web (F2):** Prefs gets "Use the Omarchy theme of <station>" (a list of the stations that have a palette, plus "Off"). The wall and display pages use the palette of the bound station. The F2 brief names the CSS variables from `web/src/styles`.
- **Tests:** the file cases above; the station keeps and resends the palette; the deck stores, replaces and deletes with the station; the display route refuses an owner cookie and any other principal; a palette never leaves for another station; a display bound to a station outside its ceiling gets `404 no_palette`; a rebinding, a ceiling change and a revoke during the read give `404` or `401`; an ended display session gets `401`.

### 3.8 Task G: the ambient screensaver

- The job is the hidden command `fleetdeck desktop screensaver-run`. The plugin `Service.qml` starts it next to the feed; it exits 0 at once (no restart) when the record says `screensaver.enabled` is false.
- It reads the feed (the local op of 3.3). The first write copies `<omarchy>/branding/screensaver.txt` to `screensaver.txt.fleetdeck-backup` with an exclusive create; an existing backup is never replaced; with no original file, `had_original` is false. It records the hashes in the record.
- Every 60 s it renders and writes `screensaver.txt` atomically, only when the content changed. With no data it writes the line `Fleet Deck: no data`.
- **Layout** (ASCII only; any other character becomes `?`; at most 40 lines of 100 columns):
  1. the word `FLEET` from `omarchy ascii FLEET` when that command exists and answers within 2 s (letters work), else the line `FLEET DECK`;
  2. a blank line;
  3. the `needs_you` count drawn with an embedded 5-row digit table (`cli/src/omarchy/digits.rs`, digits 0 to 9; above 99 it shows `99`), and the line `need you`;
  4. `working N   idle N   blocked N   lost N`;
  5. the oldest needs-you item: `<callsign> · <kind label> · <age>`;
  6. the usage line: `<label> · <pct>% · resets in <h> h <m> min`.
- **Uninstall** restores the backup only when the current file hash equals `last_written_sha256`; with no original it removes the written file under the same condition; otherwise it keeps the file and names it.
- **Tests:** the render of the layout for each case; the digit table; the first write and the backup (never replaced); a user edit is kept at uninstall; a missing `omarchy ascii` (fake script); the exit 0 when disabled.

### 3.9 Task H: crash diagnosis

`fleetdeck desktop crash --last` (`cli/src/desktop_crash.rs`):
1. Run `coredumpctl list --json=short --no-pager` (10 s limit, output at most 1 MiB, desktop `PATH`). Pick the entry with the largest `time`. Missing tool, a failure, invalid JSON or an empty list: exit 1 with a fixed text ("coredumpctl is not available.", "No crash was found.").
2. Facts: `comm` (the `comm` field, else the base name of `exe`), `pid`, `exe`, `sig`, time (the `time` field in microseconds, shown as `YYYY-MM-DD HH:MM:SS UTC`). Texts lose control and bidirectional characters and are cut at 200 characters; `pid` must be digits.
3. Prompt (the text of `bin/omarchy-agent-crash`, with the values filled in):

```text
A process crashed on this Omarchy machine and I want to know why.

What systemd-coredump recorded:
  process:  <comm>
  PID:      <pid>
  binary:   <exe>
  signal:   <sig>
  time:     <time>

Use the diagnose-crash skill: it covers how to investigate, what to report, and
when a crash is worth reporting upstream to Omarchy. If your harness has no skill
mechanism, read the skill files directly and follow them instead:

  <omarchy path>/default/agents/skills/diagnose-crash/SKILL.md
```

4. Start the session through the station local socket like `station run`: the first `provider_instance` of `station.toml`, `cwd` = the canonical `$HOME`, `mode` none (the provider default, which asks for approvals). If `$HOME` is not inside `allowed_roots`: exit 1, "Add your home directory to allowed_roots to diagnose a crash." No provider instance: exit 1, "No provider is configured on this station."
5. Print the session id. Send one notification (`omarchy-notification-send --app-name "Fleet Deck" -u normal "Fleet Deck" "A crash diagnosis session started" --exec <exe> desktop open`); a failure is a warning.
- The skill tells the agent to dump a core file with `mktemp` and delete it. A core can hold secrets. The session is supervised: the owner sees each command on the board.
- **Tests:** a fake `coredumpctl` (newest entry, empty list, invalid JSON, missing tool); the prompt text with a hostile `exe` (control characters, a long value); the start command on a fake local socket; the two refusals.

### 3.10 Task I: documents and the smoke target

`PARITY.md` gets the P4 rows with the four proofs; `SECURITY-CHECKLIST.md` gets the new surfaces (the summary on the local socket, the station sender, the palette routes, the installer's writes, the hooks, the notification click). A surface that is not built stays listed as pending. `Makefile` gets `desktop-smoke` that prints the checklist of section 5 (it runs no Omarchy command by itself on a machine without Omarchy).

## 4. Tests: the four proofs

- **Behavior:** the fake rig for the installer, the feed, the notification argv, the hotkeys and the crash command (3.3 to 3.9).
- **Failure:** station not running; deck down; stale summary; backoff; interrupted install; a changed file; missing tools (`omarchy-notification-send`, `busctl`, `jq`, `xdg-open`, `coredumpctl`, `hyprctl`).
- **Exposure:** the sentinel test of the summary (no chat text, input, path, mail); no token in any URL or argv; ids and texts follow the grammars; the Station sender reaches only its own sessions and the ceiling; the display palette row is in the exact-rows test; the installer writes only the listed files and follows no symlink.
- **Version:** an older station or deck without `desktop_shell.v1` gets no new method; the feed prints `unknown`; the board hides nothing new.

## 5. Facts confirmed only by the live smoke (owner machines: server and laptop)

The repository and the source copy cannot show these. The live smoke (`make desktop-smoke` prints the list) confirms each one. A difference changes the asset files, not the Rust code.

1. The Omarchy version of both machines against `51bfd1d` (`4.0.0.alpha`).
2. `omarchy plugin validate` on the installed plugin; the widget shows in the `right` section; hot reload; the Quickshell `Process` and line-reader API; `Quickshell.execDetached`; `keepLoaded` for the panel; the `Commons.Color` names used for the urgent and dim colors.
3. The panel opens with `omarchy-shell shell toggle fleetdeck.fleet '{}'` and with the binding `{ panel = "fleetdeck.fleet" }`; `Esc` closes it.
4. Hypr Lua: the string argument of `o.window` matches the window **class**; the properties `border_size`, `float`, `size` are accepted; `o` is visible in a file loaded with `dofile` through `pcall` (the managed line, P4-7); the four keys do not collide with Omarchy defaults (`omarchy menu keybindings --print`); a command string in `o.bind` runs with a `PATH` that has `fleetdeck`.
5. Hooks: `omarchy-hook post-boot` is called at login (the source copy shows only `post-update`, `theme-set`, `font-set`); the order and the arguments of `theme-set`.
6. Theme: the value format of `{{ background }}` and the others (`#rrggbb`), the values of `{{ mode }}`, that a user template in `<omarchy>/themed/` renders to `current/theme/fleetdeck.json` on a theme switch.
7. Notifications: `busctl` and `jq` exist; a click runs the `--exec` argv; the app name and the text show as designed.
8. `omarchy-sudo-passwordless --active` exit codes with a real `sudo` (on, off, restrictive `listpw`).
9. `coredumpctl list --json=short` field names (`time`, `pid`, `exe`, `sig`, `comm`) on the installed systemd.
10. The screensaver: how Omarchy draws `screensaver.txt` (width, height, centering); `omarchy ascii FLEET`.
11. `hyprctl dispatch focuswindow address:…` and `workspace previous` for `wall show`, with a real kiosk window.
12. The menu rows appear under "Fleet" with their icons and run their actions.
13. End to end: feed and bar widget, a needs-you notification and its click, `desktop open`, `wall next` and `wall mode next` on the server with a bound wall, the laptop answer `not_bound`, theme sync to the board, the screensaver, `fleetdeck omarchy uninstall` and install again.

## 6. Phase notes

- **§8.8 accounts to provider instances:** P8 (`omarchy-p8.md`). Open item 14.2 (the registry format) stays with P8.
- **§8.10 upstream changes:** owner decision, not in this repository.
- **P8 depends on P4:** `fleetdeck omarchy install --yes` that needs no terminal, and `status` with the exit code for drift (3.4). The join step of §8.6 and the first-login hook belong to P8.
- **Wall restore after a reboot:** not built (Q2 A). The wall profile `wall-<id>` stays on disk; the owner reopens the wall from the board. A later task can add a deck-side restore with its own review.
- **Coordinator sender for directives:** P6. P4 activates only the `Station` sender.
- **Release and packages (AUR, Homebrew):** the owner's release task.
- **Open items §14.4:** the sudoers drop-in path is answered (section 2); the lock IPC target stays as P3 built it (`omarchy-lock-screen`).
- **Web:** only F2 (Prefs and the display palette) is in P4. The rest of the P2c and P3c web backlog is not P4.

## 7. Risks

- The summary leaves the deck for a socket that any program of the same user can read. It holds counts, station names, callsigns, request kinds and usage of the whole fleet, and no free-form text (no titles, no tool names). Mitigations: default off; turning it on needs elevation; the station keeps it in memory only; the content limits and the sentinel test.
- The Station sender is a new authority. Mitigations: binding check, own sessions only, ceiling, `desktop_control`, rate limit, audit row, adversary review.
- The installer writes into the user's Omarchy configuration. Mitigations: a fixed file list, hashes, refusal of conflicts, no symlink, uninstall by hash, the owner adds the Hyprland line.
- A hook or a notification click starts `fleetdeck` with data from the deck. Mitigations: ids and texts follow strict grammars; each value is a separate argument; no text reaches a shell.
- Omarchy changes between versions. Mitigations: the source copy is pinned (`51bfd1d`); section 5 lists what the live smoke must confirm; the assets are plain files that a worker or the owner can change without Rust changes.
