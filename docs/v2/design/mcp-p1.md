# MCP P1 — both protocol eras through rmcp, tool annotations, catalog budget

- **Status:** Draft revision 1, 2026-10-19. Needs an adversary DESIGN REVIEW (it changes the authenticated MCP endpoint) before it freezes.
- **Program:** the MCP program (DL-251). Architect decision A1: keep the station listener, authentication and limits (ADR-006 §5) as the outer boundary; the official SDK `rmcp` 3.5.1 owns the protocol behaviour of both eras. Evidence: the spike `spikes/s-rmcp/README.md` (T1–T10 pass) and the advisor answer `~/fleetdeck-briefs/mcp-p1-advisor-result.md`.
- **Out of scope:** resources, prompts, completions, generated instructions (P3); `subscriptions/listen`, progress and SSE responses, bridge wire v2 (P2); output schemas for deck-produced results (P3); the default worker grant (P4).

## 1. Goals

1. The station answers MCP 2026-07-28 (stateless, `server/discover`, per-request `_meta`, `-32022`, `-32020`) and the legacy versions 2025-11-25, 2025-06-18 and 2025-03-26 on the same endpoint, on HTTP and on the bridge socket.
2. Every security property of ADR-006 §5 stays: loopback only, Host and Origin checks before authentication, one uniform 401, pre-auth limits, per-binding limits, the caller identity only from the credential, the class check on every call, `not_bound` for a pending binding, the live `allow_fleet_control` switch, the P6 result recheck.
3. Tools carry honest annotations; the two station-owned results carry an `outputSchema` and `structuredContent`; the list results carry `cacheScope: private`.
4. The full catalog fits a byte budget that a test enforces.
5. A cancelled call (the client closed the connection) stops waiting and frees its permit; a durable call stays durable.

## 2. Dependency

- `crates/fleetdeck-station/Cargo.toml`: `rmcp = { version = "=3.5.1", default-features = false, features = ["server", "transport-streamable-http-server"] }`. No `macros` (the catalog is built at run time, not with `#[tool]`). Exact pin; an upgrade is its own change with the live smoke of §9.
- `Cargo.lock` gains the crates of `spikes/s-rmcp/README.md` §5e minus the ones that only `macros`/`schemars` derive bring (the worker reports the final list). `cargo deny check` must pass; `cargo fetch` runs once with network before `make check`.
- DL-251 records the dependency decision; the P1 DL entry records the final crate list.

## 3. Architecture

```
HTTP:   hyper listener ─ ingress::handle (unchanged checks, auth, permit) ─┐
                                                                            ├─ mcp_service::serve(binding, permit, request) ─ rmcp StreamableHttpService ─ StationMcp (ServerHandler)
Bridge: unix socket ─ socket::intake (AUTH, limits) ─ bridge request build ─┘                                                              │
                                                                                                                                            └─ tools/call ─ jsonrpc::call (today's dispatch: desktop / own / forward)
```

### 3.1 New module `crates/fleetdeck-station/src/coordinator/mcp_service.rs`

- `pub(super) struct McpService` holds `StreamableHttpService<StationMcp, NeverSessionManager>` built once at `Ingress::start_with` / `BridgeSocket::start` with a factory closure that clones `StationMcp` (cheap: `Arc`s inside).
- Config, exactly: `StreamableHttpServerConfig::default().with_legacy_session_mode(false).with_json_response(true).with_max_request_body_bytes(256 * 1024)`, plus `enforce_origin_validation()` with an empty allow list (a second layer; the outer 403 stays first). The rmcp default Host check stays on (`localhost`, `127.0.0.1`, `::1`). `NeverSessionManager` (no session state at all; the spike proved it works with legacy session mode off).
- `pub(super) async fn serve(&self, binding: Binding, body: Bytes, headers: &HeaderMap) -> Reply` builds an `http::Request` (method POST, URI `/mcp`, the original headers, `Accept: application/json, text/event-stream` added when absent, the body), inserts `binding.clone()` into the request extensions, calls the service, and collects the body. `Reply` is `{ status: StatusCode, content_type: Option<HeaderValue>, body: Bytes }`.
- The per-binding permit is held by the caller until `serve` returns and the reply is written (JSON mode: the body is complete when `serve` returns). P2 adds the body wrapper for SSE.

