# MCP Apps: interactive mini UIs in the chat (B15)

- **Status:** Draft, revision 1 (2026-10-23). Risk: **high** (a new command path from the board to a live provider session, untrusted HTML and JavaScript next to the board, a station migration). It needs an adversary DESIGN REVIEW before freeze and a PRE-PR REVIEW before the merge into `v2`.
- **Owner request (B15):** "Study how t3code shows custom mini UIs inside its chat, and decide whether Fleet Deck can do the same and how."
- **Base:** `v2` at `497b2fbc`. All `file:line` references are on that commit.
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
15. **Module size.** Every Rust and TypeScript module stays under 800 lines (`tools/module-size.sh`). Files near the limit: `codex/core.rs` 782, `codex/map.rs` 768, `link/intake.rs` 768, `manager/policy.rs` 785, `web/src/chat/ChatView.tsx` 715. New code goes into new files.

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

Board → deck → station → live provider session, through three new link methods and two deck routes (§4.6, §4.7). **Owner only** in phase 1: a member of a shared session, a token, a coordinator and a display get 403. The station accepts the methods only from a paired deck link (the normal link authentication) and checks the app row, the session, the server, the tool visibility and the confirmation. The path never starts, resumes or wakes a session. This is a new command path: adversary DESIGN REVIEW.

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
  └─ core: Output::AppCandidate { node, server?, tool, uri?, arguments, result }
       └─ station capture task: resolve uri (Claude: mcp_status) → read resource (20 s)
            → validate (MIME, ≤ 2 MiB, UTF-8) → sanitize csp → INSERT mcp_apps → event tool.app
board timeline: tool row + tool.app → AppFrame
  └─ GET  /api/sessions/{st}/{s}/apps/{app}        (owner) → deck: session.app_meta + view token
  └─ <iframe sandbox="allow-scripts" src="/app/{token}">  → deck: session.app_document chunks, app CSP
  └─ bridge: ui/initialize, tool-input, tool-result, size, theme
  └─ POST /api/sessions/{st}/{s}/apps/{app}/call   (owner) → deck → station: session.app_call
       → AppBridge (Codex mcpServer/tool/call | Claude mcp_call) on the live session
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

/// The MCP operations of a live session for its apps. Cloneable handle: it
/// never needs the station's `Live` lock.
#[async_trait]
pub trait AppBridge: Send + Sync {
    async fn tools(&self) -> Result<Vec<AppTool>>;
    /// `ReadResourceResult` JSON (`{ "contents": [...] }`).
    async fn read_resource(&self, server: &str, uri: &str) -> Result<serde_json::Value>;
    /// `CallToolResult` JSON.
    async fn call_tool(&self, server: &str, tool: &str, arguments: serde_json::Value)
        -> Result<serde_json::Value>;
}

pub const TOOLS_TIMEOUT: Duration = Duration::from_secs(10);
pub const READ_TIMEOUT: Duration = Duration::from_secs(20);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The provider families and minimum versions with a recorded app path.
pub const APPS_VERIFIED: [(ProviderKind, &str); 2] =
    [(ProviderKind::Claude, "2.1.308"), (ProviderKind::Codex, "0.163.0")];
pub fn apps_verified(kind: ProviderKind, version: Option<&str>) -> bool;
/// Claude's tool name of an MCP tool: `mcp__{n(server)}__{n(tool)}`, where
/// `n` replaces every character outside `[A-Za-z0-9_-]` with `_`.
pub fn claude_tool_name(server: &str, tool: &str) -> String;
/// `"text/html;profile=mcp-app"` after removing whitespace and lowercasing.
pub fn is_app_mime(mime: &str) -> bool;
```

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
- App requests: `CodexCore::app_request(op) -> (String, Vec<Output>)` writes `mcpServerStatus/list {threadId, detail: "toolsAndAuthOnly"}` (follow `nextCursor` at most 5 pages), `mcpServer/resource/read {threadId, server, uri}` or `mcpServer/tool/call {threadId, server, tool, arguments}` with a host id `fd-app-<n>`. Answers go into `app_answers: HashMap<String, Result<Value, String>>` (`#[serde(skip)]`, so a checkpoint never holds them); `take_app_answer(id)` removes one. An answer whose id is unknown is dropped without a warning count (a late answer after a restart).
- The direct driver implements `AppBridge` on a handle that holds the shared core and the write queue; it waits with the existing notify and the timeouts of §4.2; a timeout removes the pending id.

