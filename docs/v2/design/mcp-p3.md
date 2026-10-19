# MCP P3 — every session is fleet-aware: the fleet view, resources, prompts, short instructions, the fleet skill

- **Status:** Draft revision 2, 2026-10-20. Revision 1 (c8a16525) got adversary DESIGN REVIEW FAIL (1 BLOCKER, 6 MAJOR, 2 UNVERIFIED; `~/fleetdeck-briefs/mcp-p3-design-r1-adversary.md`). Revision 2 applies all nine (§9 maps them).
- **Program:** DL-251 (owner decisions D1–D3), DL-253 (P1). P3 comes before P2 (D3).
- **Evidence:** `spikes/s-aware/README.md` (live: Claude 2.1.302 opens with `server/discover`, reads MCP resources with `ReadMcpResourceTool`, expands MCP prompts as slash commands, loads a `--plugin-dir` skill; Codex 0.163.0 opens with `initialize` `2025-06-18`, reads resources with `list_mcp_resources`/`read_mcp_resource`, never uses prompts, takes a skill root with the app-server request `skills/extraRoots/set` followed by `skills/list` with `forceReload`; both follow MCP `instructions`). The D2 data path: `~/fleetdeck-briefs/mcp-p3-research-notes.md` §"D2 data path".
- **Out of scope:** completions; output schemas for deck-made tool results; P2 events (resource changes are not pushed yet; the TTLs bound staleness); the default worker grant (P4).

## 1. Goals

1. Every session, with or without a grant, can read exactly D2: the stations (name, platform, online state, providers, projects), itself, and the session list (titles and states, no content).
2. Every session gets, at start and without any write into its workspace or the user's provider configuration: short instructions with a mandatory safety preamble, the fleet view (a tool and resources), and the fleet skill.
3. The long deck control skill leaves the per-turn `instructions` and becomes `fleet://guide`; its safety rules stay per turn (§3.3).
4. Every piece of guidance (instructions, prompts, skill) names only tools that the binding has, or says "if your tool list has …".

## 2. The fleet view (D2) — link method `fleet.view`

Grant-less bindings cannot send `coordinator.query` (it needs the link scope `coordinate`, which only stations named in a grant hold). The fleet view is a new read method on the scope that every paired station holds.

### 2.1 Wire (`crates/fleetdeck-proto`, `crates/fleetdeck-link`)

- Method row (`methods_table_tail.rs`): `fleet.view`, `StationToDeck`, `Scope::Admin`, not mutating (no receipt), feature `fleet_view.v1` (new constant in `crates/fleetdeck-proto/src/version.rs`, negotiated like the other features).
- Params `FleetViewRequest { caller: SessionId, view: FleetView, cursor: Option<String> }`, `FleetView` = `stations` | `sessions` | `me` (serde `snake_case`, `deny_unknown_fields`). `cursor` is opaque (max 256 bytes) and is ignored for `me`.
- Answer: `FleetViewPage { items: Value, next_cursor: Option<String> }` where `items` is the view's JSON of §2.3 (`me`: an object; the others: an array). Typed item structs live in proto.
- The station sends `fleet.view` only on a link that negotiated `fleet_view.v1`; otherwise it never sends it and answers locally (§3.1). A deck sends the answer only to a station that negotiated the feature (the link refuses the method otherwise).

### 2.2 Deck rules (new module `crates/fleetdeck-deck/src/store/fleet_view.rs`; dispatch in `crates/fleetdeck-deck/src/link/requests.rs`)

In this order; each refusal is a link error `Outcome` with the code and the exact text:

1. The link layer has checked the station's key, its role, the scope `admin` and the feature.
2. Deck-wide concurrency: a `Semaphore` of 8; `try_acquire` fails → `busy: the fleet view is busy, try again in a moment` (at once, no queue).
3. Station-wide rate limit, counted before the caller check and also for refused requests: at most 240 views per station per minute → `rate_limited: this station asked for too many fleet views`.
4. `session_known(station, caller)` (the read model, the calling station only) else `forbidden: the session is not known on this station`. A caller ID that exists only on another station is unknown here.
5. Per-session rate limit, counted after step 4: at most 60 views per `(station, caller)` per minute → `rate_limited: at most 60 fleet views per minute`.
6. Build the page (§2.3) from `DeckStore` data. The limiter maps are bounded: entries older than the window are removed on each admission; a map holds at most 4096 keys (above it, the oldest key is removed).
7. No audit row (a read); a debug log line with the station, the caller and the view name only. A view never touches `last_used_at` of any grant.

