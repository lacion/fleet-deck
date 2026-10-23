# MCP Apps: interactive mini UIs in the chat (B15)

- **Status:** Draft, revision 2 (2026-10-23, after the adversary design review round 1; the changes are listed in the last section). Risk: **high** (a new command path from the board to a live provider session, untrusted HTML and JavaScript next to the board, a station migration). It needs an adversary DESIGN REVIEW before freeze and a PRE-PR REVIEW before the merge into `v2`.
- **Owner request (B15):** "Study how t3code shows custom mini UIs inside its chat, and decide whether Fleet Deck can do the same and how."
- **Base:** `v2` at `497b2fbc`. The `file:line` references of revision 1 are on that commit; the references added in revision 2 (facts 16 to 20, §4.6.1, the task changes) are on `00055138`.
- **Decision-log numbers:** DL-450 to DL-458 (the entries are at the end of this document).
- **Evidence:** the recordings and the scratch tools are in `~/fleetdeck-briefs/backlog/mcp-apps/` (§2.4 lists them).

## 1. Problem

An MCP server can ship an **MCP App** ([SEP-1865, specification 2026-01-26](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx)): a tool names a `ui://` resource in `_meta.ui.resourceUri`; the host reads that resource (an HTML document, MIME `text/html;profile=mcp-app`), renders it in a sandboxed frame in the chat row of the tool call, and lets the document talk to the host with JSON-RPC over `postMessage`. The document can call tools of its own server (`tools/call`), read resources of its server (`resources/read`), send a chat message (`ui/message`), open a link and change its size.

t3code shows these apps in its threads. Fleet Deck shows only a plain tool row today. The owner wants the same feature.

This document answers the questions of B15 with evidence (§3), and gives a buildable design for a first phase (§4 to §9).

## 2. Verified facts

### 2.1 Fleet Deck on `497b2fbc`

1. **Codex `initialize` declares no MCP extension.** `CodexCore::initialize` sends `"capabilities": {"experimentalApi": true}` only (`crates/fleetdeck-providers/src/codex/core.rs:299-310`).
2. **Codex MCP rows.** `mcpToolCall` maps to `ToolKind::Mcp` with the title `server.tool` and the arguments as the input summary (`crates/fleetdeck-providers/src/codex/map_tools.rs:9-19`, `:27-32`). The completed item gives only `ToolCompleted { status, files, exit_code }` (`crates/fleetdeck-providers/src/codex/map.rs:544-602`). The `result` of the item, `mcpAppResourceUri` and `mcpAppUi` are dropped.
3. **Claude MCP rows.** A tool name that starts with `mcp__` maps to `ToolKind::Mcp` (`crates/fleetdeck-providers/src/claude/map.rs:38`). The `tool_result` block gives the status only (`crates/fleetdeck-providers/src/claude/map.rs:296-345`). The structured `tool_use_result` of the user frame is read only for the subagent `agentId` (`crates/fleetdeck-providers/src/claude/frames.rs:150-160`).
4. **Claude control requests.** The host writes `control_request` frames with any subtype (`crates/fleetdeck-providers/src/claude/control.rs:78-91`). Fleet Deck sends `initialize`, `interrupt`, `set_permission_mode`, `set_model` and `get_usage` today.
5. **Live session operations.** `LiveSession` has no MCP operation (`crates/fleetdeck-providers/src/adapter.rs:326-377`). The station holds the `Live` lock of a session while it dispatches an effect (`crates/fleetdeck-station/src/manager/target.rs:513-547`). The shim host reaches the core with `internal(...)`, `wait_until(...)` and `with_core(...)`; `rollback` is the model of a request with an answer (`crates/fleetdeck-station/src/manager/shim_codex.rs:126-150`).
6. **Non-event outputs of a core.** `Output::NativeIdentity` is an output that is not an event; the direct drivers ignore it and the durable driver passes it through `split()` (`crates/fleetdeck-providers/src/codex/core.rs:22-31`, `crates/fleetdeck-providers/src/codex/driver.rs:467`, `crates/fleetdeck-station/src/manager/shim_codex.rs:22-35`, `crates/fleetdeck-station/src/manager/durable.rs:108-112`).
7. **Events.** An unknown event kind from a newer station passes through an older deck (`crates/fleetdeck-proto/src/event.rs:1-9`). `task.updated` is the latest additive kind (`crates/fleetdeck-proto/src/event.rs:484-504`).
8. **Link frames.** A control frame is at most 1 MiB (`crates/fleetdeck-proto/src/link.rs:17`). Artifact files therefore move in chunks with `artifacts.file` (`crates/fleetdeck-deck/src/http/artifact_stream.rs:57-80`).
9. **Link methods.** A side-effect method that is not a durable command can be non-mutating: `desktop.act` is `Scope::Drive`, not mutating (`crates/fleetdeck-link/src/methods_table_tail.rs:48-54`). Mutating methods need a `command_id` and a receipt (`crates/fleetdeck-station/src/link/intake.rs:631-660`). Read methods are dispatched in `read()` (`crates/fleetdeck-station/src/link/intake.rs:452-505`).
10. **HTML artifacts.** An HTML artifact answer has `Content-Security-Policy: sandbox allow-scripts allow-popups allow-forms allow-modals; default-src * data: blob: 'unsafe-inline' 'unsafe-eval'` (`crates/fleetdeck-deck/src/http/artifact_routes.rs:73`). The board frame has `sandbox="allow-scripts allow-popups allow-forms allow-modals"` and no `allow-same-origin` (`web/src/artifacts/ArtifactViewer.tsx:36`, `:44-55`). The board reaches the document through a view link `/a/{token}/` that lives one hour (`crates/fleetdeck-deck/src/http/artifact_routes.rs:66`, `:440-474`).
11. **Wall artifacts are inert.** The display artifact answer has `sandbox` without `allow-scripts` (`crates/fleetdeck-deck/src/http/display_artifact.rs:49`), as §5E item 3 of `omarchy.md` requires.
12. **Board policy.** The board CSP has `frame-src 'self'` and `frame-ancestors 'none'` (`crates/fleetdeck-deck/src/http/assets.rs:40-42`). A frame of the board can therefore load and navigate only to deck URLs.
13. **Owner-only session reads.** `GET /api/sessions/{station}/{session}/subagents/{tool_use_id}` is the pattern: `owner_only`, the link read with a feature check, an audit row (`crates/fleetdeck-deck/src/http/session_work_routes.rs:263-296`, route at `crates/fleetdeck-deck/src/http/mod.rs:572`).
14. **Version gates.** `SKILL_VERIFIED` gates a feature by provider family and minimum version (`crates/fleetdeck-providers/src/coordinator.rs:203-215`).
15. **Module size.** Every Rust and TypeScript module stays under 800 lines (`tools/module-size.sh`). Files near the limit: `codex/core.rs` 782, `codex/map.rs` 768, `link/intake.rs` 768, `manager/policy.rs` 785, `manager/mod.rs` 797, `manager/target.rs` 792, `manager/shim_target.rs` 791, `deck/http/methods_table_rest.rs` 792, `store/shell_runs.rs` 784, `testkit/sim_codex.rs` 775, `web/src/chat/ChatView.tsx` 715. New code goes into new files.
16. **Workspace protection (revert, B8).** A revert holds the workspace fence of its worktree root over the whole execution (`crates/fleetdeck-station/src/manager/revert_run.rs:126`); the fence is shared by every session in one root (`crates/fleetdeck-station/src/manager/turns.rs:50-71`). Before the fence wait it checks the durable shell barrier (`revert_run.rs:75-90`, `:114-123`): `Running` while a `shell_runs` row of the root is unsettled, `RanAfterTurn` when the folder generation (`worktree_generations`) is above the generation of the target turn's start checkpoint (`crates/fleetdeck-station/src/store/shell_runs.rs:428-451`). A shell command advances the generation at its start and again after its settlement under the fence (`store/shell_runs.rs:328-358`, `manager/shell_advance.rs:31-47`); the station start settles the rows of the last process (`manager/shell_advance.rs:55-63`, called from `manager/mod.rs:381`). The barrier is **not** checked again under the fence: today only effects of the same session's worker race with a revert. The board shows a revert reason as its code (`web/src/session/workspace.test.tsx:380`).
17. **Board-API method table.** The deck guard answers 404 `not_found` for a matched route that is not in `methods::find` (`crates/fleetdeck-deck/src/http/guard.rs:505-516`). A row declares the scope and `mutating`; a mutating row with a cookie login needs `X-CSRF-Token` (`crates/fleetdeck-deck/src/http/methods.rs:9-31`, `guard.rs:425-440`). The tables are split into files joined in `methods::api()` (`methods.rs:84-104`).
18. **Shim restore.** A station shutdown detaches every shim session and leaves its provider running (`crates/fleetdeck-station/src/manager/target.rs:253-281`). The next start restores the driver, which replays the output of the down time (`crates/fleetdeck-station/src/manager/shim_station.rs:179-240`), and `SessionTarget::adopt` inserts the session with `insert_durable` (`crates/fleetdeck-station/src/manager/shim_target.rs:133-178`). `adopt` runs before `shell_advance::start_up` (`manager/mod.rs:357-381`).
19. **Exhaustive event matches.** `SessionSummary::fold` matches every `Event` variant without a wildcard (`crates/fleetdeck-proto/src/fold.rs:38-335`), and `Event::kind_name` plus `Event::KINDS` list every kind (`crates/fleetdeck-proto/src/event_kinds.rs:8`, `:74-131`).
20. **Claude MCP tool names.** Claude builds `mcp__{n(server)}__{n(tool)}` with `n` = every character outside `[A-Za-z0-9_-]` becomes `_`. Its `mcp_call` takes **only** that full name and parses it as `split("__")`: the server is the second part, the tool is the rest joined with `__` (function `Jr` in the 2.1.308 binary). So `(a, b__c)` and `(a__b, c)` give the same name, and Claude resolves it to the server `a`. `mcp_status` lists the raw server names and the raw tool names; the `can_use_tool` request names the raw server in `mcp_server.name` (recorded, `rec-claude-turn.jsonl` frames 8 and 31).

### 2.2 Corrections to B15

