# MCP P3 — every session is fleet-aware: the fleet view, resources, prompts, short instructions, the fleet skill

- **Status:** Draft revision 1, 2026-10-19. Needs an adversary DESIGN REVIEW (a new read path for sessions without a grant, D2) before it freezes.
- **Program:** DL-251 (owner decisions D1–D3), DL-253 (P1). P3 comes before P2 (D3).
- **Evidence:** `spikes/s-aware/README.md` (live: Claude 2.1.302 opens with `server/discover`, reads MCP resources with `ReadMcpResourceTool`, expands MCP prompts as slash commands, loads a `--plugin-dir` skill; Codex 0.163.0 opens with `initialize` `2025-06-18`, reads resources with `list_mcp_resources`/`read_mcp_resource`, never uses prompts, takes a skill root with the app-server request `skills/extraRoots/set`; both follow MCP `instructions`). The D2 data path: `~/fleetdeck-briefs/mcp-p3-research-notes.md` §"D2 data path".
- **Out of scope:** completions; output schemas for deck-made tool results; P2 events (a resource change is not pushed yet: the TTLs of §4 bound staleness); the default worker grant (P4).

## 1. Goals

1. Every session, with or without a grant, can read (D2): the stations (name, platform, online state, providers, projects), itself, and the session list (titles and states, no content).
2. Every session gets, at start and without any write into its workspace or the user's provider configuration: short instructions that name the fleet resources, and the fleet skill (how to work across stations).
3. The long deck control skill leaves the per-turn `instructions` and becomes a resource (`fleet://guide`), loaded on demand.

## 2. The fleet view (D2) — link method `fleet.view`

Grant-less bindings cannot send `coordinator.query` (it needs the link scope `coordinate`, which only stations named in a grant hold). The fleet view is a new method on the scope that every paired station holds.

### 2.1 Wire (`crates/fleetdeck-proto`, `crates/fleetdeck-link`)

- Method row (`methods_table_tail.rs`): `fleet.view`, `StationToDeck`, `Scope::Admin`, not mutating (no receipt), feature `fleet_view.v1` (new constant in `crates/fleetdeck-proto/src/version.rs`, negotiated like the other features). A peer without the feature never gets the request; the station then answers the resource read with the tool-style error text `unavailable: the deck does not serve the fleet view yet`.
- Params `FleetViewRequest { caller: SessionId, view: FleetView }` with `FleetView` = `stations` | `sessions` | `me` (serde `snake_case`, `deny_unknown_fields`).
- Answer: `FleetViewAnswer` = the JSON value of §2.3 for the view (typed structs in proto, serde, `ts-rs` not needed).

### 2.2 Deck rules (new module `crates/fleetdeck-deck/src/store/fleet_view.rs` and the dispatch in `crates/fleetdeck-deck/src/link/requests.rs`)

1. The link layer has already checked the station's key, the scope `admin` and the feature.
2. `session_known(station, caller)` (the sessions table of the read model) else `forbidden: the session is not known on this station`. The caller identity is the station's claim, as for `ask_coordinator` (the link key authenticates the station; ADR-006 §3 trust boundary).
3. Rate limit: at most 60 views per `(station, caller)` per minute, in memory (an `AskLimiter`-style window); above it `rate_limited: at most 60 fleet views per minute`. A refused view costs no slot.
4. No audit row (a read, like `coordinator.query`); a debug log line with the station, the caller and the view name only.
5. The answer is built from `DeckStore` data at the time of the request (§2.3). No grant is needed and no grant is read, except in `me` (§2.3).

### 2.3 The three views (exact fields)

- `stations`: `{"stations": [ … ]}`, every paired station in name order, at most 64. Each:
  `{"station_id", "name", "online": bool (the reachability set), "platform": "macos" | "omarchy" | "linux" | "windows" | <traits.os> | null, "arch": <traits.arch> | null, "desktop": "omarchy" | "hyprland" | null (DesktopInfo.kind), "providers": [{"instance", "provider": "claude" | "codex" | …, "version" | null, "signed_in": bool}], "live_sessions": <u64>, "max_sessions": <u32> | null, "projects": [{"project_id", "name"}]}`.
  `platform` rule: `"omarchy"` when `desktop.kind` is Omarchy; else `"macos"` when `traits.os == "macos"`; else `traits.os` (`"linux"`, `"windows"`, …); `null` when the station has sent no traits. `projects`: the projects whose `station_id` is this station or whose `homes` include it, at most 50 per station.
- `sessions`: `{"sessions": [ … ]}`, every session of the deck read model whose card status is not `ended`, ordered by station name then callsign, at most 200. Each:
  `{"station_id", "station", "session_id", "callsign", "title" | null, "provider", "status" (CardStatus, snake_case), "repo" | null (the repo origin, for example `github.com/lacion/fleet-deck`, never a path), "branch" | null}`. No `cwd`, `digest`, `focus`, `open_requests`, `running_tools`, `last_error` or any transcript text.