**Claude** (new files `claude/apps.rs`, `claude/driver_apps.rs`, `claude/tests_apps.rs`):
- Every Claude launch sets `CLAUDE_CODE_MCP_APPS_HOST=1` in the child environment (direct launch and `durable_launch.rs`). An older CLI ignores the variable.
- The core sets `mcp_apps_ready = true` when the root `system/init.capabilities` contains all of `mcp_read_resource_v1`, `mcp_tool_ui_meta_v1` and `mcp_call_result_not_for_model_v1`. Without it, no candidate and no bridge (`app_bridge()` answers a bridge whose calls fail with `ProviderError::Unsupported`).
- `Output::AppCandidate` is a new variant of `claude::Output`. On a `tool_result` block of the main thread (`parent_tool_use_id` null) for a tool node whose name starts with `mcp__`, status `Ok`, the core emits a candidate with `server: None`, `tool` = the full name, `arguments` = the tool input, `result` = `tool_use_result` of the user frame normalized to a `CallToolResult`: a string `content` becomes `[{"type":"text","text":<string>}]`; `structuredContent` is kept.
- App requests: control requests `mcp_status`, `mcp_read_resource {serverName, uri}`, `mcp_call {tool: claude_tool_name(server, tool), arguments, result_not_for_model: true}` with request ids `fd-app-<n>`; the `control_response` (`success` → `response`, `error` → the error text) fills `app_answers` (`#[serde(skip)]`).

### 4.3 Station capture (`crates/fleetdeck-station/src/apps/`)

1. **Intake of candidates.** Direct host: after a start or resume, the station takes `take_app_candidates()` and spawns one forwarder per session to the capture service. Shim host: `Steps` gets `apps: Vec<AppCandidate>`; `split()` of `shim_codex.rs` and `shim_claude.rs` fills it; the durable driver hands them to the capture service after the output transaction commits.
2. **Bridge registry.** The station keeps `HashMap<SessionId, Arc<dyn AppBridge>>` beside the `Live` map: added after a start or resume (direct: `app_bridge()`; shim: the `Arc<DurableDriver<C>>`, which implements `AppBridge` with `internal`, `wait_until` and `with_core`), removed when the process ends.
3. **Limits.** At most 4 captures run at a time on a station and 2 per session; more candidates wait in a queue of 32 per station; a full queue drops the newest candidate with a warning.
4. **Resolve.** Codex: `server` and `resource_uri` come with the candidate. Claude: `bridge.tools()` (cached per session for 30 s; a miss refreshes the cache once); the tool whose `claude_tool_name(server, name)` equals the candidate's tool and whose `resource_uri` is set gives `server` and `resource_uri`. No match: stop (plain row).
5. **Read.** `bridge.read_resource(server, uri)` with `READ_TIMEOUT`. In `contents`, take the first item whose `uri` is absent or equal to the URI and whose `mimeType` passes `is_app_mime`. Check the size **before** decoding: `text` length at most 2 MiB; `blob` length at most ⌈2 MiB / 3⌉ × 4, then base64 decode. The bytes must be UTF-8 and not empty after trim.
6. **Sanitize.** `_meta.ui.csp.connectDomains` and `.resourceDomains`: keep a string only if it matches `^(https?|wss?)://(\*\.)?[a-z0-9-]+(\.[a-z0-9-]+)*(:[0-9]{1,5})?$` (case-insensitive) and is at most 256 characters; at most 32 per list; duplicates removed. `frameDomains`, `baseUriDomains` and `_meta.ui.permissions` are ignored (phase 1 grants none). `_meta.ui.prefersBorder` is kept when it is a boolean.
7. **Store and emit.** In one store transaction: insert the row of §4.4 and append the event `tool.app` of §4.5 for the session. Then apply the caps: more than 50 rows of the session or 500 rows of the station → delete the oldest rows (the events stay; the board then gets `410 app_gone`).
8. **Failure.** Any failure (no bridge, timeout, provider error, size, MIME, UTF-8) writes one `warn` log line with the session, server, tool and URI (never the document) and emits nothing: the row stays a plain tool row.
9. **No turn hold.** The turn ends when the provider ends it. `tool.app` can arrive after `turn.completed`; the board attaches it to its node.

### 4.4 Station store (migration v43, `crates/fleetdeck-station/src/store/schema_v43.rs`)

```sql
CREATE TABLE mcp_apps (
  app_id        TEXT PRIMARY KEY,          -- ULID
  session_id    TEXT NOT NULL,
  node_id       TEXT NOT NULL,
  server        TEXT NOT NULL,
  tool          TEXT NOT NULL,             -- the tool name on the server (Claude: without the mcp__ prefix)
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
```

Store API (`crates/fleetdeck-station/src/store/apps.rs`): `insert_app(NewApp, ToolAppEvent) -> Result<AppId>` (row and event in one transaction, then the caps), `app_meta(session, app_id) -> Result<Option<AppMeta>>`, `app_chunk(session, app_id, offset, len) -> Result<Option<(Vec<u8>, u64)>>`. A query with a session id that is not the row's session answers `None`.

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
}
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

Link feature `mcp_apps.v1` (`feature::MCP_APPS`, in the list of features of `version.rs`).

### 4.6 Link methods (`crates/fleetdeck-link/src/methods_table_tail.rs`)

| Method | Callers | Scope | Feature | Mutating |
| --- | --- | --- | --- | --- |
| `session.app_meta` | DeckToStation | Read | `mcp_apps.v1` | false |
| `session.app_document` | DeckToStation | Read | `mcp_apps.v1` | false |
| `session.app_call` | DeckToStation | Drive | `mcp_apps.v1` | false |