| B15 says | Fact on `497b2fbc` with Claude 2.1.308 and Codex 0.163.0 |
| --- | --- |
| "Provider coverage: Codex only. Claude's SDK has no resource read yet." (t3code, 2026-10-07) | **Wrong for Claude 2.1.308.** Its stream-json control channel has `mcp_read_resource` (ui:// only), `mcp_call` (an MCP tool call without a model turn) and `mcp_status` with the `_meta.ui` of each tool. `system/init.capabilities` advertises `mcp_read_resource_v1`, `mcp_tool_ui_meta_v1` and `mcp_call_result_not_for_model_v1`. The environment variable `CLAUDE_CODE_MCP_APPS_HOST=1` makes Claude offer the `io.modelcontextprotocol/ui` extension to its MCP servers. Recorded live (§2.4). |
| "HTML artifacts … can be the base of an app frame." | **Partly.** The opaque-origin frame and the token route can be reused. The artifact CSP (`default-src *`, `'unsafe-eval'`, popups) cannot: an app needs a CSP built from its own `_meta.ui.csp` with no network by default (spec "Host Behavior"). |
| "The R13 wall and the P6 previews … rules for inert artifact documents" | **True, and it blocks apps on the wall today**: a wall document runs no script (fact 11). An app on the wall needs its own design (§3.7). |
| t3code later fixes: #17026, #17027, #17040, #18548, #20691; open #22535 | **True.** #17026 sets the color scheme inside the app document; #17027 adds an empty `document.cookie` facade (analytics code throws in an opaque origin); #17040 sizes mobile apps from `size-changed`; #18548 opens full screen only after a tap inside the app; #20691 is a React Compiler change; #22535 (open) maps Codex connector tool names. |
| The cost note of 0e6c3600 | Not related to B15. |

### 2.3 t3code (pingdotgg/t3code, read with `gh api` and a sparse clone at `66ee628e`)

- **Capture** (`apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts:232-236`, `:493-497`, `:3796-3925`): Codex is initialized with `extensions: {"io.modelcontextprotocol/ui": {mimeTypes: ["text/html;profile=mcp-app"]}}`. On a completed `mcpToolCall` with a `ui://` URI (`mcpAppUi.resourceUri ?? mcpAppResourceUri ?? appContext.resourceUri`), it calls `mcpServer/resource/read {threadId, server, uri}` with a 20 s cap, stores the HTML (at most 5 MiB) as a thread attachment with a CSP `<meta>` in front of the document, and holds the turn open until the capture lands. An interrupted or failed turn cancels the capture.
- **CSP** (`packages/shared/src/mcpApp.ts`): `default-src 'none'`; scripts and styles `'unsafe-inline'` plus `resourceDomains`; `connect-src`, `frame-src`, `base-uri` from the declared domains, else `'none'`; `object-src 'none'`; `form-action 'none'`; **no `'self'`** ("'self' would reach T3's API"). A domain survives only if it matches `^(https?|wss?)://(\*\.)?host(:port)?$`, at most 32 per list.
- **Requests** (`apps/server/src/mcpApps/McpAppRequests.ts`): the app is resolved from its stored tool item, so it reaches only its own server; a tool whose `_meta.ui.visibility` does not contain `app` is refused on the server; a stopped session is reported, never started. Codex methods: `mcpServerStatus/list`, `mcpServer/tool/call`, `mcpServer/resource/read`.
- **Client** (`packages/client-runtime/src/mcpApps/host.ts`, `apps/web/src/components/chat/McpAppFrame.tsx`): messages over 256 KiB and more than 16 pending requests are refused; a message counts only when `event.source` is the frame window; a second `load` of the frame stops the bridge ("left its page"); the frame has `sandbox="allow-scripts allow-forms"`. Consent: a call of a tool not marked `readOnlyHint`, every `ui/message`, every link and every download asks first.
- **Left out on purpose:** picture-in-picture, `sampling/createMessage`, partial tool input, the double-iframe sandbox proxy of the spec, agent tools that drive apps.

### 2.4 Live recordings (2026-10-23, this machine)

A 120-line stdio MCP server (`scratch/demo_app_server.py`) ships one app: `show_counter` (visibility model and app, `readOnlyHint`), `bump` (visibility app only, not read-only) and the resource `ui://demo/counter.html` with `_meta.ui.csp`. It logs every request that its MCP client sends.

| File | What it proves |
| --- | --- |
| `rec-codex-ext1-noturn.jsonl` | Codex 0.163.0 accepts `capabilities.extensions["io.modelcontextprotocol/ui"]`. `mcpServerStatus/list` gives the tools with `_meta.ui` and `annotations`. `mcpServer/resource/read {threadId, server, uri}` returns `contents[{uri, mimeType, text, _meta.ui.csp}]`. `mcpServer/tool/call {threadId, server, tool, arguments}` runs the app-only tool `bump` with **no visibility check and no approval**. An unknown tool and a non-`ui://` URI return the server's own error (Codex does not restrict the URI scheme). |
| `scratch/demo-log-codex-ext1-turn0.jsonl`, `…ext0…` | Codex forwards the extension to the MCP server in its `initialize` only when the app-server client declared it. Without it, a server that follows the spec registers no UI. |
| `rec-codex-ext1-turn.jsonl` | One turn: `item/started` and `item/completed` of `mcpToolCall` carry `mcpAppResourceUri: "ui://demo/counter.html"`, `mcpAppUi: null`, `readOnlyHint: true`, `result: {content, structuredContent, _meta: null}`. The model lists only `show_counter`: Codex hides the app-only tool from the model. |
| `rec-claude-turn.jsonl` | Claude 2.1.308 (`--input-format stream-json`, the Fleet Deck flags): `system/init.capabilities` contains `mcp_read_resource_v1`, `mcp_tool_ui_meta_v1`, `mcp_call_result_not_for_model_v1`. `mcp_status` gives `tools[{name, annotations: {readOnly}, _meta: {ui: {resourceUri, visibility}}}]`. `mcp_read_resource {serverName, uri}` returns the document; a non-`ui://` URI is refused ("uri must use the ui:// scheme"). `mcp_call {tool: "mcp__demo__bump", arguments, result_not_for_model: true}` runs the app-only tool with **no permission check** and returns the raw `content` blocks. The model sees only `mcp__demo__show_counter`. The `can_use_tool` request names `mcp_server.name`. The user frame of the result has `tool_use_result: {content, structuredContent}` and no `_meta`. |
| `scratch/demo-log-claude-turn1.jsonl` | Without the environment variable, Claude offers no `io.modelcontextprotocol/ui` extension to the server. |
| `rec-claude-appshost-noturn.jsonl`, `scratch/demo-log-claude-turn0.jsonl` | With `CLAUDE_CODE_MCP_APPS_HOST=1`, Claude offers `extensions: {"io.modelcontextprotocol/ui": {"mimeTypes": ["text/html;profile=mcp-app"]}}`. |
| `codex-schema/`, `codex-schema-stable/`, `codex-ts/` | `codex app-server generate-json-schema` (with and without `--experimental`) and `generate-ts` of 0.163.0: the three MCP methods and the item fields are in the **stable** API. |
| `claude-2.1.308-mcp-apps-excerpt.txt` | The schema texts of `mcp_read_resource`, `mcp_call` and the `CLAUDE_CODE_MCP_APPS_HOST` code from the 2.1.308 binary. |

The recordings are scrubbed (installation id, session ids, thinking signatures). No Fleet Deck session was started for this exploration: the local CLIs gave all the evidence.

## 3. Answers to the questions of B15

### 3.1 Codex: the `ui://` reference and `mcpServer/resource/read` with the pinned version

Yes for 0.163.0 (stable API, recorded in §2.4). The item carries the URI in `mcpAppResourceUri` (legacy field, set) and `mcpAppUi.resourceUri` (null in 0.163.0); the design reads `mcpAppUi.resourceUri` first. Fleet Deck's tested minimum is 0.160.0 (`crates/fleetdeck-providers/src/codex/mod.rs:64`); no recording exists for 0.160 to 0.162. **Decision:** offer the extension only to Codex at or above 0.163.0 (DL-450). Codex does **not** enforce the visibility rule for `mcpServer/tool/call` and does not restrict the URI of `mcpServer/resource/read`: the station must.

### 3.2 Claude: a way today

Yes, without a station-side MCP client: Claude 2.1.308 has the three control requests (§2.2). **Decision:** phase 1 covers Claude and Codex (DL-450). Claude does not enforce visibility in `mcp_call` and skips its permission check ("the control channel is trusted"): the station and the board must do both. Claude restricts `mcp_read_resource` to `ui://`; the station applies the same rule to both providers.

### 3.3 Routing and principals

Board → deck → station → live provider session, through four new link methods and three deck API routes (§4.6, §4.7). **Owner only** in phase 1: a member of a shared session, a token, a coordinator and a display get 403. The station accepts the methods only from a paired deck link (the normal link authentication) and checks the app row, the session, the unique server and tool pair, the tool visibility and the confirmation. Every app tool call is a durable row that blocks a revert of its folder while the call runs or its end is not proven (§4.6.1). The path never starts, resumes or wakes a session. This is a new command path: adversary DESIGN REVIEW.

### 3.4 Where the app document is stored and what the deck relays

The **station** stores the captured document, the tool input and the tool result in a new table `mcp_apps` (§4.4), not in the artifact store: an app is not a published file, it must not appear in the artifact lists, and it needs its own CSP. The **deck** stores nothing; it relays the metadata (`session.app_meta`) and the document in chunks (`session.app_document`) when the board opens a view, and serves the document once per short-lived token at `/app/{token}` with the app CSP (§4.7).

### 3.5 Can Fleet Deck's own MCP tools ship apps

Technically yes after phase 1: the Fleet Deck MCP server (`crates/fleetdeck-station/src/coordinator/mcp_service.rs`) could return `_meta.ui.resourceUri` on a tool and serve the `ui://` resource, and the generic path would render it; the app's `tools/call` would go back through the provider to the station MCP endpoint under the session's binding and grant. **Decision: not in phase 1** (DL-457). The board already shows the fleet natively; an iframe copy of a board view adds a second UI to keep in step and a circular call path. A later phase can add native result cards for the coordinator tools instead.

### 3.6 Consent

| App request | Phase 1 rule |
| --- | --- |
| `tools/call`, tool with `annotations.readOnlyHint` (Codex) or `annotations.readOnly` (Claude) true | Runs without a prompt. |
| `tools/call`, any other tool (also a tool with no annotations) | The station answers `confirm`; the board asks the owner; the board sends again with `confirmed: true`. |
| `tools/call`, tool of another server, unknown tool, or visibility without `app` | Refused by the station (`tool_not_callable`). |
| `resources/read` | Only `ui://` URIs of the app's own server; no prompt (read-only). |
| `ui/message` | The board asks the owner; on Send it submits the text through the existing composer path (a normal owner prompt or follow-up). No new command path. |
| `ui/open-link` | Only `http:` and `https:`; the board asks the owner, then opens a new tab with `noopener,noreferrer`. |
| `ui/update-model-context`, `ui/download-file`, `ui/request-display-mode` to full screen | Not in phase 1 (§4.9). |

### 3.7 One app host for the chat and the wall (B17 item 4)

**Yes, one host module** (`web/src/apps/bridge.ts`) with a policy object: the chat policy (phase 1) allows the requests of §3.6; a later wall policy allows no `tools/call`, no `resources/read`, no message and no link, and feeds the app the display's own rows as host context. The wall is read-only by construction and its documents are inert today (fact 11); scripts on the wall need their own DESIGN REVIEW (the display principal, the kiosk profile, the `/display` CSP of `omarchy.md` §5F item 3). **Decision:** phase 1 is the chat only; the module is written so the wall can reuse it (DL-456).

## 4. Design (phase 1)

### 4.1 Overview

```
provider tool call completes (Codex mcpToolCall / Claude mcp__ tool_result)
  └─ core: Output::AppCandidate { node, server?, server_hint?, tool, uri?, arguments, result }
       └─ station capture task: resolve the unique (server, tool) pair (Claude: mcp_status + ClaudeToolMap)
            → read resource (20 s) → validate (MIME, ≤ 2 MiB, UTF-8) → sanitize csp
            → INSERT mcp_apps → event tool.app
board timeline: tool row + tool.app → AppFrame
  └─ GET  /api/sessions/{st}/{s}/apps/{app}        (owner) → deck: session.app_meta + view token
  └─ <iframe sandbox="allow-scripts" src="/app/{token}">  → deck: session.app_document chunks, app CSP
  └─ bootstrap (first script of the document) → hello + MessagePort to the board; all traffic on the port
  └─ bridge: ui/initialize, tool-input, tool-result, size, theme
  └─ POST /api/sessions/{st}/{s}/apps/{app}/call   (owner, CSRF) → deck → station: session.app_call
       → fence → INSERT mcp_app_calls (+ folder generation) → AppBridge (Codex mcpServer/tool/call
         | Claude mcp_call) on the live session → settle the row when the call ends (§4.6.1)
  └─ POST …/apps/{app}/calls/{call}/release        (owner, CSRF) → station: session.app_release
```

### 4.2 Provider layer (`fleetdeck-providers`)

New shared module `crates/fleetdeck-providers/src/apps.rs`:

```rust
/// A completed MCP tool call that may name an app (B15). Emitted by a core
/// as `Output::AppCandidate`; the station decides.
#[derive(Debug, Clone, PartialEq)]
pub struct AppCandidate {
    pub node_id: NodeId,
    /// Codex: the item's `server`. Claude: `None` (the station resolves it).
    pub server: Option<String>,
    /// Claude only: the raw server name of the `can_use_tool` request of this
    /// tool use (`mcp_server.name`), when the provider asked. Evidence that the
    /// station's resolution must agree with; `None` for Codex.
    pub server_hint: Option<String>,
    /// Codex: the item's `tool`. Claude: the full name `mcp__<server>__<tool>`.
    pub tool: String,
    /// Codex: `mcpAppUi.resourceUri`, else `mcpAppResourceUri`, only when it
    /// starts with `ui://`. Claude: `None`.
    pub resource_uri: Option<String>,
    /// The tool arguments (JSON object); `Value::Null` when over
    /// `fleetdeck_proto::apps::MAX_APP_INPUT_BYTES` serialized.
    pub arguments: serde_json::Value,
    /// The `CallToolResult` (`content`, `structuredContent`; `_meta` only when
    /// it is an object); `None` when over `MAX_APP_RESULT_BYTES` serialized.
    pub result: Option<serde_json::Value>,
}

/// One tool of one MCP server of a live session.
#[derive(Debug, Clone, PartialEq)]
pub struct AppTool {
    pub server: String,
    pub name: String,
    pub title: Option<String>,
    /// Codex `annotations.readOnlyHint == true`; Claude `annotations.readOnly == true`.
    pub read_only: bool,
    /// `_meta.ui.visibility` when it is an array of strings; `None` = default (model and app).
    pub visibility: Option<Vec<String>>,
    /// `_meta.ui.resourceUri`, else `_meta["ui/resourceUri"]`, when it starts with `ui://`.
    pub resource_uri: Option<String>,
}

/// Why an app operation has no result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    /// The provider answered with an error (the text as the provider sent it).
    /// The operation ended: for a tool call the tool did not run or ran and failed.
    #[error("provider error: {0}")]
    Answered(String),
    /// The provider process ended before an answer. For a tool call this is
    /// the end of the call (its stdio MCP servers end with the provider).
    #[error("the session ended")]
    Ended,
    /// `tools` and `read_resource` only: no answer within their timeout.
    #[error("no answer in time")]
    Timeout,
    /// The provider has no app path (Claude without the three capabilities).
    #[error("not supported by this provider")]
    Unsupported,
}

/// The MCP operations of a live session for its apps. Cloneable handle: it
/// never needs the station's `Live` lock. When the provider process ends,
/// every waiting operation ends with `AppError::Ended`.
#[async_trait]
pub trait AppBridge: Send + Sync {
    /// Waits at most `TOOLS_TIMEOUT`.
    async fn tools(&self) -> Result<Vec<AppTool>, AppError>;
    /// `ReadResourceResult` JSON (`{ "contents": [...] }`). Waits at most `READ_TIMEOUT`.
    async fn read_resource(&self, server: &str, uri: &str) -> Result<serde_json::Value, AppError>;
    /// `CallToolResult` JSON. **No timeout of its own:** it waits for the
    /// provider's answer or the end of the process, so the station always
    /// learns when a call ended (§4.6.1). The station passes only a pair that
    /// it validated (Claude: `ClaudeToolMap::address`).
    async fn call_tool(&self, server: &str, tool: &str, arguments: serde_json::Value)
        -> Result<serde_json::Value, AppError>;
}

pub const TOOLS_TIMEOUT: Duration = Duration::from_secs(10);
pub const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the station waits for a tool call before it answers `timeout`;
/// the call itself goes on (§4.6.1).
pub const CALL_REPLY_WAIT: Duration = Duration::from_secs(60);
/// The provider families and minimum versions with a recorded app path.
pub const APPS_VERIFIED: [(ProviderKind, &str); 2] =
    [(ProviderKind::Claude, "2.1.308"), (ProviderKind::Codex, "0.163.0")];
pub fn apps_verified(kind: ProviderKind, version: Option<&str>) -> bool;
/// Claude's normalization of one name part: every character outside
/// `[A-Za-z0-9_-]` becomes `_`.
pub fn claude_part(name: &str) -> String;
/// Claude's tool name of an MCP tool: `mcp__{claude_part(server)}__{claude_part(tool)}`.
pub fn claude_tool_name(server: &str, tool: &str) -> String;
/// Claude's own parse of a full name (fact 20): `split("__")`; `None` unless the
/// first part is `mcp` and the second part is not empty; the tool is the rest
/// joined with `__` (`None` when there is no rest).
pub fn claude_parse(full: &str) -> Option<(String, Option<String>)>;
/// `"text/html;profile=mcp-app"` after removing whitespace and lowercasing.
pub fn is_app_mime(mime: &str) -> bool;

/// The canonical map between Claude full names and the (raw server, raw tool)
/// pairs of one `tools()` answer. Built fresh from each answer; never "the first match".
pub struct ClaudeToolMap { /* full name → Vec<(server, tool)>; normalized server → Vec<server> */ }

/// The answer of a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeName {
    /// Exactly one pair, and the pair is addressable (below).
    Unique { server: String, tool: String },
    /// Two or more pairs give this name, or the pair is not addressable.
    Ambiguous,
    /// No pair gives this name.
    Unknown,
}

impl ClaudeToolMap {
    pub fn build(tools: &[AppTool]) -> Self;
    /// The pair of a full name (capture of a Claude candidate).
    pub fn resolve(&self, full: &str) -> ClaudeName;
    /// The full name of a pair when `resolve` of that name gives exactly this
    /// pair back; else `None` (an app call of that pair is `tool_not_callable`).
    pub fn address(&self, server: &str, tool: &str) -> Option<String>;
}
```

A pair `(server, tool)` is **addressable** only when all of these are true:

1. No other pair of the answer has the same `claude_tool_name` (cross-server and same-server collisions, for example `(a, b__c)` and `(a__b, c)`, or `my.server` and `my_server`).
2. No other server of the answer has the same `claude_part(server)`.
3. `claude_parse(claude_tool_name(server, tool))` is `Some((claude_part(server), Some(claude_part(tool))))`. So a pair whose normalized server name is empty, contains `__` or ends with `_`, or whose tool name is empty, is never addressable, because Claude's own parse would pick another server or no tool.

A name that is not `Unique` gives no capture and no call. The station never picks a pair by itself.

`LiveSession` gets two default methods (`crates/fleetdeck-providers/src/adapter.rs`):

```rust
/// The app operations of this session (B15); `None` when the provider has none.
fn app_bridge(&self) -> Option<Arc<dyn AppBridge>> { None }
/// The candidates of completed MCP tool calls; the station takes it once after the start.
fn take_app_candidates(&mut self) -> Option<mpsc::Receiver<AppCandidate>> { None }
```

`ProviderAdapter` gets `async fn offers_mcp_apps(&self) -> bool { false }`.

**Codex** (new files `codex/apps.rs`, `codex/core_apps.rs`, `codex/driver_apps.rs`, `codex/tests_apps.rs`):
- `CodexCore` gets `offer_mcp_apps: bool` (`#[serde(default)]`) and `pub fn set_offer_mcp_apps(&mut self, on: bool)`. With it, `initialize` adds `"extensions": {"io.modelcontextprotocol/ui": {"mimeTypes": ["text/html;profile=mcp-app"]}}` to the capabilities.
- `CodexAdapter::offers_mcp_apps` runs `codex --version` once (cached in the adapter, `run_probe`) and answers `apps_verified(Codex, version)`. The direct driver calls it before `initialize` and calls `set_offer_mcp_apps`.
- `Output::AppCandidate(AppCandidate)` is a new variant of `codex::Output`. `tool_completed` calls `apps::candidate(item, node_id)` when the item is an own-thread `mcpToolCall` with status `completed`, no `error`, a `result`, and a `ui://` URI; the candidate is pushed after the `ToolCompleted` event.
- App requests: `CodexCore::app_request(op) -> (String, Vec<Output>)` writes `mcpServerStatus/list {threadId, detail: "toolsAndAuthOnly"}` (follow `nextCursor` at most 5 pages), `mcpServer/resource/read {threadId, server, uri}` or `mcpServer/tool/call {threadId, server, tool, arguments}` with a host id `fd-app-<n>`. Answers go into `app_answers: HashMap<String, Result<Value, String>>` (`#[serde(skip)]`, so a checkpoint never holds them); `take_app_answer(id)` removes one. An answer whose id is unknown is dropped without a warning count (a late answer after a restart). Codex identifies a tool by the separate `server` and `tool` fields, so a Codex pair is always unique; no name map is needed.
- The direct driver implements `AppBridge` on a handle that holds the shared core and the write queue; it waits with the existing notify. `tools` and `read_resource` use their timeouts and remove the pending id on a timeout (`AppError::Timeout`). `call_tool` has no timeout. When the process ends (the driver sees the exit), every pending app request ends with `AppError::Ended` and its id is removed.