### 3.2 `StationMcp` (new module `crates/fleetdeck-station/src/coordinator/mcp_server.rs`)

`#[derive(Clone)] pub(super) struct StationMcp { bindings: Bindings, forward: Arc<dyn Forward>, fleet: Option<Arc<dyn FleetTools>> }` implements `rmcp::ServerHandler`:

| Method | Behaviour |
| --- | --- |
| `get_info` | `ServerConfig` with capabilities `tools` only (no `listChanged` in P1), server info name `fleetdeck`, version `env!("CARGO_PKG_VERSION")`, no instructions (context-free). |
| `supported_protocol_versions` | exactly `2025-03-26`, `2025-06-18`, `2025-11-25`, `2026-07-28` (not `2024-11-05`, as today). |
| `initialize` | `context.peer.set_peer_info(request.clone())`, then `self.negotiate_initialize(&request)`, then set `instructions` to the binding's `control_skill` (the binding from the request extensions). Then record the negotiated version for the bridge (§3.4). |
| `discover` | `DiscoverResult::from_server_info(self.supported_protocol_versions().into_owned(), info)` where `info` is `get_info()` with `instructions` = the binding's `control_skill`. |
| `list_tools` | the binding's catalog (§4.1): the tools of `tools::catalog(&binding.classes)` in today's fixed order, as rmcp `Tool` values with annotations and output schemas; `ListToolsResult::with_all_items(..)` with `cacheScope` `Private` and `ttlMs` 60 000. |
| `call_tool` | §3.3. |
| everything else | the rmcp default (`ping` answers `{}`; other methods `-32601`). |

The binding is read only with `ctx.extensions.get::<http::request::Parts>()?.extensions.get::<Binding>()`. No binding in the extensions (a programming error) → `McpError::internal_error("no binding", None)`. `clientInfo`, `_meta` and any argument never change the caller.

### 3.3 `call_tool`

1. The binding (§3.2). `tools::tool(name).filter(|t| t.granted(&binding.classes))`; none → a tool error (`isError: true`) with the text of §5 item 1. This is the class check of every call: rmcp's `tools/list` filter alone does not stop a call (spike finding 5).
2. The rest is today's `jsonrpc::answer` `tools/call` branch, moved into `jsonrpc::call(binding, forward, fleet, fleet_control, name, args) -> CallOutcome` without any behaviour change: the desktop path with `parse_request`, the live switch and `forward.call_desktop` through `Rechecked`; the fleet-control switch for a fleet-control binding; `not_bound` for a binding that is not active; own tools through `fleet.call`; all others through `forward.call`.
3. `CallOutcome` → `CallToolResponse`: a desktop result converts its `McpToolResult::to_value()` with `serde_json::from_value::<CallToolResult>`; an `Ok(v)` of `publish_artifact` or `set_focus` gives `CallToolResult` with `structuredContent` = `v` and one text block `v.to_string()`; any other `Ok(v)` gives one text block `v.to_string()` (as today); an `Err(e)` gives a tool error with the text `e` (as today, with the §5 rewrites).
4. Cancellation: the work of step 2 runs in `tokio::select!` with `ctx.ct.cancelled()`. On cancel the work future is dropped (the outbox claim guard of `04d68b94` releases the row; a durable row stays `pending` and the flush sends it) and `call_tool` returns `McpError::internal_error("cancelled", None)` (no response reaches the closed connection). The permit is freed when `serve` returns.

### 3.4 Ingress and bridge