### 2.3 The three views (exact fields, order, paging)

Common: every text field is cut to its limit by characters (`name` 128, `title` 200, project `name` 128; a cut text ends with `…`). A page is at most 256 KiB of JSON; the page size below keeps it under that. The cursor encodes the sort key of the last item (base64url of `<key>\n<id>`), so adds and removals between pages never repeat or skip a remaining item.

- `stations`: every paired station, ordered by `(name, station_id)`, 32 per page. Each:
  `{"station_id", "name", "online": bool (the reachability set), "platform": "omarchy" | "macos" | <traits.os> | null, "arch": <traits.arch> | null, "desktop": "omarchy" | "hyprland" | null, "providers": [{"instance", "provider", "version" | null, "signed_in": bool}], "live_sessions": u64, "max_sessions": u32 | null, "projects": [{"project_id", "name"}], "projects_more": bool}`.
  `platform`: `"omarchy"` when `desktop.kind` is Omarchy; else `"macos"` when `traits.os == "macos"`; else `traits.os`; `null` without traits. `projects`: the projects whose `station_id` is this station or whose `homes` include it, in `(name, project_id)` order, at most 20; `projects_more` is true when there are more (`list_projects` with `inspect` lists all).
- `sessions`: every session of the read model whose card status is not `ended`, ordered by `(station name, callsign, session_id)`, 100 per page. Each: `{"station_id", "station", "session_id", "callsign", "title" | null, "provider", "status"}` (`status`: the card status in snake_case). **Nothing else**: no `repo`, `branch`, `cwd`, `worktree`, `digest`, `focus`, `open_requests`, `running_tools`, `last_error`, `model`, `mode` or any transcript text (R1 BLOCKER).
- `me`: `{"station_id", "station", "session_id", "callsign", "title" | null, "provider", "model" | null, "mode" | null, "grant": null | {"grant_id", "classes": [..], "stations": [names], "scope", "spawn_cap", "spawn_used", "max_modes": {..}, "expires_at"}}`. `grant` is the in-effect grant of exactly this `(station, caller)` (`caller_grant` + in effect), else `null`.

## 3. Station side (`crates/fleetdeck-station/src/coordinator/`)

### 3.1 The tool `fleet_view` and the resources (every binding)

- **Tool** `fleet_view` (class `Own`; annotations read-only, `openWorldHint` false): input `{"view": "stations" | "sessions" | "me", "cursor"?: string}`; description "Read the fleet: the stations and their platforms, the live sessions (titles and states), or yourself and your grant. Pages: pass next_cursor back as cursor." It is granted to every binding (like `publish_artifact`). The Claude allow list `mcp__fleetdeck` already covers it (R1 UNVERIFIED 1: the bare resource tools are **not** added to `--allowedTools`).
- **Resources** (for clients that use them; the tool is the main path): `StationMcp` advertises `resources` (no `subscribe`, no `listChanged`) and implements:
  - `list_resources`: exactly four entries, fixed order: `fleet://guide` (`text/markdown`), `fleet://stations`, `fleet://sessions`, `fleet://me` (`application/json`). Modern-era hints: `cacheScope` private, `ttlMs` 60 000.
  - `list_resource_templates`: empty list.
  - `read_resource(uri)`: `fleet://guide` → the binding's guide text (§5.2), no deck call. `fleet://stations`, `fleet://sessions`, `fleet://me`, each with an optional `?cursor=<value>` → one `fleet.view` with the binding's caller; the text is the compact JSON `{"items", "next_cursor"}`. Modern-era hints: `cacheScope` private, `ttlMs` 30 000 (stations) or 10 000 (sessions, me). Any other URI → rmcp `resource_not_found`.
- **Both paths** use the binding's `Caller.session_id` only (no argument, URI part or `_meta` names the caller). A pending binding gets the text `{"error": "not_bound: this session is still starting. Try again in a few seconds."}` (as a tool error for the tool); no deck link → `unavailable: the deck is not connected`; a link without `fleet_view.v1` → `unavailable: the deck does not serve the fleet view yet` (nothing is sent); a deck refusal → `<code>: <text>`.
- The forward: a new `Forward::fleet_view(caller, view, cursor) -> Result<Value, String>` (default `unavailable: …`), implemented by `LinkForward` with a 10 s deadline, without the outbox and without the lifecycle fence.