Station handling (`crates/fleetdeck-station/src/link/intake_apps.rs`; one dispatch line in `intake.rs`):

- `session.app_meta`: the row of `(session_id, app_id)` → `AppMeta`, else `unknown_app`.
- `session.app_document`: `offset` ≤ `doc_bytes`, else `invalid_params`; at most `APP_DOC_CHUNK_BYTES` from `offset`.
- `session.app_call`, in this order:
  1. The row exists for the session, else `unknown_app` ("This app is no longer stored.").
  2. The session has a bridge in the registry, else `session_stopped` ("The session that made this app is not running. Resume it to use the app."). Never a start, resume or wake.
  3. Per session at most 2 calls at a time and 30 per minute, else `busy` ("The app sends too many requests. Wait a moment.").
  4. `ResourcesRead { uri }`: `uri` starts with `ui://` and is at most 4096 characters, else `invalid_params`; `bridge.read_resource(row.server, uri)`.
  5. `ToolsCall { name, arguments }`: `arguments` is an object (or `null`, sent as `{}`), serialized at most `MAX_APP_ARGS_BYTES`, else `invalid_params`. `bridge.tools()` (no cache older than 10 s); the tool `(row.server, name)` must exist and its `visibility` must be `None` or contain `"app"`, else `tool_not_callable` ("This app cannot call that tool."). If the tool is not `read_only` and `confirmed` is false: answer `Confirm { tool: name, title }` without a call. Else `bridge.call_tool(row.server, name, arguments)`.
  6. A provider result over `MAX_APP_REPLY_BYTES` serialized → `too_large` ("The answer is too large for the app."). A timeout → `timeout` ("The tool did not answer in 60 s. It may still run."). A provider error → `provider_error` with the provider text cut to 300 characters, control characters removed.

### 4.7 Deck (`crates/fleetdeck-deck/src/http/app_routes.rs`, `app_public.rs`, `app_csp.rs`)

Routes (added in `http/mod.rs` beside the subagent route and the `/a/{token}` routes):

| Route | Who | Does |
| --- | --- | --- |
| `GET /api/sessions/{station}/{session}/apps/{app_id}` | owner login only (`owner_only`) | Feature check (`Without` → 409 `apps_unsupported`, `Down` → 409 `station_unreachable`); `session.app_meta` (wait 20 s); mints a view token; answers `AppView` with `url: "/app/{token}"`. |
| `POST /api/sessions/{station}/{session}/apps/{app_id}/call` | owner login only | Body `AppCallBody` at most 128 KiB; at most 60 calls per minute per login; `session.app_call` (wait 70 s); answers `AppCallReply`; writes an audit row for every `tools/call` (action `session.app_call`, detail `server/tool`, the result `ok`, `confirm`, or the error code; never the arguments). |
| `GET /app/{token}` (also `HEAD`) | anyone with the token | 404 for an unknown or expired token. Fetches the document with `session.app_document` chunks (total wait 30 s), checks `doc_sha256`, prefixes the two bootstraps of §4.8, and answers with the headers below. |