- `ingress.rs` `handle`: every check up to and including authentication and the binding permit stays as it is, in the same order (path, POST, Host, Origin, Content-Type, Content-Length, unauthenticated permit, body with time and size limit, Bearer, per-binding permit and the `starting` hold). Then, before rmcp: the body must parse as JSON (else today's 400 `-32700`) and must not be an array (else today's 400 with no body). Then `McpService::serve` and the reply is written with its status, content type and body.
- `socket.rs` `connection`: `intake` stays (AUTH line, limits, authentication, `starting`). Then the same JSON and batch checks (today's `ERR bad_request`). Then `McpService::serve` with synthesized headers: `Host: 127.0.0.1`, `Content-Type: application/json`, `Accept: application/json, text/event-stream`; when the message has `params._meta["io.modelcontextprotocol/protocolVersion"]` (a string), also `MCP-Protocol-Version` = that value, `Mcp-Method` = `method`, and `Mcp-Name` = `params.name` for `tools/call` and `prompts/get` or `params.uri` for `resources/*` when present; when it has no such `_meta` and the binding has a recorded legacy version (below), `MCP-Protocol-Version` = that version. Reply mapping: status 202 → no line; a body that parses as JSON → that line (any status); any other body → `ERR bad_request`.
- Legacy version memory for the bridge: `Bindings` stores, per credential digest, the version that the last `initialize` of that binding negotiated (set in `StationMcp::initialize`; cleared when the binding is removed). It is only a header hint for the bridge path; it gives no authority.
- The bridge client `fleetdeck mcp-bridge` (bridge.rs) does not change in P1.

### 3.5 What is removed

`jsonrpc.rs` keeps only `call` and its helpers. `PROTOCOL_VERSIONS`, the `initialize`/`ping`/`tools/list`/method-not-found code and `Answer` are removed; `answer_message`/`MessageOutcome` change to return the `Reply` of §3.1 (or the existing early outcomes `Unauthorized`, `Limited`, `Starting`, `ParseError`, `Batch`).

## 4. Catalog

### 4.1 Annotations (all 47 tools)

Read-only tools get `readOnlyHint: true` and the `openWorldHint` below; they get no `destructiveHint` or `idempotentHint`. Every other tool gets all four hints explicitly (the spec default of `destructiveHint` is `true`).

| Tool | readOnly | destructive | idempotent | openWorld |
| --- | --- | --- | --- | --- |
| `list_sessions`, `wait_task`, `wait_tasks`, `command_status`, `read_session`, `session_diff`, `list_needs_you`, `list_projects`, `list_artifacts`, `read_artifact`, `read_file`, `search_files`, `project_branches`, `desktop_info`, `desktop_action_status`, `capture_status`, `read_capture_image`, `window_list` | true | — | — | false |
| `project_items` | true | — | — | true |
| `publish_artifact`, `set_focus`, `resume_session`, `rename_session`, `set_mode`, `move_to_project`, `wall_show`, `pointer_move` | false | false | true | false |
| `send_mail`, `assign`, `assign_auto`, `complete_task`, `ask_coordinator`, `spawn`, `start_in_project`, `notify`, `screenshot`, `record_monitor` | false | false | false | false |
| `project_fetch` | false | false | true | true |
| `preview_open` | false | false | false | true |
| `interrupt`, `stop`, `window_close` | false | true | true | false |
| `answer_request`, `move_session` | false | true | false | false |
| `land_session` | false | true | false | true |
| `key_press`, `type_text`, `pointer_click` | false | true | false | true |

The values are a `const` table next to `TOOLS` (`tools.rs`), `FLEET_TOOLS` and `DESKTOP_TOOLS`; a test checks that every tool of the three tables has exactly one annotation row.

### 4.2 Output schemas (P1)

Only the two station-owned results:
- `publish_artifact`: `{"type":"object","required":["artifact_id","version","title","created"],"properties":{"artifact_id":{"type":"string"},"version":{"type":"integer","minimum":1},"title":{"type":"string"},"created":{"type":"boolean"}}}`.
- `set_focus`: `{"type":"object","required":["focus"],"properties":{"focus":{"type":"string","maxLength":120}}}`.
A test calls both tools and validates `structuredContent` against the schema shape (required keys and types).

### 4.3 `$defs` per tool

Every desktop and input tool schema carries only the `$defs` that it references, transitively (for example `wall_show` needs `content` → `wall_content` → `session_ref`, `artifact_ref` → `id`). The existing `referenced()` of `desktop_tools_input.rs` becomes transitive and is used for all P6, P7 and P7b schemas. No shared `$defs` outside a tool schema. The schemas stay byte-for-byte the normative bundles otherwise (the existing tests that compare them adapt to the pruned `$defs` only).

### 4.4 Budget test

A test serializes the modern-era `tools/list` result (through `StationMcp::list_tools`) for a binding with every class (`read`, `mail`, `drive`, `spawn`, `inspect`, `manage`, `answer`, `desktop`, `capture`, `input`, `recording`, `input_enabled`, `click_enabled`, `input_any`, `own`) and asserts `bytes <= 26_624` (26 KiB), and prints the measured size. It also asserts that two calls give the same bytes (deterministic order). If the measured size after §4.3 is above the budget, the worker stops with STATUS: QUESTION and the number.

## 5. Error texts (station-made only)

1. Not granted: `forbidden: your grant has no tool \`<name>\`. Call tools/list to see the tools you have.`
2. Pending binding: `not_bound: this session is still starting. Try again in a few seconds.`
3. Fleet control off: `not_granted: fleet control is off on this station. Ask the owner to turn on "Allow fleet control" in the station settings.`
Deck-made texts do not change in P1.

## 6. Tests

Existing tests in `ingress_tests.rs`, `socket_tests.rs`, `bridge_tests.rs`, `fleet_control_tests.rs`, `desktop_*_tests.rs` and `jsonrpc` keep their intent. A wire assertion may change only to the standard rmcp answer for the same request (for example the HTTP status of `-32601`, or the shape of `initialize`); the worker lists every changed assertion with the old and the new value in its report. New tests (names are binding):

1. `a_modern_discover_serves_the_binding_control_skill_and_the_four_versions`
2. `a_modern_request_with_an_unknown_version_gets_32022_and_a_legacy_initialize_gets_2025_11_25`
3. `a_header_that_does_not_match_the_body_gets_32020`
4. `a_tool_outside_the_grant_is_refused_on_call_even_when_the_client_skips_tools_list` (both eras)
5. `a_pending_binding_lists_its_tools_and_every_call_is_not_bound` (both eras)
6. `the_catalog_has_one_annotation_row_per_tool_and_private_cache_hints` (§4.1, modern `cacheScope`/`ttlMs`; legacy result has neither)
7. `publish_artifact_and_set_focus_return_structured_content_that_matches_the_output_schema`
8. `the_full_catalog_stays_within_the_budget_and_is_deterministic` (§4.4)
9. `every_desktop_schema_carries_only_the_defs_that_it_references`
10. `a_cancelled_wait_frees_the_binding_permit_and_a_durable_call_stays_pending` (close the client connection during `wait_task` and during a `send_mail` whose deck answer is held: the next request of the binding is not 429, and the `send_mail` row is still `pending` and is sent by the next flush)
11. `the_bridge_serves_both_eras_and_remembers_the_legacy_version` (socket path: legacy `initialize` 2025-06-18, then `tools/list` without `_meta` is answered as 2025-06-18; a modern `tools/call` line gets `resultType: "complete"`)
12. `the_outer_checks_still_run_before_rmcp` (the ADR-006 §8.1 tests 1–3, 5b, 9, 9a stay green unchanged; this test adds: an `Origin` header is 403 before rmcp, and a request without `Accept` is served).

## 7. Files

- New: `coordinator/mcp_service.rs`, `coordinator/mcp_server.rs`, `coordinator/annotations.rs` (the §4.1 table), `coordinator/mcp_p1_tests.rs` (new tests; split further if a file reaches 800 lines).
- Changed: `coordinator/mod.rs`, `ingress.rs`, `socket.rs`, `jsonrpc.rs`, `tools.rs`, `fleet_tools.rs`, `git_tools.rs`, `desktop_tools.rs`, `desktop_tools_input.rs`, `bindings.rs` (legacy version memory), `crates/fleetdeck-station/Cargo.toml`, `Cargo.lock`, the existing test files of §6.

## 8. Risks

- rmcp owns parsing after our checks; a bug there is a protocol bug, not an authorization bug: the identity and the class check stay in our code (§3.2, §3.3).
- rmcp's `json_response` writes one frame; the permit covers the handler and the write (spike finding 6). SSE in P2 needs the body wrapper.
- An rmcp upgrade can change wire details; the exact pin and the live smoke of §9 gate it.

## 9. Live verification (architect, after the build)

- `FD_LIVE_SMOKE=1 cargo test -p fleetdeck-providers --test live_smoke -- --ignored --nocapture claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff`
- `crates/fleetdeck-cli/tests/live_bridge.rs` (its opt-in run).
- One manual session per provider on a test station: `tools/list`, one `set_focus`, one `publish_artifact`; record which era each provider used (`server/discover` or `initialize`) in the P1 DL entry.