### 3.2 Prompts (only Claude uses them)

`StationMcp` advertises `prompts`. A prompt is listed (and `get_prompt` answers it) only when the binding's catalog has every tool that its text names; otherwise `get_prompt` answers rmcp `invalid_params` "unknown prompt". Argument values are strings.

| Prompt | Needs (all in the catalog) | Message |
| --- | --- | --- |
| `report` | `publish_artifact`, `set_focus` | "Write a short report of what you did and what is open. Publish it with publish_artifact (name report.md), then set your card line with set_focus." |
| `validate_on_platform` (argument `platform`, required) | `fleet_view`, `assign`, `wait_tasks`, and `start_in_project` or `spawn` | "Validate the current work on {platform}. Call fleet_view with view stations and pick an online station with platform {platform}. Start one session there with {starter}, assign it the validation task with assign, wait with wait_tasks, then read its report and tell me the result. If no such station is online, say so and stop." where `{starter}` is `start_in_project (or spawn when there is no project)` when both are in the catalog, else the one that is. |
| `delegate` (arguments `task` required, `station` optional) | `fleet_view`, `assign`, `wait_tasks` | "Give this task to another session: {task}. Call fleet_view with view sessions and pick an idle session{ on station {station}}, then assign the task with assign{ (or use assign_auto)}. Wait with wait_tasks and report the result." — the `assign_auto` part only when the catalog has it. |

### 3.3 Generated instructions (replace the control skill as `instructions`)

`initialize` and `discover` serve `instructions` built from the binding's catalog and the station's own facts, in this order, each line only when its condition holds (at most 8 sentences):