**Claude** (new files `claude/apps.rs`, `claude/driver_apps.rs`, `claude/tests_apps.rs`):
- Every Claude launch sets `CLAUDE_CODE_MCP_APPS_HOST=1` in the child environment (direct launch and `durable_launch.rs`). An older CLI ignores the variable.
- The core sets `mcp_apps_ready = true` when the root `system/init.capabilities` contains all of `mcp_read_resource_v1`, `mcp_tool_ui_meta_v1` and `mcp_call_result_not_for_model_v1`. Without it, no candidate and no bridge (`app_bridge()` answers a bridge whose calls fail with `AppError::Unsupported`).
- `Output::AppCandidate` is a new variant of `claude::Output`. On a `tool_result` block of the main thread (`parent_tool_use_id` null) for a tool node whose name starts with `mcp__`, status `Ok`, the core emits a candidate with `server: None`, `tool` = the full name, `arguments` = the tool input, `result` = `tool_use_result` of the user frame normalized to a `CallToolResult`: a string `content` becomes `[{"type":"text","text":<string>}]`; `structuredContent` is kept.
- Server hint: on a `can_use_tool` control request whose `tool_name` starts with `mcp__` and that has a string `mcp_server.name`, the core keeps `tool_use_id → mcp_server.name` in `mcp_server_hints: HashMap<String, String>` (`#[serde(skip)]`, at most 256 entries, the oldest goes first). The candidate of that tool use takes it as `server_hint` and the entry is removed. No `can_use_tool` (an allowed tool, bypass mode) → `server_hint: None`.
- App requests: control requests `mcp_status`, `mcp_read_resource {serverName, uri}`, `mcp_call {tool: claude_tool_name(server, tool), arguments, result_not_for_model: true}` with request ids `fd-app-<n>`; the `control_response` (`success` → `response`, `error` → `AppError::Answered(<error text>)`) fills `app_answers` (`#[serde(skip)]`). `mcp_call` has no timeout (§4.2); the end of the process ends every pending request with `AppError::Ended`.

### 4.3 Station capture (`crates/fleetdeck-station/src/apps/`)

1. **Intake of candidates.** Direct host: after a start or resume, the station takes `take_app_candidates()` and spawns one forwarder per session to the capture service. Shim host: `Steps` gets `apps: Vec<AppCandidate>`; `split()` of `shim_codex.rs` and `shim_claude.rs` fills it; after the output transaction commits, the durable driver hands them to its **app sink**. The sink is attached by the station (`DurableDriver::attach_app_sink(sink)`) when the session enters the `Live` map: in `insert_durable`, which serves both a new shim launch and `adopt` at the station start (fact 18). Until a sink is attached, the driver keeps the committed candidates in memory (`pending_apps`, at most 32; more → the newest is dropped with a warning). `attach_app_sink` first hands over the kept candidates in their order, then every later one. This covers the output that the restored driver replays before `adopt` completes. A restored session whose exit is already committed (the `Exited`/`Lost` branch of `adopt`) gets no sink and no bridge: its kept candidates are dropped with one warning (the resource cannot be read without the process).
2. **Bridge registry.** The station keeps `HashMap<SessionId, AppHost>` beside the `Live` map, with `AppHost { bridge: Arc<dyn AppBridge>, turns: Arc<SessionTurns>, process: ProcessIncarnation }`. An entry is added whenever a session enters the `Live` map with a process: after a direct start or resume (`app_bridge()`), after a shim launch, and in `adopt` for every restored session that is still running (the `Arc<DurableDriver<C>>`, which implements `AppBridge` with `internal`, `wait_until` and `with_core`; a wait ends with `AppError::Ended` when the driver commits the provider's exit). The entry is removed when that process ends, only if its `process` is still the entry's process (a newer process keeps its entry). A detach at the station shutdown removes the entry with the `Live` entry.
3. **Limits.** At most 4 captures run at a time on a station and 2 per session; more candidates wait in a queue of 32 per station; a full queue drops the newest candidate with a warning.
4. **Resolve.** Codex: `server` and `resource_uri` come with the candidate. Claude: `bridge.tools()` (cached per session for 30 s; a miss refreshes the cache once), then `ClaudeToolMap::build(tools).resolve(candidate.tool)`. Only `Unique { server, tool }` continues; `Ambiguous` writes one `warn` line `ambiguous_tool_name` with the session and the name and stops; `Unknown` stops. When `server_hint` is `Some(h)` and `h != server`, stop with one `warn` line `server_hint_mismatch`. The resolved tool must have `resource_uri` set, else stop. Every stop leaves a plain row. The stored `tool` is the raw tool name of the resolved pair.
5. **Read.** `bridge.read_resource(server, uri)` with `READ_TIMEOUT`. In `contents`, take the first item whose `uri` is absent or equal to the URI and whose `mimeType` passes `is_app_mime`. Check the size **before** decoding: `text` length at most 2 MiB; `blob` length at most ⌈2 MiB / 3⌉ × 4, then base64 decode. The bytes must be UTF-8 and not empty after trim.
6. **Sanitize.** `_meta.ui.csp.connectDomains` and `.resourceDomains`: keep a string only if it matches `^(https?|wss?)://(\*\.)?[a-z0-9-]+(\.[a-z0-9-]+)*(:[0-9]{1,5})?$` (case-insensitive) and is at most 256 characters; at most 32 per list; duplicates removed. `frameDomains`, `baseUriDomains` and `_meta.ui.permissions` are ignored (phase 1 grants none). `_meta.ui.prefersBorder` is kept when it is a boolean.
7. **Store and emit.** In one store transaction: insert the row of §4.4 and append the event `tool.app` of §4.5 for the session. Then apply the caps: more than 50 rows of the session or 500 rows of the station → delete the oldest rows (the events stay). An app whose row is deleted but whose `tool.app` event is in the session's log is **gone** (`AppLookup::Gone`, §4.4): the station answers `app_gone` and the deck 410. An app id without a row and without such an event is `unknown_app` (404).
8. **Failure.** Any failure (no bridge, timeout, provider error, ambiguous name, size, MIME, UTF-8) writes one `warn` log line with the session, server, tool and URI (never the document) and emits nothing: the row stays a plain tool row. A candidate is held only in memory: a station crash between the output commit and the capture loses it (plain row).
9. **No turn hold.** The turn ends when the provider ends it. `tool.app` can arrive after `turn.completed`; the board attaches it to its node.

### 4.4 Station store (migration V46, `crates/fleetdeck-station/src/store/schema_v46.rs`)

The migration number is the one that the architect gave this track (V46; V43 is taken by DL-402). Both tables are in this one migration.

```sql
CREATE TABLE mcp_apps (
  app_id        TEXT PRIMARY KEY,          -- ULID
  session_id    TEXT NOT NULL,
  node_id       TEXT NOT NULL,
  server        TEXT NOT NULL,             -- the raw server name
  tool          TEXT NOT NULL,             -- the raw tool name on the server (Claude: the resolved pair, never the full mcp__ name)
  resource_uri  TEXT NOT NULL,
  html          BLOB NOT NULL,             -- the document as read, at most 2 MiB
  doc_sha256    TEXT NOT NULL,             -- hex
  csp_json      TEXT NOT NULL,             -- AppCsp
  prefers_border INTEGER,                  -- NULL, 0, 1
  tool_input    TEXT NOT NULL,             -- JSON, at most 64 KiB; 'null' when it was over
  tool_result   TEXT,                      -- JSON CallToolResult, at most 512 KiB; NULL when over
  created_at    INTEGER NOT NULL
);
CREATE INDEX mcp_apps_session ON mcp_apps(session_id, created_at);

-- One row per app tools/call (§4.6.1). Never deleted by the app caps.
CREATE TABLE mcp_app_calls (
  call_id         TEXT PRIMARY KEY,        -- ULID
  session_id      TEXT NOT NULL,
  app_id          TEXT NOT NULL,
  worktree_root   TEXT NOT NULL,           -- the same text as shell_runs.worktree_root and revert rows
  server          TEXT NOT NULL,
  tool            TEXT NOT NULL,
  read_only       INTEGER NOT NULL,        -- 1: the tool is read-only; no generation advance
  process         TEXT NOT NULL,           -- the ProcessIncarnation text of the session's process
  incarnation     TEXT NOT NULL,           -- store.incarnation() of the station process that started it
  state           TEXT NOT NULL,           -- running | uncertain | done | failed | ended | released
  advance_pending INTEGER NOT NULL DEFAULT 0,
  generation      INTEGER,                 -- the folder generation after the start advance; NULL when read_only
  created_at      INTEGER NOT NULL,
  ended_at        INTEGER
);
CREATE INDEX mcp_app_calls_root ON mcp_app_calls(worktree_root, state);
CREATE INDEX mcp_app_calls_app ON mcp_app_calls(session_id, app_id, state);
```

Store API of the apps (`crates/fleetdeck-station/src/store/apps.rs`): `insert_app(NewApp, ToolAppEvent) -> Result<AppId>` (row and event in one transaction, then the caps), `app_lookup(session, app_id) -> Result<AppLookup>`, `app_chunk(session, app_id, offset, len) -> Result<Option<(Vec<u8>, u64)>>`. A query with a session id that is not the row's session finds no row.

```rust
pub enum AppLookup {
    Found(AppMeta),
    /// No row, and the session's log has a `tool.app` event with this app id
    /// (`SELECT 1 FROM events WHERE session_id = ?1 AND kind = 'tool.app'
    ///   AND json_extract(payload, '$.app_id') = ?2 LIMIT 1`, the query style of
    /// `store/shell_runs_ctx.rs:63-73`): the caps deleted it.
    Gone,
    Unknown,
}
```

`AppMeta.open_calls` is filled from `mcp_app_calls` (the rows of the app in `running` or `uncertain`, oldest first, at most 8).

Store API of the calls (`crates/fleetdeck-station/src/store/app_calls.rs`, new; it does not touch `store/shell_runs.rs`, which is at 784 lines). A row is **unsettled** while `state IN ('running', 'uncertain') OR advance_pending = 1`. The terminal states are `done`, `failed`, `ended`, `released`.

| Function | Does |
| --- | --- |
| `app_call_begin(NewAppCall) -> Result<Option<i64>>` | One transaction: insert the row in `running`. When `read_only = 0`, also advance the folder generation of the root with the SQL of `shell_runs::advance` (`INSERT INTO worktree_generations (worktree_root, generation) VALUES (?1, 1) ON CONFLICT (worktree_root) DO UPDATE SET generation = generation + 1`) and store the new value in `generation`. Returns the generation. |
| `app_call_settle(call_id, end: AppCallEnd) -> Result<bool>` | One guarded statement: only a row in `running` or `uncertain` moves to `end` (`done`, `failed`, `ended` or `released`), sets `ended_at` and `advance_pending = 1 - read_only`. `false` when the row was already terminal. |
| `app_call_advance(call_id, root) -> Result<i64>` | One transaction: when `advance_pending = 1`, advance the generation of `root` (the same SQL) and clear the marker. The caller holds the fence of `root`. |
| `app_call_pending_advances() -> Result<Vec<(String, String)>>` | `(call_id, worktree_root)` of the rows with `advance_pending = 1`. |
| `app_calls_of_older_incarnations() -> Result<Vec<AppCallRow>>` | The rows in `running` whose `incarnation` is not `store.incarnation()`. |
| `app_call_mark_uncertain(call_id) -> Result<bool>` | Guarded `running` → `uncertain`. |
| `app_calls_uncertain() -> Result<Vec<AppCallRow>>` | The rows in `uncertain`. |
| `app_call_barrier(root, app_turn) -> Result<Option<AppCallBarrier>>` | `Running` when an unsettled row of `root` exists; else `RanAfterTurn` when a row of `root` has `generation` above the `generation` of the turn row `app_turn` in `turn_checkpoints` (NULL counts as 0); else `None`. |
| `app_call_release(session, app_id, call_id) -> Result<Release>` | `Released` after a guarded `uncertain` → `released` (with `advance_pending = 1 - read_only`); `Running` when the row is `running`; `NotOpen` when it is terminal; `Unknown` when no row of that session and app has the id. |

### 4.5 Proto (`crates/fleetdeck-proto/src/apps.rs` and `event.rs`)

```rust
pub const MCP_APP_MIME: &str = "text/html;profile=mcp-app";
pub const MCP_APP_EXTENSION: &str = "io.modelcontextprotocol/ui";
pub const MAX_APP_DOC_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_APP_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_APP_RESULT_BYTES: usize = 512 * 1024;
pub const MAX_APP_ARGS_BYTES: usize = 64 * 1024;
pub const MAX_APP_REPLY_BYTES: usize = 768 * 1024;
pub const APP_DOC_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppCsp {
    #[serde(default)] pub connect_domains: Vec<String>,
    #[serde(default)] pub resource_domains: Vec<String>,
}

/// `session.app_meta` params and `session.app_document` params.
pub struct AppMetaQuery { pub session_id: SessionId, pub app_id: String }
pub struct AppDocumentQuery { pub session_id: SessionId, pub app_id: String, pub offset: u64 }