- `me`: `{"station_id", "station", "session_id", "callsign", "title" | null, "provider", "model" | null, "mode" | null, "grant": null | {"grant_id", "classes": [..], "stations": [names], "scope": "lineage" | "fleet", "spawn_cap", "spawn_used", "max_modes": {..}, "expires_at"}}`; `grant` is the caller's in-effect grant (`caller_grant` + in effect), else `null`.

## 3. Station side (`crates/fleetdeck-station/src/coordinator/`)

### 3.1 Resources (every binding)

`StationMcp` advertises `resources` (no `subscribe`, no `listChanged` in P3) and implements:
- `list_resources`: exactly four entries, fixed order: `fleet://guide` (`text/markdown`, "How to work in this fleet"), `fleet://stations` (`application/json`), `fleet://sessions` (`application/json`), `fleet://me` (`application/json`). `cacheScope` private in the modern era (as `tools/list`), `ttlMs` 60 000 for the list.
- `list_resource_templates`: empty list (not method-not-found any more).
- `read_resource(uri)`:
  - `fleet://guide`: the binding's `control_skill` text (the deck control skill for a grant binding; the station's own guide text for a grant-less binding, §5.2). No deck call.
  - `fleet://stations` / `fleet://sessions` / `fleet://me`: one `fleet.view` request over the current deck link with the binding's caller session (`Caller.session_id`); the answer is the resource text (compact JSON). Modern-era hints: `cacheScope` private, `ttlMs` 30 000 (stations), 10 000 (sessions, me).
  - A pending binding (not active) gets the resource text `{"error": "not_bound: this session is still starting. Try again in a few seconds."}` (so the model reads it as data). No deck link → `{"error": "unavailable: the deck is not connected"}`; a deck refusal → `{"error": "<code>: <message>"}`.
  - Any other URI → rmcp `resource_not_found` (`-32602`).
- The forward is a new `Forward::fleet_view(caller, view) -> Result<Value, String>` (default: `unavailable`), implemented by `LinkForward` with a 10 s deadline, without the outbox and without the lifecycle fence (a read).

### 3.2 Prompts (Claude only uses them; every binding lists the ones it may use)

`StationMcp` advertises `prompts` and implements `list_prompts`/`get_prompt` with three prompts (argument values are strings):
- `report` (every binding): no arguments. Message: "Write a short report of what you did and what is open. Publish it with publish_artifact (name report.md), then set your card line with set_focus."
- `validate_on_platform` (bindings with the class `spawn`): argument `platform` (required). Message: "Validate the current work on {platform}. Read fleet://stations and pick an online station with platform {platform}. Start one session there with start_in_project (or spawn when there is no project), assign it the validation task with assign, wait with wait_tasks, then read its report and tell me the result. If no such station is online, say so and stop."
- `delegate` (bindings with `mail`): arguments `task` (required), `station` (optional). Message: "Give this task to another session: {task}. Read fleet://sessions and pick an idle session (on station {station} when given), or use assign_auto. Wait with wait_tasks and report the result."
A prompt that the binding may not use is not listed and `get_prompt` answers rmcp `invalid_params` "unknown prompt".

### 3.3 Generated instructions (replaces the control skill as `instructions`)

`initialize` and `discover` serve `instructions` built by the station from the binding and the station's own facts, at most 5 sentences:

1. `You are a Fleet Deck session on station {name} ({platform}).` — `{name}` the station name; `{platform}` from the station's own desktop kind and `std::env::consts::OS` with the rule of §2.3.
2. `Read fleet://stations for the other stations and their platforms, fleet://sessions for the other sessions, and fleet://me for your own grant.`
3. Only when the binding has `spawn`: `You may start sessions on your grant's stations with spawn or start_in_project; each start uses one unit of your session limit.` Else only when it has `mail`: `You may give work to other sessions with assign and wait for it with wait_tasks.` Else: `To get help from another station, ask the owner or your coordinator (ask_coordinator).`
4. `Publish reports with publish_artifact and keep your card line current with set_focus.`
5. `Read fleet://guide before you coordinate other sessions.`

The deck's `control_skill` text stays in `CoordinatorSpec` and is served as `fleet://guide` (§3.1). This changes ADR-006 §7.1 (the control skill was the `instructions`); ADR-006 §16 gets a note.

## 4. The fleet skill (injected at start, every session)

### 4.1 Files

The station materializes a fixed plugin directory at start (an atomic rewrite of `<station dir>/share/fleet-skill/`, mode 0755 dirs and 0644 files, owned by the station user; the content is embedded in the binary with `include_str!`):
- `.claude-plugin/plugin.json`: `{"name": "fleetdeck", "description": "Fleet Deck: work across the stations of this fleet", "version": "<CARGO_PKG_VERSION>"}`.
- `skills/fleet/SKILL.md` with frontmatter `name: fleet`, `description: Use when a task needs another machine or platform (macOS, Omarchy, Linux), more hands, or a second opinion from another session in this Fleet Deck fleet.` and the body of §4.3.