1. Always: `You are a Fleet Deck session on station {name} ({platform}).` (`{platform}` by the rule of §2.3 from the station's own desktop kind and `std::env::consts::OS`.)
2. Always: `Call fleet_view to see the stations and their platforms (view stations), the live sessions (view sessions), and yourself and your grant (view me).`
3. One line built from the catalog (R1 MAJOR 5): the verbs present, in this order, joined as one sentence: `You may` + `start sessions with {spawn|start_in_project}` (when present) + `give work with assign and wait with wait_tasks` (only when both are present; else the one present: `give work with assign` / `wait for tasks with wait_tasks`) + `stop the sessions that you started with stop` (when present) + `.`; when none of these tools is present: `To get help from another station, ask your coordinator with ask_coordinator or tell the owner.`
4. Always: `Publish reports with publish_artifact and keep your card line current with set_focus.`
5. Always (the mandatory safety preamble, R1 MAJOR 3): `Never ask for, print or pass on a credential. A refused call (forbidden, expired, revoked) is final: do not retry it in a loop; tell the owner what you need. Use only the tools in your tool list.`
6. When `command_status` is in the catalog: `A call that answers pending is not finished: check it with command_status.`
7. When the binding has a grant: `Read fleet://guide (or call fleet_view and read the guide resource) before you coordinate other sessions.`

The deck's `control_skill` text stays in `CoordinatorSpec` and is the guide (§3.1, §5.2). ADR-006 §16 gets a note (the instructions are generated; the control skill is the guide).

## 4. The fleet skill (injected at start, every session)

### 4.1 Publication (R1 MAJOR 4)

- The content (two files, embedded with `include_str!`): `.claude-plugin/plugin.json` = `{"name":"fleetdeck","description":"Fleet Deck: work across the stations of this fleet","version":"<CARGO_PKG_VERSION>"}` and `skills/fleet/SKILL.md` (§4.3). The tree hash is the SHA-256 over the two relative paths and contents in a fixed order.
- Location: `<station dir>/share/fleet-skill/<hash>/`. Before any write, the station checks with no-follow `lstat` that `<station dir>`, `share` and `share/fleet-skill` are real directories (not symlinks), owned by the station user, with no group or other write bit; it creates a missing `share` or `fleet-skill` with mode 0700. Any failed check → no injection for any session of this station run, and an error log line.
- Publication: if `<hash>/` exists, verify it (no-follow): exactly the directories `.claude-plugin`, `skills`, `skills/fleet` and the two files, regular files, the station user's, no group/other write, contents equal to the embedded bytes; a mismatch → rename it to `<hash>.bad-<unix ms>` and publish again. To publish: create `.tmp-<random>/` with mode 0700 (exclusive), create the directories and files inside with `O_EXCL | O_NOFOLLOW` (files 0644, dirs 0755 after the write), `fsync` the files and directories, then `rename` the temp directory to `<hash>` (if another writer won the rename, verify theirs as above and remove ours).
- The provider gets the absolute path of `<hash>/` (Claude) or `<hash>/skills` (Codex). Trees of older hashes stay; the station removes trees other than the current one only at its start and only when no session of the station is live (no shim-hosted provider survives).

### 4.2 Injection (only for a probed provider version at or above the verified minimum: Claude 2.1.302, Codex 0.163.0; below it, no injection and a station log line)

- Claude (`crates/fleetdeck-providers/src/claude/`): add `--plugin-dir <abs path of the tree>` to every start and resume, and add `Skill(fleetdeck:fleet)` to the existing `--allowedTools` value (nothing else is added; R1 UNVERIFIED 1). The live verification (§6) checks that this exact rule allows the fleet skill without an approval and does not allow another skill; if it does not, the architect removes the rule before merge (the skill then needs an approval in modes that ask).
- Codex (`crates/fleetdeck-providers/src/codex/`), before every initial thread open (start, resume, fork, and the shim host's open), inside the existing handshake deadline: `skills/extraRoots/set {"extraRoots": ["<abs path>/skills"]}` and, after its answer, `skills/list {"forceReload": true}` (R1 UNVERIFIED 2). A failure of either is logged and does not stop the open.
- The adapters get the tree path from the station (a new field of the start context; no environment variable).

### 4.3 SKILL.md (exact)

```
---
name: fleet
description: Use when a task needs another machine or platform (macOS, Omarchy, Linux), more hands, or a second opinion from another session in this Fleet Deck fleet.
---

# Working across the fleet

This fleet has several stations (machines). Each can run Claude and Codex sessions.

1. Call `fleet_view` with `view: "stations"`. It lists every station, its platform (`macos`, `omarchy`, `linux`), whether it is online, its providers and some of its projects. Pass `next_cursor` back as `cursor` for more.
2. Call `fleet_view` with `view: "me"` to see your grant. Without a grant you cannot start or drive sessions: ask your coordinator with `ask_coordinator`, or tell the owner what you need and on which station.
3. If your tool list has `start_in_project` or `spawn`: start a session on the station that you need (`start_in_project` when the repository is a project there).
4. If your tool list has `assign`: give the session one small, testable task and say what to report.
5. If your tool list has `wait_tasks`: wait for the task. A task in `question` needs your answer by `send_mail`.
6. Read the result: the worker publishes a report with `publish_artifact`. If your tool list has `read_artifact`, read it; otherwise ask for a summary in the task result.
7. If your tool list has `stop`: stop the sessions that you started when they are done.

Rules: never start a session in a bypass mode; never ask for, print or pass on a credential; a refused call is final, so tell the owner instead of retrying.
```

## 5. Station details

### 5.1 Capabilities
`get_info` advertises `tools`, `resources` and `prompts`. P1's method-not-found overrides of `list_prompts`, `list_resources` and `list_resource_templates` are replaced; `complete` stays method-not-found.

### 5.2 The guide
For a grant binding the guide is the deck's `control_skill`; for a grant-less binding it is `FLEET_SKILL` (coordinator_start.rs) plus "To work on another station, call fleet_view and ask your coordinator or the owner."

## 6. Tests (names are binding)

Deck (`fleetdeck-deck`):
1. `the_fleet_view_answers_stations_sessions_and_me_without_a_grant` (a station with no grant and no `coordinate`; exact key sets of §2.3; an `ended` session is not listed; seeded excluded fields — a credential-bearing origin `https://x/repo?access_token=SENTINEL`, a local repo root, a branch `SENTINEL-branch`, a `cwd`, a `last_error` — never appear in any page).
2. `paging_lists_every_station_and_session_above_one_page_with_concurrent_changes` (70 stations, 250 sessions; adds and removals between pages; no repeat, no skip of a remaining item; the 256 KiB page bound; long texts are cut with `…`).
3. `the_fleet_view_limits_hold` (the busy semaphore answers at once; the station limit counts refused and rotating callers; the per-session limit; an unknown caller costs a station slot and no session slot; the limiter maps stay at most 4096 keys).
4. `the_caller_is_checked_on_the_calling_station_only` (the same session ID on two stations; a caller of another station is `forbidden`; `me` shows only the grant of `(station, caller)` and does not change `last_used_at`).
5. `the_platform_rule_maps_omarchy_macos_and_linux` (unit).
6. Link (`fleetdeck-link`): `fleet_view_is_refused_for_a_wrong_role_a_missing_admin_scope_or_a_missing_feature` (real link tests, both directions), plus the method table agreement.
Station (`fleetdeck-station`):
7. `every_binding_has_the_fleet_view_tool_and_the_four_resources` (both eras; HTTP and bridge; the guide needs no deck).
8. `the_fleet_view_forwards_with_the_binding_caller_only` (forged session IDs in the arguments, the URI, `_meta` and `clientInfo` change nothing; parallel bindings A/B stay isolated; a missing or revoked credential gets the uniform 401 before any read).
9. `a_pending_binding_no_deck_or_an_old_deck_reads_an_error_and_sends_nothing` (a link without `fleet_view.v1`: zero `fleet.view` requests leave the station).
10. `prompts_are_listed_only_with_every_tool_that_they_name` (for grants: own only; mail only; spawn only; spawn + manage; mail + read + spawn; full).
11. `the_instructions_follow_the_catalog_and_always_carry_the_safety_preamble` (the same grants; every tool name in the text is in that catalog; the preamble in both eras and in pending, active, expired-and-renewed states).
12. `the_fleet_skill_is_published_content_addressed_and_verified` (symlinks at `share`, `fleet-skill`, the tree and each file; an extra `hooks/hooks.json` or `.mcp.json` in an existing tree → renamed `.bad-*` and republished; two publishers at once; an old tree kept while a session is live).
13. The P1 tests that pin the old `instructions` or the method-not-found answers of the resource and prompt methods change to the new behaviour; each change is listed in the report.
Providers (`fleetdeck-providers`):
14. `claude_starts_with_the_plugin_dir_and_only_the_fleet_skill_allowed` (argument builder; no bare resource tool in `--allowedTools`).
15. `codex_sets_the_skill_root_and_reloads_before_every_thread_open` (start, resume, fork; a failure does not stop the open; bounded by the handshake deadline).
16. `an_unverified_provider_version_gets_no_injection`.
Mixed versions: 17. `an_n_minus_1_deck_and_an_n_minus_1_station_degrade_cleanly` (no feature: the tool and resources answer `unavailable …`, nothing is sent; an N−1 station never sends `fleet.view`).
Live (architect, after the build; the station live test extended): one Claude and one Codex session on a test station call `fleet_view` (stations) and name a platform, and list the `fleet` skill; Claude in `default` mode uses `Skill(fleetdeck:fleet)` without an approval and is asked for approval for another skill.

## 7. Risks

- A grant-less session learns the station names, platforms, provider versions, project names and the titles of all live sessions (D2, owner decision). Titles can carry task words; no other content leaves the deck.
- The caller of `fleet.view` is the station's claim (ADR-006 §3): a same-station process can learn only what any session of that station can learn.
- The tool, the resource listings and the skill listing add a few hundred tokens per session.

## 9. Review map (R1 → revision 2)

| Finding | Section |
| --- | --- |
| BLOCKER — `repo`/`branch` exceed D2 and can leak credentials | §2.3 `sessions` (removed), test 1 |
| MAJOR — rate limits do not bound the deck | §2.2 steps 2, 3, 5, 6; §2.3 size limits; test 3 |
| MAJOR — silent truncation | §2.1 `cursor`, §2.3 paging, test 2 |
| MAJOR — safety rules optional | §3.3 lines 5–6, test 11 |
| MAJOR — plugin publication | §4.1, test 12 |
| MAJOR — guidance does not match permissions | §3.2 table, §3.3 line 3, §4.3 conditional steps, tests 10, 11 |
| MAJOR — tests miss the boundary and mixed versions | tests 3, 4, 6, 8, 9, 17 |
| UNVERIFIED — bare resource tools in the allow list | §3.1 tool `fleet_view`, §4.2 (only `Skill(fleetdeck:fleet)`), test 14, live check |
| UNVERIFIED — Codex skill reload | §4.2 (`forceReload` before every thread open), test 15 |