/// `session.app_meta` answer.
pub struct AppMeta {
    pub app_id: String, pub node_id: NodeId, pub server: String, pub tool: String,
    pub resource_uri: String, pub csp: AppCsp, pub prefers_border: Option<bool>,
    pub doc_bytes: u64, pub doc_sha256: String,
    pub tool_input: Value, pub tool_result: Option<Value>, #[ts(type = "number")] pub created_at: i64,
    /// The calls of this app that are not settled (§4.6.1), oldest first, at most 8.
    #[serde(default)] pub open_calls: Vec<AppOpenCall>,
}
#[serde(rename_all = "snake_case")]
pub enum AppCallState { Running, Uncertain }
pub struct AppOpenCall {
    pub call_id: String, pub tool: String, pub state: AppCallState,
    #[ts(type = "number")] pub created_at: i64,
}
/// `session.app_release` params; the answer is `AppReleaseReply`.
pub struct AppReleaseParams { pub session_id: SessionId, pub app_id: String, pub call_id: String }
pub struct AppReleaseReply { pub released: bool }
/// `session.app_document` answer: at most APP_DOC_CHUNK_BYTES raw bytes.
pub struct AppDocumentChunk { pub data_b64: String, pub eof: bool, pub total: u64 }

#[serde(tag = "method")]
pub enum AppRequest {
    #[serde(rename = "tools/call")] ToolsCall { name: String, #[serde(default)] arguments: Value },
    #[serde(rename = "resources/read")] ResourcesRead { uri: String },
}
/// `session.app_call` params.
pub struct AppCallParams {
    pub session_id: SessionId, pub app_id: String, pub request: AppRequest,
    #[serde(default)] pub confirmed: bool,
}
/// `session.app_call` answer and the deck answer of the call route.
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AppCallReply {
    Done { result: Value },
    Confirm { tool: String, title: Option<String> },
}
/// Board → deck body of the call route.
pub struct AppCallBody { pub request: AppRequest, #[serde(default)] pub confirmed: bool }
/// Deck → board answer of the view route.
pub struct AppView {
    pub url: String, #[ts(type = "number")] pub expires_at: i64,
    pub server: String, pub tool: String, pub resource_uri: String,
    pub csp: AppCsp, pub prefers_border: Option<bool>,
    pub tool_input: Value, pub tool_result: Option<Value>,
    pub open_calls: Vec<AppOpenCall>,
}
```

(All with `Debug, Clone, PartialEq, Serialize, Deserialize, TS`, `#[ts(export_to = "apps/")]`; `Value` fields with `#[ts(type = "unknown")]`.)

New event kind (additive; an older deck passes it through):

```rust
/// A tool call of this node shipped an MCP App that the station stored (B15).
/// Older peers decode this kind as unknown.
#[serde(rename = "tool.app")]
ToolApp {
    node_id: NodeId,
    app_id: String,
    server: String,
    tool: String,
    resource_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    prefers_border: Option<bool>,
},
```

`Event::kind_name` answers `"tool.app"` for it and `Event::KINDS` gets `"tool.app"` (61 → 62 entries). `SessionSummary::fold` lists `Event::ToolApp { .. }` in its arm of the events that change no summary field, with the comment "The board reads the app from the event (B15)." (fact 19).

Link feature `mcp_apps.v1` (`feature::MCP_APPS`, in the list of features of `version.rs`).

### 4.6 Link methods (`crates/fleetdeck-link/src/methods_table_tail.rs`)

| Method | Callers | Scope | Feature | Mutating |
| --- | --- | --- | --- | --- |
| `session.app_meta` | DeckToStation | Read | `mcp_apps.v1` | false |
| `session.app_document` | DeckToStation | Read | `mcp_apps.v1` | false |
| `session.app_call` | DeckToStation | Drive | `mcp_apps.v1` | false |
| `session.app_release` | DeckToStation | Drive | `mcp_apps.v1` | false |

`session.app_call` and `session.app_release` are not durable `SessionCommand`s: a repeated `app_release` is a guarded update with the same result, and an app call is tracked by its own durable row (§4.6.1), not by a receipt.

Station handling (`crates/fleetdeck-station/src/link/intake_apps.rs`; one dispatch arm in `intake.rs`). A missing row answers `app_gone` ("This app is no longer stored.") for `AppLookup::Gone` and `unknown_app` for `AppLookup::Unknown`, in every method.

- `session.app_meta`: `AppLookup::Found(meta)` → `AppMeta` (with `open_calls`).
- `session.app_document`: `offset` ≤ `doc_bytes`, else `invalid_params`; at most `APP_DOC_CHUNK_BYTES` from `offset`.
- `session.app_call`, in this order:
  1. The row exists for the session (`Found`), else `app_gone` or `unknown_app`.
  2. The session has an `AppHost` in the registry, else `session_stopped` ("The session that made this app is not running. Resume it to use the app."). Never a start, resume or wake.
  3. Per session at most 2 **unsettled calls of this station process** (a permit is held by the call task until the call is settled, not until the reply, §4.6.1) and 30 calls per minute, else `busy` ("The app sends too many requests. Wait a moment.").
  4. `ResourcesRead { uri }`: `uri` starts with `ui://` and is at most 4096 characters, else `invalid_params`; `bridge.read_resource(row.server, uri)`. No call row (a read changes nothing).
  5. `ToolsCall { name, arguments }`: `arguments` is an object (or `null`, sent as `{}`), serialized at most `MAX_APP_ARGS_BYTES`, else `invalid_params`. `bridge.tools()` (no cache older than 10 s). The tool `(row.server, name)` must be in the list, and for Claude `ClaudeToolMap::build(tools).address(row.server, name)` must be `Some` (a unique, addressable pair, §4.2), and its `visibility` must be `None` or contain `"app"`; else `tool_not_callable` ("This app cannot call that tool."). If the tool is not `read_only` and `confirmed` is false: answer `Confirm { tool: name, title }` without a call and without a row. Else run the call with the workspace protection of §4.6.1.
  6. A provider result over `MAX_APP_REPLY_BYTES` serialized → `too_large` ("The answer is too large for the app."). No answer within `CALL_REPLY_WAIT` → `timeout` ("The tool did not answer in 60 s. It may still run. A revert of this folder waits until it ends."). `AppError::Answered` → `provider_error` with the provider text cut to 300 characters, control characters removed. `AppError::Ended` → `session_stopped`.
- `session.app_release`: the row of the app exists (else `app_gone`/`unknown_app`); `app_call_release(session, app_id, call_id)`: `Released` → `AppReleaseReply { released: true }` and a wake of the advance task; `NotOpen` → `{ released: false }`; `Running` → `call_running` ("This app action is still running. Stop the session to end it."); `Unknown` → `unknown_call`. Every release writes one `info` log line with the session, app, call, server and tool.

#### 4.6.1 Workspace protection of app calls

An app tool call can change the files of the session's folder at any time, outside a turn, like a `!` shell command (B8). It therefore uses the same two protections as a shell command (fact 16): the workspace fence and the durable folder barrier. The code is in `crates/fleetdeck-station/src/apps/calls.rs` (new).

**Start of a call** (step 5 of `session.app_call`, after the confirmation):

1. `root` = `host.turns.worktree_root()` (a new accessor of `SessionTurns`: the same text that turn rows store). A session without a Git worktree has no checkpoints and no revert: the call runs without a row and without the fence (the steps 2 to 4 below are skipped; the permit of item 3 of `session.app_call` still applies).
2. Take the fence: `tokio::time::timeout(FENCE_WAIT = 5 s, host.turns.fence())`. No fence in 5 s → `busy` ("The session folder is busy (a revert or a checkpoint runs). Try again in a moment."), no row, no call.
3. Under the fence: `app_call_begin(NewAppCall { call_id: new ULID, session_id, app_id, worktree_root: root, server, tool, read_only, process: host.process, incarnation: store.incarnation() })`. A non-read-only call advances the folder generation now, so no revert can cross it (the B8 rule "a revert never crosses a command that ran in the folder").
4. Release the fence. Spawn the **call task** with the permit; the request handler waits for the task's outcome on a oneshot for at most `CALL_REPLY_WAIT`.

**The call task** awaits `bridge.call_tool(server, tool, arguments)` (no timeout, §4.2) and sends the outcome to the handler (a dropped handler is fine). Then it settles the row: `Ok` → `done`; `AppError::Answered` → `failed`; `AppError::Ended` → `ended`. A non-read-only row then has `advance_pending = 1`; the task takes the fence of `root` (no timeout) and calls `app_call_advance`, then wakes the advance task. It releases the permit after the settlement. So a call that timed out for the board keeps its permit and keeps the barrier until the provider answers or the process ends.

**Revert** (`manager/revert_run.rs`):

- `shell_block` also checks `app_call_barrier(root, app_turn)` when the shell barrier is `None`: `Running` → reason `app_call_running`, `RanAfterTurn` → reason `app_call_ran_after_turn` (two new constants in `reason`). So every place that meets the shell barrier also meets the app barrier: the dispatch, every restore retry (`retry_restore`) and the start recovery of `rolled_back`/`restoring` rows.
- `run()` calls `barrier_at_dispatch` a **second time right after** `turns.fence().await`, before `execute`. An app call takes the fence only to insert its row, so without this second check a revert that passed the first check could wait for the fence while an app call starts, and then run. The second check also covers the same window for a Codex shell of another session in the root.

**Station start** (`manager/shell_advance.rs`; no line is added to `manager/mod.rs`, which is at 797 lines): `start_up` calls `crate::apps::calls::start_up(store)` before its `advance_pending`; `advance_pending` also advances every `app_call_pending_advances()` row under the fence of its root; `task` calls `crate::apps::calls::settle_proven(store)` at every loop, beside `settle_proven` of the shells.

- `calls::start_up`: for each row of `app_calls_of_older_incarnations()` (no task of this station process waits for it): when the session's newest shim row proves the end of its execution (`store::resumes::proves_end`, the same proof as `shell_advance::shim_proof`) → `app_call_settle(ended)`; else → `app_call_mark_uncertain`. A direct-host row has no shim row and always becomes `uncertain` (a station crash does not stop a direct host's provider, B8 PRE-PR r1).
- `calls::settle_proven`: each `uncertain` row whose session's newest shim row proves the end → `app_call_settle(ended)`.
- An `uncertain` row of an adopted shim session stays `uncertain` while that provider runs: the late answer cannot be matched after the restart (`app_answers` is not in the checkpoint). It ends when the provider ends (shim proof) or by the owner's release.

**Owner release.** The board shows the open calls of an app (`AppMeta.open_calls`) and lets the owner release an `uncertain` call (`session.app_release`, §4.8.3) when the owner knows that the tool stopped. A `running` call cannot be released: the station still waits for it; stopping the session ends it (`AppError::Ended`).

**Not covered** (residuals in §5): a tool that writes outside the session's worktree root; a remote (HTTP) MCP server that writes into the folder after the provider ended; a stdio MCP server that ignores the end of its provider; a read-only tool that writes (its row blocks a revert only while it runs, and it advances no generation, because a read-only call must not block every later revert).

### 4.7 Deck (`crates/fleetdeck-deck/src/http/app_routes.rs`, `app_public.rs`, `app_csp.rs`, `methods_table_apps.rs`)

Routes (added in `http/mod.rs` beside the subagent route and the `/a/{token}` routes):

| Route | Who | Does |
| --- | --- | --- |
| `GET /api/sessions/{station}/{session}/apps/{app_id}` | owner login only (`owner_only`) | Feature check (`Without` → 409 `apps_unsupported`, `Down` → 409 `station_unreachable`); `session.app_meta` (wait 20 s); mints a view token; answers `AppView` with `url: "/app/{token}"` and the `open_calls`. |
| `POST /api/sessions/{station}/{session}/apps/{app_id}/call` | owner login only | Body `AppCallBody` at most 128 KiB; at most 60 calls per minute per login; `session.app_call` (wait 70 s); answers `AppCallReply`; writes an audit row for every `tools/call` (action `session.app_call`, detail `server/tool`, the result `ok`, `confirm`, or the error code; never the arguments). |
| `POST /api/sessions/{station}/{session}/apps/{app_id}/calls/{call_id}/release` | owner login only | No body; `session.app_release` (wait 20 s); answers `AppReleaseReply`; writes an audit row (action `session.app_release`, detail `call_id`, the result `ok`, `not_open` or the error code). |
| `GET /app/{token}` (also `HEAD`) | anyone with the token | 404 for an unknown or expired token. Fetches the document with `session.app_document` chunks (total wait 30 s), checks `doc_sha256`, prefixes the bootstrap of §4.8, and answers with the headers below. Outside the guard, like `/a/{token}`. |

- **Method table (fact 17).** A new file `crates/fleetdeck-deck/src/http/methods_table_apps.rs` holds `pub(super) const API_APPS: &[ApiMethod]`, and `methods::api()` chains it after `table_tools::API_TOOLS` (one `#[path]` module and one `.chain` line in `methods.rs`; `methods_table_rest.rs` is at 792 lines and is not changed). The rows use the `m(...)` constructor, so `member` and `display` are `false` (the guard refuses a member and a display with 403 before the handler; the handler still calls `owner_only` for a token principal):

  | Name | Method | Path | Scope | Mutating |
  | --- | --- | --- | --- | --- |
  | `sessions.app_view` | GET | `/api/sessions/{station}/{session}/apps/{app_id}` | Read | false |
  | `sessions.app_call` | POST | `/api/sessions/{station}/{session}/apps/{app_id}/call` | Drive | **true** |
  | `sessions.app_release` | POST | `/api/sessions/{station}/{session}/apps/{app_id}/calls/{call_id}/release` | Drive | **true** |

  The two POST rows are mutating on the board API although their link methods are not: with a cookie login the guard requires `X-CSRF-Token`, and the guard audits a refused request. The board sends both through its deck HTTP helper, which adds the CSRF header (`web/src/settings/http.ts:54`, `deckHttp`).