- **View tokens.** 32 random bytes (base64url), kept in deck memory as `sha256(token) → {station_id, session_id, app_id, csp, login_session_hash, expires_at}`; life 10 minutes; at most 512 entries (the oldest goes first); a deck restart forgets them (the board asks again). A token can load its document more than once within its life (a reload of the frame).
- **Answer headers of `/app/{token}`:** `Content-Type: text/html; charset=utf-8`, `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and `Content-Security-Policy` =

  ```
  sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' R; style-src 'unsafe-inline' R;
  img-src data: blob: R; font-src data: R; media-src data: blob: R; connect-src C;
  frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'
  ```

  where `R` is the space-joined `resource_domains` (empty → nothing) and `C` is the space-joined `connect_domains`, or `'none'` when empty. There is no `'self'` in any fetch directive: the app must not reach the deck API. The domains are sanitized again on the deck with the pattern of §4.3 item 6 (defense in depth).
- **HTTP errors of the API routes:** 403 `owner_only`; 404 `unknown_station`, `unknown_session`, `unknown_app`; 409 `apps_unsupported`, `station_unreachable`, `session_stopped`; 410 `app_gone` (row deleted by the caps); 400 `invalid_params`; 403 `tool_not_callable`; 413 `too_large`; 429 `busy`; 502 `provider_error`, `station_error`; 504 `timeout`.

### 4.8 Board (`web/src/apps/`)

**Bootstraps** (prefixed by the deck, after a leading `<!doctype …>` if there is one): (1) an empty, non-persistent `document.cookie` facade installed only when reading `document.cookie` throws `SecurityError` (t3code #17027); (2) a listener that sets `document.documentElement.style.colorScheme` from `hostContext.theme` of the `ui/initialize` result and of `ui/notifications/host-context-changed`, accepting only messages whose `event.source === window.parent` (t3code #17026).

**Frame** (`AppFrame.tsx`): `<iframe sandbox="allow-scripts" referrerPolicy="no-referrer" title="App from {server}" src={view.url}>`, no `allow` attribute, width 100% of the chat column, height from `size-changed` clamped to 80–800 px (start 160 px). `prefersBorder` true → the frame sits on `--raised` with `--r-2` corners; else on `--bg` with no edge.

**Bridge** (`bridge.ts`, pure TypeScript, no React): one instance per mounted frame.
- Accepts a message only when `event.source === frame.contentWindow`, `event.origin === 'null'`, the bridge is open, the serialized size is at most 256 KiB, and it is a JSON-RPC 2.0 object. At most 16 requests of the app wait at a time (more → error `-32000` "Too many requests").
- The frame's second `load` event closes the bridge for good (the app left its page); the row shows "The app left its page and was stopped."
- Before `ui/initialize`, every request except `ping` gets `-32002` "Not initialized".
- `ui/initialize` → result `{ protocolVersion: "2026-01-26", hostInfo: { name: "fleet-deck", version }, hostCapabilities: { openLinks: {}, serverTools: {}, serverResources: {}, logging: {}, sandbox: { permissions: {}, csp: { connectDomains, resourceDomains } } }, hostContext }`.
- `hostContext`: `theme` (`light` | `dark` of the board), `styles.variables` (§4.8.1), `displayMode: "inline"`, `availableDisplayModes: ["inline"]`, `containerDimensions: { width: <frame width>, maxHeight: 800 }`, `locale`, `timeZone`, `platform: "web"`, `userAgent: "fleet-deck/<version>"`, `deviceCapabilities: { touch, hover }` (from `matchMedia`), `toolInfo: { tool: { name: <tool> } }`.
- After `ui/notifications/initialized`: `ui/notifications/tool-input { arguments }` (the stored input; `null` → `{}`), then `ui/notifications/tool-result` with the stored result (a `_meta` that is not an object is removed; a missing result → `{ content: [{ type: "text", text: "The result is too large to show in the app." }] }`).
- Theme or width change → `ui/notifications/host-context-changed` with the changed fields.
- `tools/call`, `resources/read` → the call route; `Confirm` → the consent dialog, then the call again with `confirmed: true`; Cancel → error `-32000` "Declined by the user".
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
| Navigated away | "The app left its page and was stopped." and "Reload app" |
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
- A station setting to turn the feature off (owner question 2).

### 4.10 Compatibility

| Peers | Behavior |
| --- | --- |
| New station, old deck | `tool.app` passes through as an unknown event; the old board ignores it; no route exists. Plain rows. |
| Old station, new deck | No `mcp_apps.v1`: no `tool.app` events; the routes answer 409 `apps_unsupported` if called. |
| New deck, old board | The board ignores `tool.app` (unknown kind in its fold). |
| Codex < 0.163.0 | No extension; no `ui://` URI on items; plain rows. |
| Claude < 2.1.308 | The variable is ignored; `system/init` lacks the capabilities; no candidates; plain rows. |
| Station restart | Stored apps stay; a view of a session whose process ended answers `session_stopped` for calls but still renders the stored document with its stored input and result. |

## 5. Risk class and threat notes

**High:** a new command path (board → live provider session), untrusted active content in the owner's browser, a station migration.