### 4.2 Injection (providers; only for a probed provider version at or above the verified minimum: Claude 2.1.302, Codex 0.163.0; below it, no injection and a station log line)

- Claude (`crates/fleetdeck-providers/src/claude/`): add `--plugin-dir <station dir>/share/fleet-skill` to every start and resume, and extend the existing `--allowedTools mcp__fleetdeck` with `ListMcpResourcesTool`, `ReadMcpResourceTool` and `Skill(fleetdeck:fleet)` (one argument, the same list syntax as today).
- Codex (`crates/fleetdeck-providers/src/codex/`): after the app-server `initialize` and before the first `thread/start`, send `skills/extraRoots/set {"extraRoots": ["<station dir>/share/fleet-skill/skills"]}`; a failure is logged and does not stop the start.
- The adapters get the directory from the station (a new field of the start context; no environment variable).

### 4.3 SKILL.md body (exact)

```
# Working across the fleet

This fleet has several stations (machines). Each can run Claude and Codex sessions.

1. Read `fleet://stations` (an MCP resource of the `fleetdeck` server). It lists every station, its platform (`macos`, `omarchy`, `linux`), whether it is online, its providers and its projects.
2. Read `fleet://me` to see your own grant. Without a grant you cannot start or drive sessions: ask your coordinator (`ask_coordinator`) or tell the owner what you need and on which station.
3. With the class `spawn`: start a session on the station that you need with `start_in_project` (when the repository is a project there) or `spawn`. Give it one clear task with `assign`. Keep the task small and testable, and say what to report.
4. Wait with `wait_tasks`. A task in `question` needs your answer by `send_mail`.
5. Read the result: the worker publishes a report with `publish_artifact`; read it with `read_artifact` when you have `inspect`, or ask for the summary in the task result.
6. Stop the sessions that you started when they are done (`stop`).

Rules: never start a session in a bypass mode; never pass on a credential; a refused call is final, so tell the owner instead of retrying.
```

## 5. Station details

### 5.1 Capabilities
`get_info` advertises `tools`, `resources` and `prompts` (P1 advertised only `tools`). P1's method-not-found overrides of `list_prompts`, `list_resources` and `list_resource_templates` are replaced by §3; `complete` stays method-not-found.

### 5.2 The guide of a grant-less binding
`FLEET_SKILL` (coordinator_start.rs) becomes the guide text of a grant-less binding: the current text plus the sentence "To work on another station, read fleet://stations and ask your coordinator or the owner."

## 6. Tests (names are binding)

Deck (`fleetdeck-deck`):
1. `the_fleet_view_answers_stations_sessions_and_me_without_a_grant` (a station with no grant and no `coordinate` scope; exact field sets of §2.3; a session in `ended` is not listed; no excluded field appears).
2. `the_fleet_view_refuses_an_unknown_caller_and_limits_each_session_to_60_per_minute`.
3. `the_platform_rule_maps_omarchy_macos_and_linux` (unit).
4. `me_shows_the_in_effect_grant_and_null_without_one`.
5. The method table agreement tests include `fleet.view` (Admin, StationToDeck, `fleet_view.v1`, not mutating).
Station (`fleetdeck-station`):
6. `every_binding_lists_the_four_fleet_resources_and_reads_the_guide_without_the_deck`.
7. `a_fleet_resource_read_forwards_one_fleet_view_with_the_binding_caller` (a forged session in the URI or `_meta` changes nothing; the caller is the binding's).
8. `a_pending_binding_or_no_deck_reads_an_error_object_not_a_protocol_error`.
9. `prompts_are_listed_by_class_and_get_prompt_fills_the_arguments` (both eras; an unlisted prompt is `invalid_params`).
10. `the_instructions_have_at_most_five_sentences_and_name_only_listed_tools` (for grant-less, mail-only, spawn and full grants: every tool name in the text is in that binding's catalog).
11. `the_fleet_skill_directory_is_written_atomically_with_the_embedded_files`.
Providers (`fleetdeck-providers`):
12. `claude_starts_with_the_plugin_dir_and_the_resource_and_skill_tools_allowed` (argument builder).
13. `codex_sets_the_skill_root_after_initialize_and_a_failure_does_not_stop_the_start` (with the existing app-server test harness).
14. `an_unverified_provider_version_gets_no_injection`.
Live (architect, after the build; the opt-in station live test extended): one Claude and one Codex session on a test station read `fleet://stations` and name a station platform, and list the `fleet` skill.

## 7. Risks

- A grant-less session learns the station names, platforms, provider versions and the titles of all live sessions (D2, owner decision). Session titles can carry task words; no content beyond the title leaves the deck.
- The caller of `fleet.view` is the station's claim; a same-station process can only learn what any session of that station could learn (ADR-006 §3).
- The fleet skill and the resource tools add a few hundred tokens of tool and skill listings per session (the skill listing is about 12 tokens in Claude).