- **View tokens.** 32 random bytes (base64url), kept in deck memory as `sha256(token) → {station_id, session_id, app_id, csp, login_session_hash, expires_at}`; life 10 minutes; at most 512 entries (the oldest goes first); a deck restart forgets them (the board asks again). A token can load its document more than once within its life (a reload of the frame).
- **Answer headers of `/app/{token}`:** `Content-Type: text/html; charset=utf-8`, `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and `Content-Security-Policy` =

  ```
  sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' R; style-src 'unsafe-inline' R;
  img-src data: blob: R; font-src data: R; media-src data: blob: R; connect-src C;
  frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'
  ```

  where `R` is the space-joined `resource_domains` (empty → nothing) and `C` is the space-joined `connect_domains`, or `'none'` when empty. There is no `'self'` in any fetch directive: the app must not reach the deck API. The domains are sanitized again on the deck with the pattern of §4.3 item 6 (defense in depth).
- **HTTP errors of the API routes:** 403 `owner_only`; 404 `unknown_station`, `unknown_session`, `unknown_app`, `unknown_call`; 409 `apps_unsupported`, `station_unreachable`, `session_stopped`, `call_running`; 410 `app_gone` (the caps deleted the row; the station knows it from the `tool.app` event, §4.4); 400 `invalid_params`; 403 `tool_not_callable`; 413 `too_large`; 429 `busy`; 502 `provider_error`, `station_error`; 504 `timeout`.

### 4.8 Board (`web/src/apps/`)

**Bootstrap** (one inline `<script>`, prefixed by the deck after a leading `<!doctype …>` if there is one, so it is the first script of the document and runs before any script of the app). The source is a constant of `crates/fleetdeck-deck/src/http/app_public.rs` (`APP_BOOTSTRAP`), exactly:

```html
<script>(()=>{"use strict";
try{void document.cookie}catch(e){if(e&&e.name==="SecurityError"){Object.defineProperty(document,"cookie",{configurable:true,get:()=>"",set:()=>{}})}}
const host=window.parent,ch=new MessageChannel(),port=ch.port1,send=MessagePort.prototype.postMessage;
port.postMessage=function(m,_o,t){send.call(port,m,Array.isArray(t)?t:[])};
Object.defineProperty(window,"parent",{configurable:true,enumerable:true,get:()=>port});
port.onmessage=(ev)=>{const d=ev.data;
const c=d&&((d.result&&d.result.hostContext)||(d.method==="ui/notifications/host-context-changed"&&d.params));
if(c&&(c.theme==="light"||c.theme==="dark"))document.documentElement.style.colorScheme=c.theme;
window.dispatchEvent(new MessageEvent("message",{data:d,source:port}))};
port.start();
host.postMessage({"fleet-deck-app-hello":1},"*",[ch.port2]);
})();</script>
```

What it does:

1. The `document.cookie` facade (t3code #17027): an empty, non-persistent value, installed only when reading `document.cookie` throws `SecurityError`.
2. The **bridge channel**: a `MessageChannel` whose `port2` goes to the board in one hello message. The document keeps `port1`. `window.parent` is `[Replaceable]` in the HTML standard, so the bootstrap redefines it as `port1`, with a `postMessage(message, targetOrigin, transfer)` that sends on the port. The board's messages arrive on the port and are dispatched again as `message` events on `window` with `source: port1`, so app code that sends with `window.parent.postMessage` and checks `event.source === window.parent` (the ext-apps SDK transport) works without change.
3. The color scheme (t3code #17026): from `hostContext.theme` of the `ui/initialize` result and of `ui/notifications/host-context-changed`, read only from the port.

The bridge authority is **the port, not the window**. A document that replaces the app in the same frame (a navigation) is a new realm: it never holds `port1`, and it cannot get it, because the app has no frames, no popups and no shared worker (opaque origin). App code that posts to `window.top` or to the real parent (for example through the `Window.prototype` getter) gets no answer.

**Frame** (`AppFrame.tsx`): `<iframe sandbox="allow-scripts" referrerPolicy="no-referrer" title="App from {server}" src={view.url}>`, no `allow` attribute, width 100% of the chat column, height from `size-changed` clamped to 80–800 px (start 160 px). `prefersBorder` true → the frame sits on `--raised` with `--r-2` corners; else on `--bg` with no edge.

**Bridge** (`bridge.ts`, pure TypeScript, no React): one instance per mounted frame (a "Reload app" or "Show app" is a new mount with a new view token and a new bridge).
- **Handshake.** The bridge listens to `window` `message` events only for the hello. It accepts a hello only when `event.source === frame.contentWindow`, `event.origin === 'null'`, `event.data` is exactly `{"fleet-deck-app-hello": 1}` (one key), `event.ports.length === 1`, and no hello was accepted before in this mount. Then it keeps `event.ports[0]` as **the** port, calls `port.start()`, and from now on reads requests only from `port.onmessage` and sends every answer and notification only with `port.postMessage`. Every other `window` message from the frame window is dropped, before and after the hello (no answer, no error).
- **Closing.** These close the bridge for good: a second hello from the frame window (a replacement document, or a reload inside the frame); the frame's second `load` event; no accepted hello 1 s after the frame's first `load` event ("The app could not start."; the grace second covers a browser that runs the `load` task before the message task). Closing drops every pending request without an answer, calls `port.close()` and ignores every later message. After a second hello or a second `load`, the row shows "The app left its page and was stopped."
- On the port, a message counts only when the bridge is open, its serialized size (`JSON.stringify`) is at most 256 KiB, and it is a JSON-RPC 2.0 object. At most 16 requests of the app wait at a time (more → error `-32000` "Too many requests").
- Before `ui/initialize`, every request except `ping` gets `-32002` "Not initialized".
- `ui/initialize` → result `{ protocolVersion: "2026-01-26", hostInfo: { name: "fleet-deck", version }, hostCapabilities: { openLinks: {}, serverTools: {}, serverResources: {}, logging: {}, sandbox: { permissions: {}, csp: { connectDomains, resourceDomains } } }, hostContext }`.
- `hostContext`: `theme` (`light` | `dark` of the board), `styles.variables` (§4.8.1), `displayMode: "inline"`, `availableDisplayModes: ["inline"]`, `containerDimensions: { width: <frame width>, maxHeight: 800 }`, `locale`, `timeZone`, `platform: "web"`, `userAgent: "fleet-deck/<version>"`, `deviceCapabilities: { touch, hover }` (from `matchMedia`), `toolInfo: { tool: { name: <tool> } }`.
- After `ui/notifications/initialized`: `ui/notifications/tool-input { arguments }` (the stored input; `null` → `{}`), then `ui/notifications/tool-result` with the stored result (a `_meta` that is not an object is removed; a missing result → `{ content: [{ type: "text", text: "The result is too large to show in the app." }] }`).
- Theme or width change → `ui/notifications/host-context-changed` with the changed fields.
- `tools/call`, `resources/read` → the call route; `Confirm` → the consent dialog, then the call again with `confirmed: true`; Cancel → error `-32000` "Declined by the user". A station error code becomes a JSON-RPC error `-32000` with the station's message text. After a `timeout` answer, and every 30 s while the last `open_calls` is not empty, the row asks the view route again only to update `open_calls` (the frame keeps its document; the new token is not used).
- `ui/message` → the message dialog; Send → the composer's send path with the text; result `{}`; Cancel → `-32000` "Declined by the user". `params.content` may be one block or an array of blocks; only `text` blocks count (joined with a blank line); no text → `-32602` "Only text messages are supported."
- `ui/open-link` → only `http:`/`https:` (else `-32602` "Only web links can be opened."); the link dialog; Open → `window.open(url, '_blank', 'noopener,noreferrer')`; result `{}`.
- `ui/request-display-mode` → result `{ mode: "inline" }`.
- `ui/notifications/size-changed` → the frame height.
- `ui/notifications/request-teardown` → the bridge sends `ui/resource-teardown`, waits at most 2 s, unmounts the frame; the row shows "Show app".
- `notifications/message` → dropped. `ping` → `{}`.
- `ui/update-model-context`, `ui/download-file`, any other method → `-32601` "Not supported by Fleet Deck."
- Before an unmount that the owner caused (Hide app, leaving the session view): `ui/resource-teardown { reason }`, wait at most 2 s, then remove.

#### 4.8.1 Theme variables

The bridge reads the computed values of the board tokens (`getComputedStyle(document.documentElement)`) and sends literal colors (the app cannot read the board's variables):

| Spec variable | Board token |
| --- | --- |
| `--color-background-primary` / `-secondary` / `-tertiary` | `--bg` / `--surface` / `--raised` |
| `--color-text-primary` / `-secondary` / `-tertiary` | `--text` / `--dim` / `--faint` |
| `--color-border-primary` / `-secondary` | `--border` / `--border-soft` |
| `--color-text-danger`, `--color-background-danger` | `--hazard`, `--hazard-bg` |
| `--color-text-success`, `--color-background-success` | `--ok`, `--ok-bg` |
| `--color-text-info`, `--color-background-info`, `--color-ring-primary` | `--focus`, `--focus-bg`, `--focus` |
| `--color-text-warning` | `--act` |
| `--font-sans`, `--font-mono` | `--font-ui`, `--font-data` (the names only: the fonts are not served to the app) |

#### 4.8.2 Timeline and chat row

- `web/src/session/timeline.ts`: case `tool.app` sets `app = { app_id, server, tool, prefers_border }` on the tool item of `node_id` (a new optional field of the `tool` item). An event for an unknown node is ignored.
- `ChatView` renders `ToolView` as today and, when `tool.app` is set, an `AppRow` under it. `ChatView` gets an optional prop `apps?: AppsEnv` (`{ station, session, owner: boolean, actions: AppActions, sendMessage(text): Promise<void> }`), built by `SessionView`. Without `apps` (an old caller, a gallery fixture), no `AppRow`.
- Mounting: an `AppRow` mounts its frame when it enters the viewport (IntersectionObserver). At most 3 frames are mounted in one chat; when a fourth mounts, the frame that was mounted first is torn down and shows "Show app".
- A member (not the owner) sees the caption and the note "Only the owner can open this app." and no frame.

#### 4.8.3 Exact UI text

| Place | Text |
| --- | --- |
| Caption above the frame | `App from {server}` (`.fd-label`), then the tool name in mono, then the menu `⋯` with "Reload app" and "Hide app" |
| Collapsed row | button "Show app" (plain) |
| Loading | `Loading` with "Loading the app…" |
| Load error | `ErrorNotice` "The app could not load." with Try again |
| Station offline | `ErrorNotice` "The station is offline. The app cannot load." |
| App gone | "This app is no longer stored." (muted line) |
| Station without the feature | no caption, plain tool row |
| Navigated away (second hello or second `load`) | "The app left its page and was stopped." and "Reload app" |
| No hello before the first `load` | `ErrorNotice` "The app could not start." with Try again |
| Open call, state `running` (one line per call under the frame, `--faint`) | `An app action is running: {tool}. A revert of this folder waits until it ends.` |
| Open call, state `uncertain` (one line per call, `ErrorNotice` tone `--act`) | `An app action may still be running after a station restart: {tool}. A revert of this folder waits until it ends.` and the button "It has stopped" (plain) |
| Release dialog | title "Mark this app action as stopped?"; description "Only do this when you know that {tool} is not running. A revert can then change the files that it writes."; buttons "Cancel" (plain) and "Mark as stopped" (`.fd-decide`); on `released: false` the line goes away without a message; on `call_running` the notice "This app action is still running. Stop the session to end it." |
| Session stopped (after a refused call) | notice under the frame: "The session is not running. Resume it to use this app." |
| Network note (when `connect_domains` is not empty) | under the frame, `--faint`: `This app can connect to: {domains joined with ", "}` |
| Tool consent dialog | title "Allow this app action?"; description "The app from {server} wants to run {title or tool}. It runs with the session's access to {server}."; body: the arguments as formatted JSON in a code block (max 20 lines, scroll); buttons "Cancel" (plain) and "Allow" (`.fd-decide`) |
| Message dialog | title "Send this message to the session?"; body: the text as a quote; buttons "Cancel" and "Send" (`.fd-decide`) |
| Link dialog | title "Open this link?"; body: the URL in mono; buttons "Cancel" and "Open link" (`.fd-decide`) |

Dialogs use `.fd-dialog-head` and become bottom sheets on a phone (DESIGN-SYSTEM §4.8). While a dialog of an app is open, a second request of the same app that needs a dialog waits in a queue (at most 4; more → `-32000` "Too many requests").

### 4.9 Out of scope (phase 1)

- Full screen and picture-in-picture (t3code needed 7 review rounds for full screen alone: covering approvals, traps, activation).
- `ui/update-model-context` (needs per-provider untrusted context injection; Claude has no recorded path), `ui/download-file`, `sampling/createMessage`, `tool-input-partial`, `tool-cancelled`.
- Apps of Codex subagent threads and of Claude subagents (parent node set).
- `frameDomains`, `baseUriDomains`, `_meta.ui.permissions` (camera, microphone, geolocation, clipboard).
- A deck copy of app documents; apps in shared links; apps on the wall; apps of the Fleet Deck MCP server; coordinator tools that drive apps.
- Codex connector tool-name aliases (t3code #22535).
- App code that talks to the host through `window.top` or the real parent window instead of `window.parent` (§4.8): it gets no answer.
- A Claude app whose server and tool names are not addressable (§4.2): it stays a plain row.
- A station setting to turn the feature off (owner question 2).

### 4.10 Compatibility

| Peers | Behavior |
| --- | --- |
| New station, old deck | `tool.app` passes through as an unknown event; the old board ignores it; no route exists. Plain rows. |
| Old station, new deck | No `mcp_apps.v1`: no `tool.app` events; the routes answer 409 `apps_unsupported` if called. |
| New deck, old board | The board ignores `tool.app` (unknown kind in its fold). |
| Codex < 0.163.0 | No extension; no `ui://` URI on items; plain rows. |
| Claude < 2.1.308 | The variable is ignored; `system/init` lacks the capabilities; no candidates; plain rows. |
| Station restart | Stored apps stay; a view of a session whose process ended answers `session_stopped` for calls but still renders the stored document with its stored input and result. A shim session whose provider survived the restart gets its bridge back in `adopt`: its apps keep working, and the candidates of the down time are captured after `adopt` (§4.3 items 1 and 2). An app call that was running when the station stopped becomes `uncertain` unless the shim proves the end of its provider (§4.6.1). |

## 5. Risk class and threat notes

**High:** a new command path (board → live provider session), untrusted active content in the owner's browser, a station migration.