| Threat | Mitigation |
| --- | --- |
| The app reaches the deck API or the owner's cookies | Opaque origin twice (CSP `sandbox allow-scripts` header and the frame `sandbox="allow-scripts"`, never `allow-same-origin`); no `'self'` in the app CSP; the API needs the board cookie, which an opaque origin never sends; `frame-ancestors 'self'`. |
| The app navigates its frame to a page that poses as the app | The board CSP `frame-src 'self'` keeps the frame on the deck origin; deck pages refuse framing (`frame-ancestors 'none'`); the second `load` closes the bridge; tokens are unguessable and per app. |
| A forged app reference (impersonation) | The board never names a server or a URI: it names an `app_id`; the station takes the server from its own row, captured from the provider's own item (Codex) or its own `mcp_status` (Claude). A tool result that looks like an app reference has no effect. |
| The app calls a hidden or foreign tool | The station checks `(row.server, name)` against the live tool list and the `app` visibility (both providers skip this check, §2.4). |
| The app runs a mutating tool without the owner | Station answers `Confirm` for every tool that is not read-only; only a second request with `confirmed: true` runs it; only the owner login reaches the route. Residual: the tool's own `readOnlyHint` is the server's claim (the same as t3code). |
| A call bypasses the provider's permission mode | Accepted and stated: Codex `mcpServer/tool/call` and Claude `mcp_call` do not ask the provider's approval; the owner's confirmation replaces it. A bypass-mode session changes nothing here. |
| Data exfiltration from the app | `connect-src` only to declared domains, shown to the owner under the frame; no forms, no popups, no frames, no `base`. Residual: the app can show what it received (tool input and result) to its declared domains — that is its own server's data. |
| Key capture by a focused frame (t3code #15968 open decision) | Accepted residual, the same as HTML artifacts today: a frame that has focus receives keys. The board never moves focus into a frame by itself. |
| Oversized or hostile documents | 2 MiB cap checked before decoding; UTF-8 check; chunked relay; 256 KiB message cap; 16 pending requests; rate limits on the station (30/min/session, 2 concurrent) and the deck (60/min/login). |
| Resource exhaustion by a malicious app (CPU) | At most 3 mounted frames per chat; frames mount only in view. Residual: a frame can use CPU while mounted. |
| A stopped session is woken by an app | `session_stopped`; no start, resume or wake on this path. |
| Store growth | 50 rows per session, 500 per station, oldest out. |
| Capture blocks a turn or the effect worker | Captures run in their own tasks with their own limits; the bridge never takes the `Live` lock; the turn is not held. |

## 6. Build plan

Branch `mcp-apps` from `v2`; the branch merges into `v2` only after all tasks, the PRE-PR REVIEW and the live smoke (T1 adds a feature that the station advertises; the methods arrive in T5). Every task resets its worktree to the branch head first. Heavy commands only through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <cmd>`.

Order: **T1** → (**T2** ∥ **T4** ∥ **T6** ∥ **T7**) → **T3** (after T2) → **T5** (after T2, T3, T4) → **T8** (after T7, needs the T6 routes in the fake deck only) → **T9** (after all).

Every task runs the guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`. UI tasks also run `sh tools/design-check.sh`.

### T1 — Proto, event and link contracts (backend, about 300 lines)

- **Owned files:** `crates/fleetdeck-proto/src/apps.rs` (new), `crates/fleetdeck-proto/src/apps_tests.rs` (new), `crates/fleetdeck-proto/src/event.rs`, `crates/fleetdeck-proto/src/event_tests.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/ts.rs`, `crates/fleetdeck-link/src/methods_table_tail.rs`, `crates/fleetdeck-link/src/methods_tests.rs`, `web/src/gen/**` (generated only).
- **Changes:** §4.5 types and constants; the `tool.app` variant; `feature::MCP_APPS = "mcp_apps.v1"` in the feature list; the three method rows of §4.6; TS exports; regenerate `web/src/gen`.
- **Tests:** `apps_tests::app_request_wire_names` (`tools/call` and `resources/read` tags round-trip); `apps_tests::app_call_reply_outcomes` (`done` and `confirm` tags); `event_tests::tool_app_round_trips_and_omits_border` (no `prefers_border` key when `None`); `event_tests::old_peer_keeps_tool_app_as_unknown` (an `EventBody` of a build without the variant keeps it as `Unknown`, as the B6 test for `task.updated` does); `methods_tests::app_methods_scopes` (Read, Read, Drive; all non-mutating; feature `mcp_apps.v1`).
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(apps) | test(tool_app)'`; `cargo nextest run -p fleetdeck-link --no-fail-fast -E 'test(app_methods)'`; the guards.

### T2 — Provider shared layer and Codex (backend, about 550 lines)

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-providers/src/apps.rs` (new), `crates/fleetdeck-providers/src/lib.rs`, `crates/fleetdeck-providers/src/adapter.rs`, `crates/fleetdeck-providers/src/codex/core.rs`, `crates/fleetdeck-providers/src/codex/map.rs`, `crates/fleetdeck-providers/src/codex/apps.rs` (new), `crates/fleetdeck-providers/src/codex/core_apps.rs` (new), `crates/fleetdeck-providers/src/codex/driver.rs`, `crates/fleetdeck-providers/src/codex/driver_apps.rs` (new), `crates/fleetdeck-providers/src/codex/driver_probe.rs`, `crates/fleetdeck-providers/src/codex/tests_apps.rs` (new), `spikes/s2-codex/fixtures/mcp_app.jsonl` (new, from `rec-codex-ext1-turn.jsonl` in the fixture format of the other files).
- **Changes:** §4.2 shared module and the `LiveSession`/`ProviderAdapter` defaults; Codex: `set_offer_mcp_apps`, the `initialize` extension, `Output::AppCandidate`, candidate building, app requests and answers, the direct-driver `AppBridge`, `offers_mcp_apps` with a cached `--version` probe. `codex/core.rs` and `codex/map.rs` get only the hook lines (they are at 782 and 768 lines).
- **Tests:** `initialize_offers_the_ui_extension_only_when_set`; `completed_mcp_call_with_ui_uri_gives_one_candidate_after_tool_completed` (fixture `mcp_app.jsonl`: `server demo`, `tool show_counter`, URI, arguments `{}`, result with `_meta` removed because it is null); `mcp_call_without_uri_or_failed_gives_no_candidate`; `child_thread_mcp_call_gives_no_candidate`; `oversized_result_gives_candidate_without_result`; `app_requests_write_the_three_methods` (exact params of §4.2); `app_answer_is_taken_once_and_late_answers_are_dropped`; `apps_verified_gate` (0.162.1 false, 0.163.0 true, Claude 2.1.307 false, 2.1.308 true); `claude_tool_name_normalizes` (`my.server`, `a b` → `mcp__my_server__a_b`).
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(apps) | test(mcp_app) | test(ui_extension) | test(candidate)'`; the existing Codex replay: `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'binary(codex_core_replay) | binary(codex_driver_replay)'`; the guards.

### T3 — Claude provider (backend, about 450 lines)

- **Depends on:** T2.
- **Owned files:** `crates/fleetdeck-providers/src/claude/core.rs`, `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/claude/frames.rs`, `crates/fleetdeck-providers/src/claude/apps.rs` (new), `crates/fleetdeck-providers/src/claude/driver.rs`, `crates/fleetdeck-providers/src/claude/driver_apps.rs` (new), `crates/fleetdeck-providers/src/claude/durable_launch.rs`, `crates/fleetdeck-providers/src/claude/tests_apps.rs` (new), `spikes/s1-claude/fixtures/rust/mcp_app.jsonl` (new, from `rec-claude-turn.jsonl`).
- **Changes:** §4.2 Claude part: the environment variable on both launch paths; `mcp_apps_ready` from `system/init.capabilities`; `Output::AppCandidate` on main-thread `mcp__` results with the normalized `tool_use_result`; `mcp_status`, `mcp_read_resource`, `mcp_call` requests and answers; the direct-driver `AppBridge` and candidate receiver.
- **Tests:** `launch_env_sets_mcp_apps_host` (both launch paths); `init_capabilities_enable_apps_only_with_all_three`; `mcp_tool_result_gives_candidate_with_call_tool_result` (fixture: tool `mcp__demo__show_counter`, result `{content:[{type:text,text:"{\"count\":2}"}], structuredContent:{count:2}}`); `subagent_mcp_result_gives_no_candidate`; `denied_or_error_result_gives_no_candidate`; `app_requests_write_control_requests` (exact subtypes and fields); `control_error_answer_is_an_error`; `tools_parse_read_only_and_visibility` (from the recorded `mcp_status` answer).
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(apps) | test(mcp_app) | test(mcp_apps_host) | test(candidate)'`; `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'binary(claude_core_replay) | binary(claude_driver_replay)'`; the guards.

### T4 — Station store and capture service (backend, about 550 lines)

- **Depends on:** T1 (types). Uses `AppBridge`/`AppCandidate` from T2: start T4 after T2 merges, or in parallel against the T2 signatures of §4.2 copied into the brief (no edits to provider files).
- **Owned files:** `crates/fleetdeck-station/src/store/schema_v43.rs` (new), `crates/fleetdeck-station/src/store/migrations.rs`, `crates/fleetdeck-station/src/store/mod.rs`, `crates/fleetdeck-station/src/store/apps.rs` (new), `crates/fleetdeck-station/src/store/apps_tests.rs` (new), `crates/fleetdeck-station/src/apps/mod.rs` (new), `crates/fleetdeck-station/src/apps/capture.rs` (new), `crates/fleetdeck-station/src/apps/sanitize.rs` (new), `crates/fleetdeck-station/src/apps/capture_tests.rs` (new), `crates/fleetdeck-station/src/lib.rs`.
- **Changes:** §4.4 table and store API; §4.3 items 3 to 9 as `apps::Capture` (`offer(session_id, candidate, bridge)`), with a fake `AppBridge` in the tests.
- **Tests:** `insert_app_writes_row_and_event_in_one_transaction`; `caps_delete_oldest_rows_per_session_and_station`; `app_meta_of_another_session_is_none`; `chunk_reads_bounded_slices`; `capture_codex_candidate_stores_and_emits_tool_app`; `capture_claude_candidate_resolves_server_by_normalized_name`; `capture_refuses_wrong_mime_oversize_blob_before_decode_and_non_utf8`; `capture_timeout_emits_nothing`; `sanitize_keeps_only_scheme_origins` (drops `*`, `'self'`, paths, quotes, `javascript:`; keeps `https://*.example.com:8443`); `capture_limits_queue_and_concurrency`; `migration_v43_applies_on_v42`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(app) | test(migration_v43)'`; the guards.

### T5 — Station wiring and link intake (backend, about 500 lines)

- **Depends on:** T2, T3, T4.
- **Owned files:** `crates/fleetdeck-station/src/manager/durable.rs`, `crates/fleetdeck-station/src/manager/shim_codex.rs`, `crates/fleetdeck-station/src/manager/shim_claude.rs`, `crates/fleetdeck-station/src/manager/shim_target.rs`, `crates/fleetdeck-station/src/manager/target.rs`, `crates/fleetdeck-station/src/manager/target_apps.rs` (new), `crates/fleetdeck-station/src/link/intake.rs`, `crates/fleetdeck-station/src/link/intake_apps.rs` (new), `crates/fleetdeck-station/src/link/intake_apps_tests.rs` (new), `crates/fleetdeck-station/src/manager/apps_wired_tests.rs` (new).
- **Changes:** §4.3 items 1 and 2 (candidate forwarders, `Steps.apps`, the bridge registry, `AppBridge` for `DurableDriver<CodexCore>` and `DurableDriver<ClaudeCore>`, `offers_mcp_apps` passed to `DurableCodex::open` at `shim_target.rs:455`); §4.6 station handling. `intake.rs` gets one match arm (`"session.app_meta" | "session.app_document" | "session.app_call" => intake_apps::call(...)`).
- **Tests:** `app_call_unknown_app`; `app_call_session_stopped_never_starts`; `app_call_refuses_app_only_tool_of_other_server_and_hidden_tool`; `app_call_non_read_only_needs_confirmation_then_runs`; `app_call_read_only_runs_at_once`; `app_call_resources_read_needs_ui_scheme`; `app_call_rate_and_concurrency_busy`; `app_call_too_large_and_timeout`; `app_document_chunks_reassemble_to_sha256`; `direct_and_shim_candidates_reach_capture` (wired test with the fake provider of `fleetdeck-testkit`, both hosts); `bridge_registry_drops_on_process_end`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(app_call) | test(app_document) | test(candidates_reach_capture) | test(bridge_registry)'`; the guards.

### T6 — Deck routes and the app document route (backend, about 500 lines)

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-deck/src/http/app_routes.rs` (new), `crates/fleetdeck-deck/src/http/app_public.rs` (new), `crates/fleetdeck-deck/src/http/app_csp.rs` (new), `crates/fleetdeck-deck/src/http/app_views.rs` (new), `crates/fleetdeck-deck/src/http/app_routes_tests.rs` (new), `crates/fleetdeck-deck/src/http/mod.rs`.
- **Changes:** §4.7 routes, token map, CSP builder, bootstraps, audit rows.
- **Tests:** `view_is_owner_only` (member, token principal and display get 403); `view_without_feature_is_409`; `view_mints_token_and_document_route_serves_with_app_csp` (exact header string for empty and non-empty domains; no `'self'` in fetch directives); `document_route_unknown_or_expired_token_404`; `document_sha_mismatch_is_502`; `bootstraps_follow_doctype`; `call_route_maps_station_errors_to_http` (each code of §4.7); `call_route_writes_audit_for_tools_call_only`; `call_route_rate_limit_429`; `token_map_evicts_oldest_over_512`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(app_)'`; the guards.

### T7 — Board bridge library (ux, about 500 lines)

- **Depends on:** T1 (generated types).
- **Owned files:** `web/src/apps/bridge.ts` (new), `web/src/apps/bridge.test.ts` (new), `web/src/apps/theme.ts` (new), `web/src/apps/theme.test.ts` (new), `web/src/apps/actions.ts` (new), `web/src/apps/actions.test.ts` (new).
- **Changes:** §4.8 bridge (the policy object with the chat policy), the theme mapping of §4.8.1, the API calls (`view`, `call`) with shape checks in the style of `web/src/settings/desktopTools.ts:planOf`.
- **Tests (bun):** "drops messages from another source or a non-null origin"; "refuses requests before ui/initialize except ping"; "replays tool-input then tool-result after initialized and strips a null _meta"; "too many pending requests"; "oversized message is dropped"; "second load closes the bridge"; "confirm outcome asks, Cancel answers Declined by the user"; "ui/message joins text blocks and refuses non-text"; "open-link refuses non-web schemes"; "request-display-mode answers inline"; "unsupported methods answer -32601"; "theme variables are literal colors of the tokens".
- **Acceptance:** `cd web && bun run test src/apps` (through heavy.sh); `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; the guards.

### T8 — Board app row, dialogs and chat integration (ux, about 550 lines)

- **Depends on:** T7 (and T6 for the route contract; the tests use a fake).
- **Owned files:** `web/src/apps/AppFrame.tsx` (new), `web/src/apps/AppRow.tsx` (new), `web/src/apps/AppDialogs.tsx` (new), `web/src/apps/apps.test.tsx` (new), `web/src/styles/apps.css` (new), `web/src/main.tsx`, `web/src/session/timeline.ts`, `web/src/session/timeline.test.ts`, `web/src/chat/ChatView.tsx`, `web/src/session/SessionView.tsx`, `web/e2e/fake-deck.ts`, `web/e2e/mcp-apps.spec.ts` (new), `web/e2e/gallery.spec.ts`, `web/e2e/gallery-data.ts`.
- **Changes:** §4.8 frame, §4.8.2 timeline and row, §4.8.3 texts and dialogs; gallery shot "MCP app in the chat" (both themes, desktop and phone).
- **Tests:** unit: "tool.app attaches to its tool node and ignores unknown nodes"; "member sees the owner-only note and no frame"; "at most three frames are mounted"; "consent dialog texts and Allow sends confirmed"; "error states (offline, gone, load error)". Playwright `mcp-apps.spec.ts` against the fake deck that serves `/app/{token}` with the §4.7 CSP: "the app document cannot fetch /api (blocked by CSP)"; "the app cannot read document.cookie of the board (opaque origin)"; "navigating the frame stops the bridge"; "a button in the demo app calls bump after Allow and the counter text changes".
- **Acceptance:** `cd web && bun run test src/apps src/session/timeline src/chat` (through heavy.sh); `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; `cd web && bun run build` then `FD_E2E_PORT=<free port> ./node_modules/.bin/playwright test e2e/mcp-apps.spec.ts` and the gallery shot (both through heavy.sh); the guards.

### T9 — End-to-end proof and live smoke (backend, about 250 lines)

- **Depends on:** all.
- **Owned files:** `spikes/mcp-apps/demo_app_server.py` (new, from the scratch server), `spikes/mcp-apps/README.md` (new), `crates/fleetdeck-cli/tests/mcp_apps.rs` (new), `tools/smoke-mcp-apps.sh` (new).
- **Changes:** a deck + station test with the testkit fake provider that emits one candidate: `tool.app` reaches the deck timeline; `GET …/apps/{id}` returns the view; `/app/{token}` returns the document with the CSP; `POST …/call` round-trips through the fake bridge. The live smoke starts one Codex and one Claude session on Lighting with the demo server (`-c mcp_servers.demo…` for Codex, `--mcp-config` for Claude through a station test profile), asks for `show_counter`, and checks that `tool.app` arrives and that a `bump` call with `confirmed: true` changes the counter.
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
9. Three non-mutating link methods with feature `mcp_apps.v1`; `session.app_call` has scope Drive. Rejected: a durable `SessionCommand` (an app call is not replayable; a receipt adds nothing); scope Admin (the agent can already call the same tools with Drive prompts).
10. Owner only. Rejected: members of a shared session (owner question 3).
11. The station enforces same server, the `app` visibility, `ui://` reads and the confirmation of non-read-only tools. Rejected: board-only checks (both providers skip them, §2.4).
12. Confirmation through a `Confirm` answer and a second call with `confirmed: true`. Rejected: a separate tool-info route.
13. `ui/message` reuses the composer's send path after a dialog. Rejected: a new station method for app messages.
14. Phase 1 bridge subset of §4.8; inline only. Rejected: full screen, model context, downloads in phase 1.
15. One opaque frame, no sandbox proxy, no `'self'` in the CSP, navigation guard. Rejected: the spec's double-iframe proxy (it needs `allow-same-origin` on a second origin that Fleet Deck does not have).
16. `frameDomains`, `baseUriDomains` and permissions are ignored. Rejected: granting them in phase 1.
17. The deck prefixes the cookie facade and the color-scheme bootstrap at serve time (t3code #17026, #17027). Rejected: storing modified HTML.
18. At most 3 mounted frames per chat, mounted in view. Rejected: every app mounted (CPU), click-to-show only (owner question 1).
19. A deck audit row for each `tools/call`, never the arguments. Rejected: no audit; a timeline event per call.
20. One host module with a policy object for the chat now and the wall later. Rejected: a separate wall host.
21. No apps from the Fleet Deck MCP server in phase 1.
22. No station setting in phase 1 (owner question 2).
23. Apps of subagents are out of scope. Rejected: child-thread reads in phase 1.

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

- Choice: a station table `mcp_apps` (migration v43) keeps the document (at most 2 MiB), the tool input (64 KiB) and the tool result (512 KiB); 50 rows per session, 500 per station. The deck keeps no copy: `GET /api/sessions/{station}/{session}/apps/{app_id}` mints a 10-minute in-memory token and `GET /app/{token}` serves the document in chunks from the station with the app CSP.
- Rejected: the artifact store; a deck cache; `srcdoc` frames.

## DL-452 — Capture after the tool call, without holding the turn

- Choice: provider cores emit `Output::AppCandidate`; a station capture task resolves the `ui://` URI (Claude through `mcp_status`), reads it (20 s), validates it, stores it and appends the additive event `tool.app`. A failure leaves a plain tool row. The turn is never held open.
- Rejected: t3code's turn hold; capture inside the core.

## DL-453 — The app request path is owner-only and station-enforced

- Choice: link methods `session.app_meta` (Read), `session.app_document` (Read) and `session.app_call` (Drive), all non-mutating, feature `mcp_apps.v1`. Deck routes are owner-only. The station resolves the app from its own row, reaches only that server, refuses tools without the `app` visibility, allows only `ui://` reads, answers `confirm` for every tool that is not read-only until the board sends `confirmed: true`, rate-limits (2 concurrent, 30 per minute per session), and never starts, resumes or wakes a session.
- Rejected: a durable session command; board-only checks; members and coordinators in phase 1.

## DL-454 — Phase 1 host bridge and consent

- Choice: `ui/initialize`, tool-input and tool-result replay, host context with literal theme colors, `size-changed`, `host-context-changed`, `request-teardown`, `resource-teardown`, `ping`, `tools/call` and `resources/read` through the deck, `ui/message` (owner dialog, then the normal composer send), `ui/open-link` (owner dialog, http and https only), inline display only. Not supported: full screen, model context, downloads, sampling.
- Rejected: t3code's full host side in one step.

## DL-455 — One opaque frame with a per-app CSP

- Choice: `sandbox allow-scripts` in both the frame attribute and the CSP header, never `allow-same-origin`; the CSP is built from the sanitized `connectDomains` and `resourceDomains` only, with no `'self'`, no frames, no forms, no `base`; the board's `frame-src 'self'` and a second-`load` guard keep the bridge on the served document. The deck prefixes a cookie facade and a color-scheme bootstrap.
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