| Threat | Mitigation |
| --- | --- |
| The app reaches the deck API or the owner's cookies | Opaque origin twice (CSP `sandbox allow-scripts` header and the frame `sandbox="allow-scripts"`, never `allow-same-origin`); no `'self'` in the app CSP; the API needs the board cookie, which an opaque origin never sends; `frame-ancestors 'self'`. |
| The app navigates its frame to a page that poses as the app, or to a script-capable `/a/{token}/` artifact with an open CSP, to use the bridge from there | The bridge talks only on the `MessagePort` that the original document's bootstrap made before any app script ran (§4.8); the replacement document never holds that port, so it gets no answer, also before its `load` event, and pending answers go only to the port. A second hello or a second `load` closes the bridge. The board CSP `frame-src 'self'` keeps the frame on the deck origin; deck pages refuse framing (`frame-ancestors 'none'`); tokens are unguessable and per app. Residual: the app can navigate to an artifact view URL that it already knows and carry its own data in the URL; it has no artifact token unless its data contains one. |
| A forged app reference (impersonation) | The board never names a server or a URI: it names an `app_id`; the station takes the server from its own row, captured from the provider's own item (Codex) or its own `mcp_status` (Claude). A tool result that looks like an app reference has no effect. |
| A Claude tool name that fits two servers (`(a, b__c)` and `(a__b, c)`, or two names that normalize the same) sends the tool's input and result to the wrong server's HTML, or a call checked for one pair runs on another | `ClaudeToolMap` (§4.2): a capture and a call continue only for a unique, addressable pair; the station never takes the first match; a `can_use_tool` server hint must agree. Residual: a new MCP server that the provider connects between the check and the `mcp_call` (a window of one request). |
| The app calls a hidden or foreign tool | The station checks `(row.server, name)` against the live tool list, the unique Claude address and the `app` visibility (both providers skip this check, §2.4). |
| An app call changes the folder while a revert restores it, or after a timeout, or across a station restart | Every `tools/call` is a durable `mcp_app_calls` row inserted under the workspace fence; a revert meets the app barrier at the dispatch, again under the fence, at every restore retry and in its start recovery; a non-read-only call advances the folder generation at its start and its end, so no revert crosses it; a timed-out call keeps its row and its permit until the provider answers or the process ends; after a restart an unproven call is `uncertain` until the shim proves the end or the owner releases it (§4.6.1). Residuals: writes outside the worktree root; a remote MCP server; a stdio server that ignores the end of its provider; a read-only tool that writes; an owner who releases a call that still runs. |
| The app runs a mutating tool without the owner | Station answers `Confirm` for every tool that is not read-only; only a second request with `confirmed: true` runs it; only the owner login reaches the route. Residual: the tool's own `readOnlyHint` is the server's claim (the same as t3code). |
| A call bypasses the provider's permission mode | Accepted and stated: Codex `mcpServer/tool/call` and Claude `mcp_call` do not ask the provider's approval; the owner's confirmation replaces it. A bypass-mode session changes nothing here. |
| Data exfiltration from the app | `connect-src` only to declared domains, shown to the owner under the frame; no forms, no popups, no frames, no `base`. Residual: the app can show what it received (tool input and result) to its declared domains — that is its own server's data. |
| Key capture by a focused frame (t3code #15968 open decision) | Accepted residual, the same as HTML artifacts today: a frame that has focus receives keys. The board never moves focus into a frame by itself. |
| Oversized or hostile documents | 2 MiB cap checked before decoding; UTF-8 check; chunked relay; 256 KiB message cap; 16 pending requests; rate limits on the station (30/min/session, 2 unsettled calls) and the deck (60/min/login). |
| Resource exhaustion by a malicious app (CPU) | At most 3 mounted frames per chat; frames mount only in view. Residual: a frame can use CPU while mounted. |
| A stopped session is woken by an app | `session_stopped`; no start, resume or wake on this path. |
| Store growth | 50 app rows per session, 500 per station, oldest out. Call rows are small and stay (they are the revert evidence, like `shell_runs`). |
| Capture blocks a turn or the effect worker | Captures run in their own tasks with their own limits; the bridge never takes the `Live` lock; the turn is not held. |

## 6. Build plan

Branch `mcp-apps` from `v2`; the branch merges into `v2` only after all tasks, the PRE-PR REVIEW and the live smoke (T1 adds a feature that the station advertises; the methods arrive in T5b). Every task resets its worktree to the branch head first. Heavy commands only through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <cmd>`.

Order: **T1** → (**T2** ∥ **T4** ∥ **T6** ∥ **T7**) → **T3** (after T2) → **T5a** (after T2, T3, T4) → **T5b** (after T5a) → **T8** (after T7, needs the T6 routes in the fake deck only) → **T9** (after all).

Every task runs the guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`. UI tasks also run `sh tools/design-check.sh`.

### T1 — Proto, event and link contracts (backend, about 340 lines)

- **Owned files:** `crates/fleetdeck-proto/src/apps.rs` (new), `crates/fleetdeck-proto/src/apps_tests.rs` (new), `crates/fleetdeck-proto/src/event.rs`, `crates/fleetdeck-proto/src/event_kinds.rs`, `crates/fleetdeck-proto/src/event_tests.rs`, `crates/fleetdeck-proto/src/fold.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/ts.rs`, `crates/fleetdeck-link/src/methods_table_tail.rs`, `crates/fleetdeck-link/src/methods_tests.rs`, `web/src/gen/**` (generated only).
- **Changes:** §4.5 types and constants (with `AppOpenCall`, `AppCallState`, `AppReleaseParams`, `AppReleaseReply`, `AppMeta.open_calls`, `AppView.open_calls`); the `tool.app` variant; `kind_name` and `KINDS` (62 entries); `Event::ToolApp { .. }` in the no-change arm of `SessionSummary::fold` with its comment; `feature::MCP_APPS = "mcp_apps.v1"` in the feature list; the four method rows of §4.6; TS exports; regenerate `web/src/gen`.
- **Tests:** `apps_tests::app_request_wire_names` (`tools/call` and `resources/read` tags round-trip); `apps_tests::app_call_reply_outcomes` (`done` and `confirm` tags); `apps_tests::app_meta_without_open_calls_decodes` (`open_calls` defaults to empty); `event_tests::tool_app_round_trips_and_omits_border` (no `prefers_border` key when `None`); `event_tests::old_peer_keeps_tool_app_as_unknown` (an `EventBody` of a build without the variant keeps it as `Unknown`, as the B6 test for `task.updated` does); `event_tests::tool_app_kind_name_is_listed` (`kind_name` is `tool.app` and `KINDS` contains it); `fold` test `tool_app_changes_no_summary_field`; `methods_tests::app_methods_scopes` (Read, Read, Drive, Drive; all non-mutating; feature `mcp_apps.v1`).
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(apps) | test(tool_app)'`; `cargo nextest run -p fleetdeck-link --no-fail-fast -E 'test(app_methods)'`; the guards.

### T2 — Provider shared layer and Codex (backend, about 650 lines)

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-providers/src/apps.rs` (new), `crates/fleetdeck-providers/src/apps_names.rs` (new: `claude_part`, `claude_tool_name`, `claude_parse`, `ClaudeToolMap`), `crates/fleetdeck-providers/src/apps_tests.rs` (new), `crates/fleetdeck-providers/src/lib.rs`, `crates/fleetdeck-providers/src/adapter.rs`, `crates/fleetdeck-providers/src/codex/core.rs`, `crates/fleetdeck-providers/src/codex/map.rs`, `crates/fleetdeck-providers/src/codex/apps.rs` (new), `crates/fleetdeck-providers/src/codex/core_apps.rs` (new), `crates/fleetdeck-providers/src/codex/driver.rs`, `crates/fleetdeck-providers/src/codex/driver_apps.rs` (new), `crates/fleetdeck-providers/src/codex/driver_probe.rs`, `crates/fleetdeck-providers/src/codex/tests_apps.rs` (new), `spikes/s2-codex/fixtures/mcp_app.jsonl` (new, from `rec-codex-ext1-turn.jsonl` in the fixture format of the other files).
- **Changes:** §4.2 shared module (`AppError`, `AppBridge` with the signatures of §4.2, `CALL_REPLY_WAIT`, the name functions and `ClaudeToolMap` with the three addressability rules) and the `LiveSession`/`ProviderAdapter` defaults; Codex: `set_offer_mcp_apps`, the `initialize` extension, `Output::AppCandidate` (`server_hint: None`), candidate building, app requests and answers, the direct-driver `AppBridge` (`call_tool` without a timeout; every pending app request ends with `AppError::Ended` at the process exit), `offers_mcp_apps` with a cached `--version` probe. `codex/core.rs` and `codex/map.rs` get only the hook lines (they are at 782 and 768 lines).
- **Tests:** `initialize_offers_the_ui_extension_only_when_set`; `completed_mcp_call_with_ui_uri_gives_one_candidate_after_tool_completed` (fixture `mcp_app.jsonl`: `server demo`, `tool show_counter`, URI, arguments `{}`, result with `_meta` removed because it is null); `mcp_call_without_uri_or_failed_gives_no_candidate`; `child_thread_mcp_call_gives_no_candidate`; `oversized_result_gives_candidate_without_result`; `app_requests_write_the_three_methods` (exact params of §4.2); `app_answer_is_taken_once_and_late_answers_are_dropped`; `codex_call_tool_waits_past_the_reply_wait_and_ends_on_exit` (paused clock: no answer after 120 s is still pending; the process exit gives `AppError::Ended`); `apps_verified_gate` (0.162.1 false, 0.163.0 true, Claude 2.1.307 false, 2.1.308 true); `claude_tool_name_normalizes` (`my.server`, `a b` → `mcp__my_server__a_b`); `claude_parse_matches_the_cli` (`mcp__a__b__c` → `("a", Some("b__c"))`, `mcp__a` → `("a", None)`, `x__a__b` → `None`); `claude_map_rejects_delimiter_collisions` (servers `a` with tool `b__c` and `a__b` with tool `c`: `resolve("mcp__a__b__c")` is `Ambiguous`, `address` is `None` for both pairs); `claude_map_rejects_normalization_collisions` (servers `my.server` and `my_server` with tool `t`: both `Ambiguous`; one server with tools `a.b` and `a_b`: both `Ambiguous`); `claude_map_rejects_unaddressable_servers` (server `a_` and server `x__y` alone: `address` is `None`); `claude_map_resolves_a_unique_pair` (server `demo`, tools `show_counter`, `bump`: `Unique` and `address` round-trips).
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(apps) | test(mcp_app) | test(ui_extension) | test(candidate) | test(claude_map) | test(claude_parse) | test(claude_tool_name) | test(call_tool)'`; the existing Codex replay: `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'binary(codex_core_replay) | binary(codex_driver_replay)'`; the guards.

### T3 — Claude provider (backend, about 480 lines)

- **Depends on:** T2.
- **Owned files:** `crates/fleetdeck-providers/src/claude/core.rs`, `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/claude/frames.rs`, `crates/fleetdeck-providers/src/claude/apps.rs` (new), `crates/fleetdeck-providers/src/claude/driver.rs`, `crates/fleetdeck-providers/src/claude/driver_apps.rs` (new), `crates/fleetdeck-providers/src/claude/durable_launch.rs`, `crates/fleetdeck-providers/src/claude/tests_apps.rs` (new), `spikes/s1-claude/fixtures/rust/mcp_app.jsonl` (new, from `rec-claude-turn.jsonl`).
- **Changes:** §4.2 Claude part: the environment variable on both launch paths; `mcp_apps_ready` from `system/init.capabilities`; the `mcp_server_hints` map from `can_use_tool`; `Output::AppCandidate` on main-thread `mcp__` results with the normalized `tool_use_result` and the `server_hint`; `mcp_status`, `mcp_read_resource`, `mcp_call` requests and answers (`AppError::Answered` for an error answer); the direct-driver `AppBridge` (`call_tool` without a timeout, `AppError::Ended` at the exit) and candidate receiver.
- **Tests:** `launch_env_sets_mcp_apps_host` (both launch paths); `init_capabilities_enable_apps_only_with_all_three`; `mcp_tool_result_gives_candidate_with_call_tool_result` (fixture: tool `mcp__demo__show_counter`, result `{content:[{type:text,text:"{\"count\":2}"}], structuredContent:{count:2}}`, `server_hint: Some("demo")` from frame 31); `candidate_without_can_use_tool_has_no_server_hint`; `subagent_mcp_result_gives_no_candidate`; `denied_or_error_result_gives_no_candidate`; `app_requests_write_control_requests` (exact subtypes and fields); `control_error_answer_is_an_error`; `claude_call_tool_ends_on_exit`; `tools_parse_read_only_and_visibility` (from the recorded `mcp_status` answer).
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(apps) | test(mcp_app) | test(mcp_apps_host) | test(candidate) | test(server_hint) | test(call_tool)'`; `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'binary(claude_core_replay) | binary(claude_driver_replay)'`; the guards.

### T4 — Station store and capture service (backend, about 700 lines)

- **Depends on:** T1 (types). Uses `AppBridge`, `AppError`, `AppCandidate` and `ClaudeToolMap` from T2: start T4 after T2 merges, or in parallel against the T2 signatures of §4.2 copied into the brief (no edits to provider files).
- **Owned files:** `crates/fleetdeck-station/src/store/schema_v46.rs` (new), `crates/fleetdeck-station/src/store/migrations.rs`, `crates/fleetdeck-station/src/store/mod.rs`, `crates/fleetdeck-station/src/store/apps.rs` (new), `crates/fleetdeck-station/src/store/apps_tests.rs` (new), `crates/fleetdeck-station/src/store/app_calls.rs` (new), `crates/fleetdeck-station/src/store/app_calls_tests.rs` (new), `crates/fleetdeck-station/src/apps/mod.rs` (new), `crates/fleetdeck-station/src/apps/capture.rs` (new), `crates/fleetdeck-station/src/apps/sanitize.rs` (new), `crates/fleetdeck-station/src/apps/capture_tests.rs` (new), `crates/fleetdeck-station/src/lib.rs`.
- **Changes:** §4.4 both tables and both store APIs (`AppLookup` with the event query; the call rows with the shared `worktree_generations` advance); §4.3 items 3 to 9 as `apps::Capture` (`offer(session_id, candidate, bridge)`), with the Claude resolution through `ClaudeToolMap` and the hint check, and a fake `AppBridge` in the tests.
- **Tests:** `insert_app_writes_row_and_event_in_one_transaction`; `caps_delete_oldest_rows_per_session_and_station`; `lookup_after_caps_is_gone_and_never_stored_is_unknown`; `app_lookup_of_another_session_is_unknown`; `chunk_reads_bounded_slices`; `capture_codex_candidate_stores_and_emits_tool_app`; `capture_claude_candidate_resolves_a_unique_pair` (stores the raw tool name); `capture_claude_cross_server_collision_stores_nothing` (servers `a` + `b__c` and `a__b` + `c`, both with a `ui://` URI: no row, no event, no `read_resource` call); `capture_claude_hint_mismatch_stores_nothing`; `capture_refuses_wrong_mime_oversize_blob_before_decode_and_non_utf8`; `capture_timeout_emits_nothing`; `sanitize_keeps_only_scheme_origins` (drops `*`, `'self'`, paths, quotes, `javascript:`; keeps `https://*.example.com:8443`); `capture_limits_queue_and_concurrency`; `app_call_begin_advances_generation_only_when_not_read_only`; `app_call_settle_is_guarded_and_sets_advance_pending`; `app_call_barrier_running_then_ran_after_turn` (a running row → `Running`; after settle and advance, a turn row with a lower generation → `RanAfterTurn`; a read-only row after settle → `None`); `app_call_release_states` (`Released`, `Running`, `NotOpen`, `Unknown`); `open_calls_of_an_app`; `migration_v46_applies`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(app) | test(migration_v46)'`; the guards.

### T5a — Station wiring: candidates, bridge registry, restore (backend, about 450 lines)

- **Depends on:** T2, T3, T4.
- **Owned files:** `crates/fleetdeck-station/src/manager/durable.rs`, `crates/fleetdeck-station/src/manager/durable_apps.rs` (new), `crates/fleetdeck-station/src/manager/shim_codex.rs`, `crates/fleetdeck-station/src/manager/shim_claude.rs`, `crates/fleetdeck-station/src/manager/shim_target.rs`, `crates/fleetdeck-station/src/manager/shim_adopt.rs` (new), `crates/fleetdeck-station/src/manager/target.rs`, `crates/fleetdeck-station/src/manager/target_stop.rs` (new), `crates/fleetdeck-station/src/manager/target_apps.rs` (new), `crates/fleetdeck-station/src/manager/apps_wired_tests.rs` (new).
- **Changes:**
  1. Make room first, with no behavior change: move `SessionTarget::adopt` unchanged from `shim_target.rs` (791 lines) into `shim_adopt.rs`, declared from `shim_target.rs` as `#[path = "shim_adopt.rs"] mod adopt;`; move `SessionTarget::stop_all` unchanged from `target.rs` (792 lines) into `target_stop.rs`, declared from `target.rs` as `#[path = "target_stop.rs"] mod stop;`. `target_apps.rs` is declared from `target.rs` as `#[path = "target_apps.rs"] mod apps;` and the tests from `target_apps.rs`. **No line is added to `manager/mod.rs`** (797 lines).
  2. §4.3 item 1: direct candidate forwarders; `Steps.apps`; `split()` fills it; the durable driver's `pending_apps` buffer and `attach_app_sink` (in `durable_apps.rs`, with only hook lines in `durable.rs`); `AppBridge` for `DurableDriver<CodexCore>` and `DurableDriver<ClaudeCore>` (waits end with `AppError::Ended` when the driver commits the exit); `offers_mcp_apps` passed to `DurableCodex::open` (`shim_target.rs:455` before the move of step 1).
  3. §4.3 item 2: the registry of `AppHost` in `target_apps.rs`, with `register_app_host(session, host)` called after a direct start or resume, in `insert_durable` (new launch and `adopt`), and `drop_app_host(session, process)` called where the process ends and in the shutdown detach. `insert_durable` also attaches the app sink. The `Exited`/`Lost` branch of `adopt` registers nothing and drops the kept candidates with one warning. `pub fn app_host(&self, session: &SessionId) -> Option<AppHost>` (a clone; `AppHost` derives `Clone`) is the only accessor that T5b's intake uses, through the manager's existing `target` field.
- **Tests:** `direct_and_shim_candidates_reach_capture` (wired test with the fake provider of `fleetdeck-testkit`, both hosts); `durable_driver_keeps_candidates_until_a_sink_attaches` (two candidates committed before `attach_app_sink` arrive in order, then a third); `adopt_registers_the_bridge_and_drains_replayed_candidates` (a `Restored` session that is still running: after `adopt`, the registry has its `AppHost` and the candidate committed during the restore reaches the capture service); `adopt_of_an_exited_session_registers_no_bridge`; `bridge_registry_drops_on_process_end` (and a newer process keeps its entry); `shutdown_detach_drops_the_app_host`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(candidates_reach_capture) | test(sink_attaches) | test(adopt_) | test(bridge_registry) | test(app_host)'`; the existing shim and revert tests stay green: `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(shim) | test(adopt) | test(stop_all)'`; the guards.

### T5b — Link intake and the workspace protection of app calls (backend, about 600 lines)

- **Depends on:** T5a.
- **Owned files:** `crates/fleetdeck-station/src/link/intake.rs`, `crates/fleetdeck-station/src/link/intake_apps.rs` (new), `crates/fleetdeck-station/src/link/intake_apps_tests.rs` (new), `crates/fleetdeck-station/src/apps/mod.rs`, `crates/fleetdeck-station/src/apps/calls.rs` (new), `crates/fleetdeck-station/src/apps/calls_tests.rs` (new), `crates/fleetdeck-station/src/manager/turns.rs`, `crates/fleetdeck-station/src/manager/revert_run.rs`, `crates/fleetdeck-station/src/manager/revert_run_apps_tests.rs` (new), `crates/fleetdeck-station/src/manager/shell_advance.rs`, `crates/fleetdeck-testkit/src/sim_codex.rs` (hook lines only, it is at 775 lines), `crates/fleetdeck-testkit/src/sim_codex_mcp.rs` (new), `crates/fleetdeck-cli/tests/mcp_apps_shim_restart.rs` (new).
- **Changes:**
  1. §4.6 station handling of the four methods; `intake.rs` gets one match arm (`"session.app_meta" | "session.app_document" | "session.app_call" | "session.app_release" => apps::call(...)`) and the module line `#[path = "intake_apps.rs"] mod apps;`.
  2. §4.6.1: `SessionTurns::worktree_root()`; the call start under the fence, the call task, the permits held to the settlement; `apps::calls::{start_up, settle_proven}`; the release.
  3. `revert_run.rs`: the app barrier inside `shell_block` with the reasons `app_call_running` and `app_call_ran_after_turn`, and the second `barrier_at_dispatch` right after `turns.fence().await` in `run()`.
  4. `shell_advance.rs`: `start_up` calls `apps::calls::start_up` before `advance_pending`; `advance_pending` also advances the app call rows; `task` calls `apps::calls::settle_proven`.
  5. Testkit: `sim_codex_mcp.rs` answers `mcpServerStatus/list` (one server `demo` with the tools of §2.4), `mcpServer/resource/read` (a small HTML document with the app MIME) and `mcpServer/tool/call` (`show_counter` answers at once; `bump` answers after the scenario's delay; the scenario `mcp_app` also completes one `mcpToolCall` item with `mcpAppResourceUri`). `sim_codex.rs` gets only the dispatch lines.
- **Tests:** `app_call_unknown_app`; `app_call_gone_after_caps_is_app_gone`; `app_call_session_stopped_never_starts`; `app_call_refuses_app_only_tool_of_other_server_and_hidden_tool`; `app_call_refuses_an_ambiguous_claude_pair` (the live list has `(a, b__c)` and `(a__b, c)`: a call of either is `tool_not_callable`, the fake bridge sees no `call_tool`); `app_call_non_read_only_needs_confirmation_then_runs`; `app_call_read_only_runs_at_once`; `app_call_resources_read_needs_ui_scheme`; `app_call_rate_and_concurrency_busy`; `app_call_too_large_and_timeout`; `app_call_timeout_keeps_row_and_permit_until_the_answer` (paused clock: the reply is `timeout` after 60 s, the row stays `running`, a third call is `busy`; the late answer settles the row `done`, advances the generation and frees the permit); `app_call_busy_when_the_fence_is_held` (a held fence for 5 s → `busy`, no row); `app_call_without_worktree_has_no_row`; `app_call_process_end_settles_ended`; `app_document_chunks_reassemble_to_sha256`; `app_release_states`; `revert_refused_while_an_app_call_runs` (`app_call_running`, nothing restored); `revert_refused_across_a_finished_non_read_only_call` (`app_call_ran_after_turn`); `revert_allowed_after_a_read_only_call_ended`; `revert_rechecks_the_barrier_under_the_fence` (a revert that passed the first check while an app call holds the fence and inserts its row ends `failed` with `app_call_running`); `retry_restore_meets_the_app_barrier`; `start_up_marks_unproven_calls_uncertain_and_settles_proven_ones` (a direct-host row → `uncertain`; a shim row with `proves_end` → `ended` and advanced); `settle_proven_ends_uncertain_calls_of_an_ended_shim`. The CLI test `mcp_apps_shim_restart.rs` (the shim harness of `crates/fleetdeck-cli/tests/shim_station.rs`, sim Codex scenario `mcp_app`): start a shim session, capture its app, start a `bump` call with `confirmed: true` whose answer is delayed, stop the station (the shim and the provider survive), start a new station: (a) the delayed call's row is `uncertain` and `app_call_barrier` of the folder is `Running` (the revert itself is covered by the station tests above); (b) `session.app_call` of `show_counter` answers `Done` (not `session_stopped`); (c) an `mcpToolCall` item that the sim completes while the station is down is captured after the restart (`tool.app` arrives); (d) after the provider ends, the `uncertain` row settles `ended`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(app_call) | test(app_document) | test(app_release) | test(revert_) | test(start_up_marks) | test(settle_proven_ends)'`; `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(revert) | test(shell)'` (the existing revert and shell tests stay green); `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(mcp_apps_shim_restart) | binary(shim_station)'`; the guards.

### T6 — Deck routes, method table and the app document route (backend, about 560 lines)

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-deck/src/http/app_routes.rs` (new), `crates/fleetdeck-deck/src/http/app_public.rs` (new), `crates/fleetdeck-deck/src/http/app_csp.rs` (new), `crates/fleetdeck-deck/src/http/app_views.rs` (new), `crates/fleetdeck-deck/src/http/app_routes_tests.rs` (new), `crates/fleetdeck-deck/src/http/methods_table_apps.rs` (new), `crates/fleetdeck-deck/src/http/methods.rs`, `crates/fleetdeck-deck/src/http/mod.rs`.
- **Changes:** §4.7 routes (with the release route), the three method-table rows and the `.chain`, token map, CSP builder, the bootstrap constant `APP_BOOTSTRAP` of §4.8 exactly as written, audit rows, the 410 mapping of `app_gone`.
- **Tests (all through `http::router()` with the guard, not by calling a handler):** `app_routes_are_in_the_method_table` (each of the three routes answers something other than 404 `not_found` for the owner); `app_call_and_release_need_csrf_with_a_cookie_login` (no `X-CSRF-Token` → 403 and a refused audit row; with the token → the handler runs); `view_is_owner_only` (member, token principal and display get 403); `view_without_feature_is_409`; `view_mints_token_and_document_route_serves_with_app_csp` (exact header string for empty and non-empty domains; no `'self'` in fetch directives); `document_route_unknown_or_expired_token_404`; `document_sha_mismatch_is_502`; `bootstrap_follows_doctype_and_is_first` (the bootstrap is the first `<script>` of the answer, after the doctype); `call_route_maps_station_errors_to_http` (each code of §4.7, with `app_gone` → 410 and `call_running` → 409); `call_route_writes_audit_for_tools_call_only`; `release_route_writes_audit`; `call_route_rate_limit_429`; `token_map_evicts_oldest_over_512`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(app_) | test(methods)'`; the guards.

### T7 — Board bridge library (ux, about 560 lines)

- **Depends on:** T1 (generated types).
- **Owned files:** `web/src/apps/bridge.ts` (new), `web/src/apps/bridge.test.ts` (new), `web/src/apps/theme.ts` (new), `web/src/apps/theme.test.ts` (new), `web/src/apps/actions.ts` (new), `web/src/apps/actions.test.ts` (new).
- **Changes:** §4.8 bridge (the hello handshake, the port-only traffic, the closing rules, the policy object with the chat policy), the theme mapping of §4.8.1, the API calls (`view`, `call`, `release`) through `deckHttp` (`web/src/settings/http.ts`) so the POSTs carry `X-CSRF-Token`, with shape checks in the style of `web/src/settings/desktopTools.ts:planOf`.
- **Tests (bun):** "accepts one hello with one port from the frame window"; "drops window messages without the handshake, also JSON-RPC requests from the frame window"; "answers only on the port, never on the frame window"; "a second hello closes the bridge and drops pending answers"; "no hello 1 s after the first load closes the bridge"; "refuses requests before ui/initialize except ping"; "replays tool-input then tool-result after initialized and strips a null _meta"; "too many pending requests"; "oversized message is dropped"; "second load closes the bridge"; "confirm outcome asks, Cancel answers Declined by the user"; "ui/message joins text blocks and refuses non-text"; "open-link refuses non-web schemes"; "request-display-mode answers inline"; "unsupported methods answer -32601"; "theme variables are literal colors of the tokens"; "call and release send the CSRF header".
- **Acceptance:** `cd web && bun run test src/apps` (through heavy.sh); `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; the guards.

### T8 — Board app row, dialogs and chat integration (ux, about 620 lines)

- **Depends on:** T7 (and T6 for the route contract; the tests use a fake).
- **Owned files:** `web/src/apps/AppFrame.tsx` (new), `web/src/apps/AppRow.tsx` (new), `web/src/apps/AppDialogs.tsx` (new), `web/src/apps/AppCalls.tsx` (new: the open-call lines and the release dialog), `web/src/apps/apps.test.tsx` (new), `web/src/styles/apps.css` (new), `web/src/main.tsx`, `web/src/session/timeline.ts`, `web/src/session/timeline.test.ts`, `web/src/chat/ChatView.tsx`, `web/src/session/SessionView.tsx`, `web/e2e/fake-deck.ts`, `web/e2e/mcp-apps.spec.ts` (new), `web/e2e/gallery.spec.ts`, `web/e2e/gallery-data.ts`.
- **Changes:** §4.8 frame, §4.8.2 timeline and row, §4.8.3 texts and dialogs (with the open-call lines and the release dialog); gallery shot "MCP app in the chat" (both themes, desktop and phone) and "MCP app with an uncertain action".
- **Tests:** unit: "tool.app attaches to its tool node and ignores unknown nodes"; "member sees the owner-only note and no frame"; "at most three frames are mounted"; "consent dialog texts and Allow sends confirmed"; "error states (offline, gone, load error, could not start)"; "open calls show one line each; It has stopped asks, then releases". Playwright `mcp-apps.spec.ts` against the fake deck that serves `/app/{token}` with the §4.7 CSP and the exact bootstrap, and serves `/a/{token}/` test pages with the artifact CSP of `artifact_routes.rs:73`: "the app document cannot fetch /api (blocked by CSP)"; "the app cannot read document.cookie of the board (opaque origin)"; "navigating the frame stops the bridge"; "a replacement document gets no answer before its load event" (the demo app navigates its frame to an `/a/{token}/` page whose first inline script, before a slow image that delays `load` by 3 s, sends `tools/call` of `bump` with `window.parent.postMessage` and then a forged hello with its own `MessageChannel` port: the fake deck's call route counts zero requests, neither port nor window gets an answer, and the row shows "The app left its page and was stopped." before the replacement's `load`); "a button in the demo app calls bump after Allow and the counter text changes".
- **Acceptance:** `cd web && bun run test src/apps src/session/timeline src/chat` (through heavy.sh); `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; `cd web && bun run build` then `FD_E2E_PORT=<free port> ./node_modules/.bin/playwright test e2e/mcp-apps.spec.ts` and the gallery shots (both through heavy.sh); the guards.

### T9 — End-to-end proof and live smoke (backend, about 250 lines)

- **Depends on:** all.
- **Owned files:** `spikes/mcp-apps/demo_app_server.py` (new, from the scratch server), `spikes/mcp-apps/README.md` (new), `crates/fleetdeck-cli/tests/mcp_apps.rs` (new), `tools/smoke-mcp-apps.sh` (new).
- **Changes:** a deck + station test with the testkit fake provider that emits one candidate: `tool.app` reaches the deck timeline; `GET …/apps/{id}` returns the view; `/app/{token}` returns the document with the CSP; `POST …/call` (with the CSRF header of the owner's cookie login) round-trips through the fake bridge and leaves a settled `mcp_app_calls` row; `POST …/call` without the CSRF header is 403. The live smoke starts one Codex and one Claude session on Lighting with the demo server (`-c mcp_servers.demo…` for Codex, `--mcp-config` for Claude through a station test profile), asks for `show_counter`, and checks that `tool.app` arrives, that a `bump` call with `confirmed: true` changes the counter, and that a revert of the turn before the `bump` call is refused with `app_call_ran_after_turn`.
- **Acceptance:** `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(mcp_apps)'`; `sh tools/smoke-mcp-apps.sh` (manual, owner machine, records its frames under `~/fleetdeck-briefs/backlog/mcp-apps/smoke/`); the guards.

## 7. Decisions made in this draft

1. Phase 1 covers **Claude and Codex**, not Codex only. Rejected: Codex only as t3code (Claude 2.1.308 has the three control requests, §2.4).
2. Codex gets the extension only at 0.163.0 or later, by a cached `--version` probe of the adapter. Rejected: always send it (0.160–0.162 are unrecorded); a station setting.
3. Claude gets `CLAUDE_CODE_MCP_APPS_HOST=1` on every launch and apps only when `system/init` has the three capabilities. Rejected: a version gate on the variable (an older CLI ignores it).
4. The app document lives in a station table `mcp_apps`. Rejected: the artifact store (lists, CSP, kind parsing in older decks fails on an unknown kind: `crates/fleetdeck-deck/src/store/artifacts.rs:227-230`); a deck copy.
5. The deck relays the document per view through `/app/{token}` with a 10-minute in-memory token. Rejected: `srcdoc` (it inherits the board CSP, which blocks inline scripts); a database token table (a migration for a short-lived value).
6. Cores emit `Output::AppCandidate`; the station captures. Rejected: capture inside the core (the document is too large for an event, fact 8); capture from `provider_refs` (the result would travel in the event log).
7. The turn is **not** held open until the capture lands; `tool.app` may follow `turn.completed`. Rejected: t3code's turn hold (it needed cancel and settle rules after review).
8. Capture limits: 2 MiB document, 64 KiB input, 512 KiB result, 20 s read, 4 per station and 2 per session, 50 rows per session and 500 per station. Rejected: t3code's 5 MiB (no deck cache here, every view relays).
9. Four non-mutating link methods with feature `mcp_apps.v1`; `session.app_call` and `session.app_release` have scope Drive. On the board API the two POST routes are mutating (CSRF). Rejected: a durable `SessionCommand` (an app call is not replayable; a receipt adds nothing); scope Admin (the agent can already call the same tools with Drive prompts).
10. Owner only. Rejected: members of a shared session (owner question 3).
11. The station enforces same server, a unique Claude address, the `app` visibility, `ui://` reads and the confirmation of non-read-only tools. Rejected: board-only checks (both providers skip them, §2.4).
12. Confirmation through a `Confirm` answer and a second call with `confirmed: true`. Rejected: a separate tool-info route.
13. `ui/message` reuses the composer's send path after a dialog. Rejected: a new station method for app messages.
14. Phase 1 bridge subset of §4.8; inline only. Rejected: full screen, model context, downloads in phase 1.
15. One opaque frame, no sandbox proxy, no `'self'` in the CSP; the bridge talks only on a `MessagePort` that the deck's bootstrap creates before any app script runs, with `window.parent` redefined as that port. Rejected: the spec's double-iframe proxy (it needs `allow-same-origin` on a second origin that Fleet Deck does not have); a window-message bridge guarded only by `event.source` and the second `load` (a replacement document passes both checks before its `load`).
16. `frameDomains`, `baseUriDomains` and permissions are ignored. Rejected: granting them in phase 1.
17. The deck prefixes the cookie facade and the color-scheme bootstrap at serve time (t3code #17026, #17027). Rejected: storing modified HTML.
18. At most 3 mounted frames per chat, mounted in view. Rejected: every app mounted (CPU), click-to-show only (owner question 1).
19. A deck audit row for each `tools/call`, never the arguments. Rejected: no audit; a timeline event per call.
20. One host module with a policy object for the chat now and the wall later. Rejected: a separate wall host.
21. No apps from the Fleet Deck MCP server in phase 1.
22. No station setting in phase 1 (owner question 2).
23. Apps of subagents are out of scope. Rejected: child-thread reads in phase 1.
24. Every app `tools/call` is a durable `mcp_app_calls` row inserted under the workspace fence; a non-read-only call advances the folder generation at its start and its end, like a `!` shell command. Rejected: holding the fence over the whole call (it would block the session's inputs for up to the call's length, and a timed-out call could still write after the fence is released); no tracking (a revert could restore files that the tool is writing).
25. A tool call has no provider-side timeout; the station answers `timeout` after 60 s but keeps the row and the permit until the provider answers or the process ends. Rejected: dropping the wait at 60 s (the outcome would be unknown while the process runs).
26. After a station restart an unproven call is `uncertain` until the shim proves the provider's end or the owner releases it. Rejected: settling it at the start (a direct host's provider survives a station crash); a time limit (Claude's MCP tool timeout is not bounded).
27. Claude pairs go through `ClaudeToolMap`: only a unique, addressable pair is captured or called; the `can_use_tool` server name is evidence when it exists. Rejected: the first match; reconstructing names without the CLI's own parse.
28. A shim session gets its bridge and its app sink in `insert_durable`, so `adopt` restores both; candidates of the replay wait in the driver until the sink attaches. Rejected: registering bridges only after start and resume.
29. A deleted app is told apart from an unknown one by its `tool.app` event in the session log. Rejected: a tombstone row (more state for the same answer).

## 8. Owner questions (taste or policy; the build goes on with the default)

1. **Show apps at once or after a click?** Default: at once, inline, when the row comes into view (at most 3 at a time), as t3code does.
2. **A station switch "MCP Apps" in Settings → station?** Default: no switch in phase 1; add one in phase 2 if a server behaves worse when the extension is offered.
3. **May members of a shared session open and use apps?** Default: no, owner only in phase 1.
4. **Phase 2 scope:** full screen, `ui/update-model-context`, downloads. Default: yes, in that order, after a week of phase 1 use.
5. **Apps on the wall (B17 item 4):** Default: a separate design after phase 1 that reuses the host module with a read-only policy.
6. **Native cards for the Fleet Deck coordinator tools** (instead of shipping them as apps): Default: no work now; note it in the backlog.

## Decision log entries (to append)

## DL-450 — MCP Apps phase 1 for Claude and Codex (B15)

- Context: B15 asked whether Fleet Deck can show MCP Apps in the chat as t3code does. Live recordings on 2026-10-23 prove the provider paths: Codex 0.163.0 (`io.modelcontextprotocol/ui` extension in `initialize`, `mcpAppResourceUri` on `mcpToolCall`, `mcpServer/resource/read`, `mcpServer/tool/call`, `mcpServerStatus/list`) and Claude 2.1.308 (`CLAUDE_CODE_MCP_APPS_HOST=1`, `mcp_status` with `_meta.ui`, `mcp_read_resource`, `mcp_call` with `result_not_for_model`).
- Choice: phase 1 renders MCP Apps inline in the chat for both providers. Codex at or above 0.163.0 (cached `--version` probe); Claude when `system/init.capabilities` has `mcp_read_resource_v1`, `mcp_tool_ui_meta_v1` and `mcp_call_result_not_for_model_v1`.
- Rejected: Codex only.
- Review: high risk; adversary DESIGN REVIEW of `docs/v2/design/mcp-apps.md` before the build.

## DL-451 — App documents live on the station; the deck relays them per view

- Choice: a station table `mcp_apps` (migration V46) keeps the document (at most 2 MiB), the tool input (64 KiB) and the tool result (512 KiB); 50 rows per session, 500 per station. The deck keeps no copy: `GET /api/sessions/{station}/{session}/apps/{app_id}` mints a 10-minute in-memory token and `GET /app/{token}` serves the document in chunks from the station with the app CSP.
- Rejected: the artifact store; a deck cache; `srcdoc` frames.

## DL-452 — Capture after the tool call, without holding the turn

- Choice: provider cores emit `Output::AppCandidate`; a station capture task resolves the `ui://` URI (Claude through `mcp_status`), reads it (20 s), validates it, stores it and appends the additive event `tool.app`. A failure leaves a plain tool row. The turn is never held open.
- Rejected: t3code's turn hold; capture inside the core.

## DL-453 — The app request path is owner-only and station-enforced

- Choice: link methods `session.app_meta` (Read), `session.app_document` (Read), `session.app_call` (Drive) and `session.app_release` (Drive), all non-mutating, feature `mcp_apps.v1`. Deck routes are owner-only and in the board-API method table; the POST routes are mutating (CSRF). The station resolves the app from its own row, reaches only that server, calls a Claude tool only by a unique, addressable name, refuses tools without the `app` visibility, allows only `ui://` reads, answers `confirm` for every tool that is not read-only until the board sends `confirmed: true`, rate-limits (2 unsettled calls, 30 per minute per session), and never starts, resumes or wakes a session. Every tool call is a durable row under the workspace fence that blocks a revert of its folder while it runs or its end is unproven; a non-read-only call advances the folder generation (the B8 barrier).
- Rejected: a durable session command; board-only checks; members and coordinators in phase 1.

## DL-454 — Phase 1 host bridge and consent

- Choice: `ui/initialize`, tool-input and tool-result replay, host context with literal theme colors, `size-changed`, `host-context-changed`, `request-teardown`, `resource-teardown`, `ping`, `tools/call` and `resources/read` through the deck, `ui/message` (owner dialog, then the normal composer send), `ui/open-link` (owner dialog, http and https only), inline display only. Not supported: full screen, model context, downloads, sampling.
- Rejected: t3code's full host side in one step.

## DL-455 — One opaque frame with a per-app CSP

- Choice: `sandbox allow-scripts` in both the frame attribute and the CSP header, never `allow-same-origin`; the CSP is built from the sanitized `connectDomains` and `resourceDomains` only, with no `'self'`, no frames, no forms, no `base`. The deck prefixes one bootstrap: a cookie facade, a color-scheme listener and the bridge channel (a `MessagePort` sent to the board in one hello before any app script runs; `window.parent` becomes that port). The board talks only on that port; a second hello or a second `load` closes the bridge.
- Rejected: the spec's double-iframe sandbox proxy; reusing the artifact CSP.

## DL-456 — One app host module for the chat and the wall

- Choice: `web/src/apps/bridge.ts` takes a policy object; phase 1 ships the chat policy. Apps on the wall (B17 item 4) need their own design and DESIGN REVIEW (scripts on the wall, the display principal, a read-only policy).
- Rejected: a separate wall host.

## DL-457 — Fleet Deck's own MCP tools ship no apps in phase 1

- Choice: the coordinator MCP server returns no `_meta.ui`. The generic path would render such apps later, but the board's native views are the better place for fleet data.
- Rejected: a fleet-view app in the chat now.

## DL-458 — No owner switch in phase 1

- Choice: the extension offer follows the provider gates of DL-450 only; no station setting yet (owner question 2 of the design).
- Rejected: a Settings → station switch in phase 1.

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. Show apps at once in the chat, or only after a click? — **The recommended default: At once, inline, when the row comes into view; at most 3 frames mounted per chat**
2. Add a station switch 'MCP Apps' in Settings, station tab, in phase 1? — **The recommended default: No; add it in phase 2 only if a server behaves worse when the extension is offered**
3. May members of a shared session open and use apps? — **The recommended default: No, owner only in phase 1**
4. Phase 2 scope: full screen, ui/update-model-context, downloads? — **Not in this program: phase 1 only. Phase 2 stays a backlog item.**
5. Apps on the wall (B17 item 4)? — **The recommended default: A separate design after phase 1 that reuses the host module with a read-only policy**
6. Native result cards for the Fleet Deck coordinator tools instead of apps? — **The recommended default: No work now; note it in the backlog**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.

## Revision 2 (design review round 1)

The adversary design review round 1 (`~/fleetdeck-briefs/backlog/mcp-apps/adv-design-r1.md`, commit `00055138`) failed with 3 BLOCKER, 2 MAJOR and 2 MINOR findings. Each finding was checked against the code before the change. All seven are fixed.

| # | Finding | Verified | What changed |
| --- | --- | --- | --- |
| 1 | BLOCKER, conf. 0.8: app tool calls bypass the workspace fence and the durable barrier; a revert can restore files while a confirmed tool writes; a timed-out call can go on writing after its permit is released. | Yes: `revert_run.rs:75-90`, `:126` and `shell_runs.rs:428-451` cover only `shell_runs`; the barrier is not checked again under the fence (fact 16). | New §4.6.1 and the `mcp_app_calls` table (§4.4). Every `tools/call` is a durable row inserted under the fence; a non-read-only call advances the folder generation at its start and after its settlement (the B8 rule). `AppBridge::call_tool` has no timeout and ends with `AppError::Ended` at the process exit (§4.2), so the call task always settles the row; the permit is held until the settlement. The revert meets the app barrier in `shell_block` (dispatch, retry, recovery) and checks the barrier a second time under the fence. Station start: unproven calls become `uncertain`; a shim proof settles them; the owner can release an `uncertain` call (`session.app_release`, release route, UI lines and dialog in §4.8.3). Threat rows, decisions 24-26, DL-453, T4, T5b, T6, T7, T8, T9 updated. |
| 2 | BLOCKER, conf. 0.8: a replacement document can use the bridge before its `load` event, because `event.source` and the `null` origin stay the same after a navigation. | Yes: the bridge rules of revision 1 (§4.8) and the artifact CSP (`artifact_routes.rs:73`). | The deck bootstrap (§4.8, exact source) creates a `MessageChannel` before any app script runs, sends one port to the board in a hello, and redefines `window.parent` as the kept port. The board reads requests and sends answers only on that port; window messages from the frame are dropped; a second hello or a second `load` closes the bridge and drops pending answers. Threat row, decision 15, DL-455, T6 (bootstrap constant and test), T7 (handshake tests) and T8 (Playwright test of a replacement document that sends requests and a forged hello before its `load`) updated. |
| 3 | BLOCKER, conf. 0.8: Claude name reconstruction is many-to-one; capture can pick another server's HTML and a call can be checked for one pair and run on another. | Yes: Claude's own parse (`Jr` in 2.1.308, fact 20) splits on `__` and takes the second part as the server; `mcp_call` takes only the full name. | `ClaudeToolMap` with three addressability rules (§4.2): capture and call continue only for a unique pair whose name round-trips through the CLI's parse and whose normalized server name is unique; never the first match. The `can_use_tool` `mcp_server.name` is kept as `server_hint` and must agree (§4.2 Claude, §4.3 item 4). The stored tool is the raw name. Tests for delimiter, normalization and cross-server collisions in T2, T4 and T5b. |
| 4 | MAJOR, conf. 0.7: the API routes are not in the board-API method table and answer 404. | Yes: `guard.rs:505-516` refuses a route without a `methods::find` row. | New `methods_table_apps.rs` with three owner-only rows (view: Read; call and release: Drive, mutating, so a cookie login needs CSRF), chained in `methods::api()` (§4.7). T6 owns both files and tests through the guarded router; T7 sends the POSTs through `deckHttp` (CSRF header). |
| 5 | MAJOR, conf. 0.7: shim sessions restored at the station start get no bridge, and the candidates of the replay have no defined path. | Yes: `target.rs:253-281`, `shim_station.rs:179-240`, `shim_target.rs:133-178`. | §4.3 items 1 and 2: the registry entry and the app sink are installed in `insert_durable`, which serves a new launch and `adopt`; the driver keeps candidates committed before the sink attaches (also during the restore replay) and hands them over in order; an exited restored session gets neither. New task T5a (wiring and restore) with unit tests for the buffer and `adopt`; T5b adds the CLI restart test with a surviving sim Codex provider. The old T5 is split into T5a and T5b (dependencies in the order line). |
| 6 | MINOR, conf. 0.8: T1 cannot compile, because `SessionSummary::fold` matches every event. | Yes: `fold.rs:38-335`; also `event_kinds.rs:8`, `:74-131`. | `fold.rs` and `event_kinds.rs` are in T1's owned files; `ToolApp` joins the no-change arm; `kind_name` and `KINDS` (62) list it (§4.5). |
| 7 | MINOR, conf. 0.7: `410 app_gone` cannot be told apart from `unknown_app`. | Yes: revision 1 had only `Option<AppMeta>`. | `AppLookup { Found, Gone, Unknown }` (§4.4): `Gone` when the session log has the app's `tool.app` event but the row is deleted. Every method answers `app_gone` or `unknown_app` from it (§4.6); tests in T4 and T5b. |

Other changes for consistency: the migration is V46 (`schema_v46.rs`), the number that the architect decisions give this track (V43 is taken by DL-402); facts 15 to 20 were added; T5a moves `SessionTarget::adopt` and `SessionTarget::stop_all` into new files so that `shim_target.rs` and `target.rs` stay under 800 lines, and no line is added to `manager/mod.rs` (797 lines).

Findings not fixed: none.
