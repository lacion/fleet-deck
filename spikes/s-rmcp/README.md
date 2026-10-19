# S-RMCP: can `rmcp` 3.5.1 serve both MCP eras behind the station ingress? (DL-251, MCP P1, decision A1)

- **Date:** 2026-10-19
- **Versions:** `rmcp` and `rmcp-macros` 3.5.1 (pinned `=3.5.1`), `hyper` 1.12.0, `tokio` 1.53.3, rustc 1.96.0, edition 2024. No provider CLI was started.
- **Result:** **PASS.** T1 to T10 pass on the first run, none is ignored. `rmcp` serves the legacy era (`2025-11-25`) and the modern era (`2026-07-28`) behind our own listener checks and bearer authentication, with the binding in the request extensions.
- **Advisor answer:** `~/fleetdeck-briefs/mcp-p1-advisor-result.md` (keep the station listener, authentication and limits as the outer boundary; `rmcp` owns the protocol).

## 1. Question

Can the station keep its own listener, pre-auth checks, `Bearer` authentication and per-binding limits (ADR-006 §5, `crates/fleetdeck-station/src/coordinator/ingress.rs`), and let `rmcp`'s `StreamableHttpService` answer the protocol for both eras? Does `tools/list` depend on the binding? Does the same outer service work in process (the bridge)?

## 2. Method

- A standalone crate with its own `[workspace]` and `Cargo.lock`. The root workspace does not include it.
- `src/outer.rs`: a hyper 1 HTTP/1 listener and `Outer::handle`. The check order is the order of `ingress.rs` `handle`: path `/mcp` (404), POST (405), `Host` `127.0.0.1:<port>` or `localhost:<port>` and no `Origin` (403), `Content-Type: application/json` (415), `Authorization: Bearer <token>` in a `HashMap<String, Binding>` (401, body `{"error":"unauthorized"}`, `WWW-Authenticate: Bearer`), the per-binding `Semaphore(1)` with `try_acquire_owned` (429). Then the `Binding` goes into the request extensions and the request goes to `StreamableHttpService`. The response body is wrapped in a `PermitBody` that owns the permit.
- `src/lib.rs`: `Binding`, `SpikeServer` with the tools `whoami`, `list_things`, `poke` and a test-only `slow`, and `bridge_call` lives in `outer.rs` (the in-process form of the bridge).
- `tests/spike.rs`: T1 to T10 as `#[tokio::test]` against a real `127.0.0.1:0` listener (T8 and T9 are in process, see the table), and two print-only tests (`probe_edge_cases`, `the_permit_on_tcp_with_an_unread_response`) that record behaviour without asserting it.
- The test client is `hyper::client::conn::http1` (the dev-dependency feature `client`), not `reqwest`.

The `rmcp` config is the one of the owner's guide: `StreamableHttpServerConfig::default().with_legacy_session_mode(false).with_json_response(true)` and `StreamableHttpService::new(factory, LocalSessionManager::default().into(), config)`.

## 3. Reproduce

```sh
umask 022 && sh tools/heavy.sh cargo test --manifest-path spikes/s-rmcp/Cargo.toml -- --nocapture --test-threads=1
umask 022 && sh tools/heavy.sh cargo clippy --manifest-path spikes/s-rmcp/Cargo.toml --all-targets -- -D warnings
```

`--nocapture` prints the wire bytes of this report.

## 4. Results

| Test | Result | Notes |
| --- | --- | --- |
| T1 `legacy_initialize_then_tools_list_and_call` | PASS | `initialize` (2025-11-25) answers `protocolVersion` 2025-11-25, `serverInfo` `fleetdeck` `0.0.0-spike`. No `Mcp-Session-Id` header. `notifications/initialized` is 202. `tools/list` lists `list_things`, `whoami`. `tools/call whoami` returns `session-a`. |
| T2 `modern_discover_then_stateless_call` | PASS | `server/discover` without `initialize`, then `tools/call list_things` with the modern `_meta` and headers: `resultType: "complete"`, `structuredContent` and a text copy of it. |
| T3 `unsupported_version_gets_32022_with_supported` | PASS | HTTP 400, `-32022`, `data.supported` is the list of 5 versions. `initialize` with `2099-01-01` is recorded below. |
| T4 `header_mismatch_gets_32020` | PASS | HTTP 400, `-32020`, message names both values. |
| T5 `outer_checks_run_before_rmcp` | PASS | 401 with the exact body and `WWW-Authenticate: Bearer` (no bearer and unknown bearer), 403 for `Origin`, 403 for `Host: evil:1`, 415 for `text/plain`. A bad `Host` without a bearer is 403, not 401 (order). |
| T6 `the_catalog_depends_on_the_binding` | PASS | Token A (`[]`): `list_things`, `whoami`. Token B (`["mail"]`): `list_things`, `poke`, `whoami`. The order is sorted by tool name (`ToolRouter::list_all`), the same for every call, in both eras. |
| T7 `annotations_and_output_schema_on_the_wire` | PASS | `list_things`: `annotations.readOnlyHint` true, `openWorldHint` false, `outputSchema` present. `poke`: `destructiveHint` false, `idempotentHint` true. Serialized `tools/list`: see 4.4. |
| T8 `in_process_bridge_call` | PASS | **In process, no TCP** (the brief names this test as in process). Legacy `tools/list` line gives one JSON line with 3 tools (token B). Modern `tools/call` line gives `resultType: "complete"` and `structuredContent`. A notification line gives `None`. An unknown token gives the line `{"error":"unauthorized"}` (not a JSON-RPC error: the bridge must map it). |
| T9 `the_permit_lives_until_the_body_ends` | PASS | **In process, no TCP** (reason: see finding 6). First response unread: second request with the same token is 429. Another token is 200. After the first body is consumed, a request is 200. |
| T10 `cancellation_drops_the_handler` | PASS | Closing the client connection in the middle of `slow` makes `RequestContext::ct` fire (0.5 ms after the abort in the run). See API map. |
| (print) `the_permit_on_tcp_with_an_unread_response` | recorded | On TCP, with the first response unread in the client socket, the second request is **200**, not 429. See finding 6. |
| (print) `probe_edge_cases` | recorded | See 4.5. |

### 4.1 T2 wire bytes (modern era)

```
request  server/discover: {"id":1,"jsonrpc":"2.0","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}
  headers: MCP-Protocol-Version: 2026-07-28, Mcp-Method: server/discover
response 200: {"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2024-11-05","2025-03-26","2025-06-18","2025-11-25","2026-07-28"],"capabilities":{"tools":{}},"instructions":"Fleet Deck tools for one session. Use whoami to see your session.","ttlMs":0,"cacheScope":"private","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fleetdeck","version":"0.0.0-spike"}}}}

request  tools/call: {"id":2,"jsonrpc":"2.0","method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/protocolVersion":"2026-07-28"},"arguments":{},"name":"list_things"}}
  headers: MCP-Protocol-Version: 2026-07-28, Mcp-Method: tools/call, Mcp-Name: list_things
response 200: {"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","content":[{"type":"text","text":"{\"things\":[\"alpha\",\"beta\"]}"}],"structuredContent":{"things":["alpha","beta"]},"isError":false}}
```

`next_cursor` is absent when `None` (`skip_serializing_if`).

### 4.2 T3 wire bytes

```
request  tools/list, header and _meta 2099-01-01
response 400: {"jsonrpc":"2.0","id":1,"error":{"code":-32022,"message":"Unsupported protocol version","data":{"requested":"2099-01-01","supported":["2024-11-05","2025-03-26","2025-06-18","2025-11-25","2026-07-28"]}}}

legacy initialize with protocolVersion 2099-01-01 (with and without the MCP-Protocol-Version header):
response 200: {"jsonrpc":"2.0","id":2,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fleetdeck","version":"0.0.0-spike"},"instructions":"Fleet Deck tools for one session. Use whoami to see your session."}}
```

### 4.3 T4 wire bytes

```
request  tools/call whoami, header Mcp-Name: poke
response 400: {"jsonrpc":"2.0","id":1,"error":{"code":-32020,"message":"Mcp-Name header `poke` does not match body value `whoami`"}}
```

### 4.4 Catalog size (T7, serialized `tools/list` response body)

| Case | Bytes |
| --- | --- |
| Modern era, token A (2 tools) | 663 |
| Modern era, token B (3 tools) | 825 |
| Legacy era, token B (3 tools) | 768 |

The modern result has `resultType`, `ttlMs: 0`, `cacheScope: "private"`. The legacy result has none of the three. `list_things` carries an `outputSchema` of about 330 bytes (it has `$schema`). `next_cursor` appears in the `outputSchema` as `{"type":["string","null"]}` and is not `required`.

### 4.5 Edge cases (print-only probe)

| Request | Answer |
| --- | --- |
| Modern body (`_meta` version), no MCP headers | 400 `-32020` "request _meta protocolVersion requires MCP-Protocol-Version header" |
| Modern, version header but no `Mcp-Method` | 400 `-32020` "missing required Mcp-Method header" |
| Modern header, no `_meta` | 400 `-32602` "request _meta is missing or has malformed required fields: ...protocolVersion, ...clientCapabilities" |
| Modern, `_meta` without `clientCapabilities` | 400 `-32602` naming `io.modelcontextprotocol/clientCapabilities` |
| Header 2025-11-25, `_meta` 2026-07-28 | 400 `-32020` "MCP-Protocol-Version header (2025-11-25) does not match request _meta protocolVersion (2026-07-28)" |
| Modern, unknown method | **404** `-32601` |
| `tools/call poke` by token A (no `mail` class) | **200, the tool runs** (`poked`). `tools/list` hides it, `tools/call` does not check. |
| `tools/call` unknown tool | 200 with JSON-RPC error `-32602` "tool not found" |
| POST without `Accept: application/json, text/event-stream` | 406 (plain text) |
| JSON array (batch) | 415 (plain text, "fail to deserialize request body") |
| Body `{` (not JSON) | 415 (plain text, not `-32700`) |
| Legacy `ping` without `initialize` | 200 `{}` (stateless: no handshake needed) |
| `legacy_session_mode = true` (the `rmcp` default): `initialize` | 200 `text/event-stream` with an `Mcp-Session-Id` header and a priming `retry: 3000` event |
| `legacy_session_mode = true`: `tools/list` without a session id | 422 "Unexpected message, expect initialize request" |
| `legacy_session_mode = true` with `NeverSessionManager` (tried once, not kept) | `initialize` is 500 "Session management is not supported" |

## 5. Findings (answers a to g)

### a. `rmcp` types, traits and config calls used for each test (paths in `~/.cargo/registry/src/*/rmcp-3.5.1/src/`)

| Item | Where |
| --- | --- |
| `StreamableHttpServerConfig` (+ `with_legacy_session_mode`, `with_json_response`, `with_allowed_hosts`, `enforce_origin_validation`, `with_max_request_body_bytes`, `with_stateless_protocol_metadata_required`, `with_cancellation_token`) | `transport/streamable_http_server/tower.rs:78` and following (the builders at 214 to 284) |
| `StreamableHttpService::new(factory, Arc<M>, config)` and `handle(Request<B>) -> Response<BoxBody<Bytes, Infallible>>` (also `tower_service::Service`) | `tower.rs:1332`, `1418`, `1824` |
| POST routing (`handle_post`, stateless branch, `json_response` branch, `CancelOnDisconnect`) | `tower.rs:2005`, `2290` to `2360` (the drop guard at `2322`), `2445` |
| Era routing (`is_legacy_request`, `uses_legacy_lifecycle`) | `tower.rs:390`, `service.rs:204` |
| Header checks (`validate_standard_headers`, `validate_header_matches_init_body`, `validate_request_protocol_version_meta`) and `-32020` | `tower.rs:849`, `499`, `530`, `827`; the `Mcp-Method`/`Mcp-Name` rules in `transport/common/mcp_headers.rs:261` (`Mcp-Name` comes from `params.name` for `tools/call` and `prompts/get`, `params.uri` for resources, `params.taskId` for tasks) |
| HTTP status of an error (`jsonrpc_http_status`: `-32022`, `-32020` and malformed `_meta` give 400, `-32601` gives 404, all others 200) | `tower.rs:648` |
| `LocalSessionManager`, `NeverSessionManager` | `transport/streamable_http_server/session/local.rs`, `session/never.rs` |
| `ServerHandler` (`get_info`, `list_tools`, `call_tool`, `get_tool`, `discover`, `supported_protocol_versions`, `negotiate_initialize`) | `handler/server.rs:642` (the defaults in the macro at 307 to 640) |
| `ServerConfig` (the name of the `initialize` result; `ServerInfo` is a deprecated alias) | `model.rs:1178`, `1200` |
| `ToolRouter` (`list_all` sorts by name, `get`, `call`), `ToolCallContext::new` | `handler/server/router/tool.rs:581`, `596`, `560`; `handler/server/tool.rs:36` |
| `#[tool_router]`, `#[tool(annotations(...))]`, `#[tool_handler]` | the crate `rmcp-macros` 3.5.1, `src/lib.rs`; `tool_handler.rs` writes `call_tool`, `list_tools`, `get_tool`, `get_info` only if you did not write them |
| `Json<T>` (structured output; `CallToolResult::structured` also writes the text copy) | `handler/server/wrapper/json.rs`; `model.rs:4155` |
| `CallToolResponse` (the return type of `call_tool`: `Complete`, `InputRequired`, `Task`) | `handler/server.rs` (`call_tool`), `handler/server/tool.rs:91` |
| `ListToolsResult::with_all_items`, `CacheScope::{Public, Private}`, `ResultType::COMPLETE` | `model.rs:1729` (macro `paginated_result`), `1687` |
| `RequestContext<RoleServer>` (`ct`, `extensions`, `meta`, `peer`, `protocol_version()`) | `service.rs:1208`, `1234` |
| `ProtocolVersion::KNOWN_VERSIONS` (5 versions), `negotiate_protocol_version` | `model.rs:204`, `service/server.rs:486` |

Tests: T1 uses `initialize`, `ServerConfig`, `tools/list`, `call_tool`. T2 uses `discover` (the default `ServerHandler::discover`), the modern `_meta`, `Json<T>`. T3 and T4 need no handler code (`is_legacy_request`, `validate_*`). T5 and T9 are outer code only (`Outer::handle`, `PermitBody`). T6 and T7 use `ServerHandler::list_tools`, `ToolRouter::list_all`, the `#[tool(annotations(...))]` attribute and `Json<T>`. T8 uses `Outer::handle` without TCP. T10 uses `RequestContext::ct`.

### b. Stateless legacy mode and a session manager; what `with_legacy_session_mode(false)` does to `initialize`

- The session manager is a type parameter of `StreamableHttpService` and is required by `new`, but with `legacy_session_mode = false` the POST path never creates, looks up or stores a session. It is read only for `event_store()` (GET replay). `LocalSessionManager::default()` works as in the guide. `NeverSessionManager` also works in this mode (tried once with the same tests, all passed); it fails with 500 as soon as legacy session mode is on.
- With `false`, `initialize` is answered **statelessly** (`tower.rs` `handle_post` stateless branch): the request goes through `OneshotTransport` and `NegotiatingStatelessHttpService`, which negotiates the version (T1: the response has `protocolVersion` 2025-11-25) and returns the `InitializeResult` as plain `application/json`. **No `Mcp-Session-Id`** is issued, no priming event. Later legacy requests (`tools/list`, `tools/call`, `ping`) work without `initialize` and without any session id: the server has no memory of the handshake. The negotiated version of a later request comes from its `MCP-Protocol-Version` header (absent header: `2025-03-26`).
- With `true` (the default of `rmcp`): `initialize` creates a session and answers `text/event-stream` with `Mcp-Session-Id`; every later legacy request must carry it (422 without). That mode needs a real session manager and keeps state per client; modern requests are still served statelessly in the same service.
- `stateless_protocol_metadata_required` stays `false` (the default): a legacy request without `MCP-Protocol-Version` is accepted as `2025-03-26`.

### c. How a handler and `list_tools` read the request extensions

`rmcp` stores the `http::request::Parts` of the POST in `RequestContext::extensions`. The outer service inserts `Binding` into `Parts::extensions` before calling `rmcp` (`Request::from_parts(parts, body)` with `parts.extensions.insert(binding)`). Reading, in both `list_tools` and every tool (`binding_of` in `src/lib.rs`):

```rust
ctx.extensions.get::<http::request::Parts>()?.extensions.get::<Binding>().cloned()
```

`list_tools(&self, request, context: RequestContext<RoleServer>)` gets the same `RequestContext` as `call_tool`, so the catalog can depend on the binding (T6). A tool can also take `rmcp::handler::server::tool::Extension<Parts>` as a parameter (`handler/server/common.rs:180`, a `Clone` extension type; it works on `Parts`, not on `Binding` directly). `Binding` must be `Clone + Send + Sync + 'static`. The binding comes from the outer service only, never from `clientInfo` or an MCP session.

### d. What `rmcp` answers for a legacy `initialize` with an unknown version

`initialize` with `protocolVersion: "2099-01-01"` answers 200 and `protocolVersion: "2025-11-25"`: the newest version that still has an `initialize` handshake and that the server supports (`negotiate_protocol_version`, `service/server.rs:486`). It does **not** select `2026-07-28`. This is the same with and without a matching `MCP-Protocol-Version` header. A modern request (per-request `_meta`) with an unknown version is not a negotiation: it is HTTP 400 `-32022` with `supported` (T3).

### e. Crates in the spike `Cargo.lock` that the root `Cargo.lock` does not have

The spike lock has 111 packages (without `s-rmcp`), the root lock has 284. **39 crate names are new** (versions and licenses from `cargo metadata`; all licenses are in the `deny.toml` allow list; `Unicode-3.0` and `Apache-2.0` are allowed too):

| Crate | Version | License |
| --- | --- | --- |
| android_system_properties | 0.1.6 | MIT OR Apache-2.0 |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 |
| darling | 0.24.1 | MIT |
| darling_core | 0.24.1 | MIT |
| darling_macro | 0.24.1 | MIT |
| dyn-clone | 1.0.20 | MIT OR Apache-2.0 |
| futures | 0.3.34 | MIT OR Apache-2.0 |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 |
| futures-executor | 0.3.34 | MIT OR Apache-2.0 |
| futures-io | 0.3.34 | MIT OR Apache-2.0 |
| futures-macro | 0.3.34 | MIT OR Apache-2.0 |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 |
| ident_case | 1.0.1 | MIT/Apache-2.0 |
| lock_api | 0.4.14 | MIT OR Apache-2.0 |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 |
| pastey | 0.2.3 | MIT OR Apache-2.0 |
| redox_syscall | 0.5.18 | MIT |
| ref-cast | 1.0.27 | MIT OR Apache-2.0 |
| ref-cast-impl | 1.0.27 | MIT OR Apache-2.0 |
| rmcp | 3.5.1 | Apache-2.0 |
| rmcp-macros | 3.5.1 | Apache-2.0 |
| schemars | 1.2.3 | MIT |
| schemars_derive | 1.2.3 | MIT |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 |
| serde_derive_internals | 0.30.0 | MIT OR Apache-2.0 |
| sse-stream | 0.2.6 | MIT OR Apache-2.0 |
| tokio-stream | 0.1.19 | MIT |
| tokio-util | 0.7.20 | MIT |
| try-lock | 0.2.5 | MIT |
| uuid | 1.28.0 | Apache-2.0 OR MIT |
| want | 0.3.2 | MIT |
| windows-core | 0.62.2 | MIT OR Apache-2.0 |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |

Notes:

- `android_system_properties`, `core-foundation-sys`, `iana-time-zone*`, `redox_syscall` and the five `windows-*` crates are target-specific (chrono's clock): they are in the lock but are not compiled on Linux. On Linux the new crates that are compiled: about 29.
- Eight crates are at another version than the root lock only because the spike resolved newer releases (no root lock was used): `cc` 1.9.0 (root 1.7.0), `equivalent` 1.0.3 (1.0.2), `find-msvc-tools` 0.1.15 (0.1.14), `libc` 0.2.191 (0.2.190), `serde_json` 1.0.154 (1.0.152), `syn` 3.0.9 (root has 2.0.119 and 3.0.7), `tokio` 1.53.3 (1.53.2), `unicode-ident` 1.0.27 (1.0.26). In the root workspace `cargo` would use the root versions.
- `rmcp` depends unconditionally on `tokio`, `futures`, `tokio-util`, `chrono`, `serde`, `serde_json`, `thiserror`, `uuid` and `base64` with `default-features = false` (the advisor's note is right). `tower` was not needed (`rmcp` uses `tower-service`, already in the root lock). `http` and `bytes` are used directly by the spike (both are already in the root lock).
- `pastey` (a macro helper, fork of `paste`) is a new proc-macro dependency; `darling` and `ident_case` come from `rmcp-macros`.
- Features: exactly the four of the brief on `rmcp`. The build needed no other feature. The tests added the feature `client` to `hyper` (dev-dependency) for the test client.

### f. Clean build time

`cargo build --tests --manifest-path spikes/s-rmcp/Cargo.toml` from an empty `target`, through `heavy.sh`, with the compiler cache off (`CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER=`; the machine has `kache` on by default): **12.43 s** (cargo's own "Finished" time, without the queue wait), 32 logical CPUs, dev profile. With `kache` on, the first full `cargo test` after the first compile took 11.6 s, so the cache did not change the result much.

### g. Surprises and gaps versus the guide summary

1. **`#[tool_handler]` writes `list_tools` with `cacheScope: Public`** (`rmcp-macros` `tool_handler.rs`). A per-binding catalog must be `Private`. The spike writes its own `list_tools` (`CacheScope::Private`, `ttlMs: 0`). The default `discover` result is also `private` in 3.5.1 (T2 bytes).
2. **`tools/list` is a filter only.** `ToolRouter::call` runs any registered tool: token A could call `poke` (probe, 200). P1 must check the class in `call_tool` too. The spike does not (the brief asks only for the list to depend on the binding).
3. **`ServerInfo` is now `ServerConfig`** (`ServerInfo` is a deprecated alias). `call_tool` returns `CallToolResponse` (an enum), not `CallToolResult`. `ListToolsResult` is built with `with_all_items(...)` or `Default`.
4. **`rmcp` requires `Accept: application/json, text/event-stream` on every POST** (406 otherwise) and checks `Content-Type` itself too. The bridge (`bridge_call`) must send it. Our outer 415 check runs first.
5. **`rmcp` own error bodies are plain text for malformed input** (a batch and non-JSON give 415 with a text body, not a JSON-RPC `-32700`/`-32600` error as `ingress.rs` gives today: 400 with `-32700` for a parse error, 400 for a batch). A client that today gets `-32700` would get a different answer. Decide in P1 whether the outer service keeps its own parse and batch checks before `rmcp`.
6. **The per-binding permit and the response body.** The `PermitBody` wrapper keeps the permit until the body is dropped or ends (T9, in process). On a real socket with the JSON mode the body is one frame, which hyper writes into the kernel buffer at once and then drops: the permit is released even if the client has not read the response (the print test: second request 200 with the first response unread). So with `with_json_response(true)` the permit covers the handler run and the write, not a slow reader. A long SSE response (P2) keeps the body, and so the permit, alive for the whole stream: the wrapper is needed then.
7. **Cancellation is cooperative.** On a client disconnect hyper drops the service future of `Outer::handle`; `rmcp` holds a drop guard on a per-request `CancellationToken` (`tower.rs` json-response branch, `CancelOnDisconnect` for SSE) and cancels `RequestContext::ct`. The handler runs in its own task (`serve_directly_with_ct`), so it is **not aborted**: a handler that ignores `ctx.ct` runs on. T10 observed `ct` within 0.5 ms; the drop-signal in the test fires when the handler returns, so it does not prove an abort. For durable commands (the outbox `claim` and `release`) the handler must treat `ctx.ct` explicitly. (Also in the advisor's finding 4.)
8. **Body limit.** `rmcp` has its own `max_request_body_bytes` (default 4 MiB); the station uses 256 KiB. The spike does not set it: P1 sets `with_max_request_body_bytes(256 * 1024)` and keeps the outer size and time limits (the spike's outer service has no body-time limit and no unauthenticated permit).
9. **Host and Origin are checked twice.** `rmcp` rejects hosts other than `localhost`, `127.0.0.1` and `::1` (any port) by default and, with `enforce_origin_validation()` and an empty list, any `Origin`. The spike did not enable these (the outer checks are stricter: exact port). They can be switched on as a second layer.
10. **Modern era strictness is good.** With `with_legacy_session_mode(false)` and a modern request: missing `MCP-Protocol-Version` (with a modern `_meta`) is `-32020`, missing `Mcp-Method` is `-32020`, a header/`_meta` mismatch is `-32020`, a missing `_meta` key is `-32602` (HTTP 400), an unknown method is HTTP 404 `-32601`, an unsupported version is HTTP 400 `-32022` with `requested` and `supported` (4.5). These match the advisor's list (HTTP 400, 400, 404).
11. **Legacy responses carry no `resultType`, `ttlMs` or `cacheScope`;** modern ones do (`ListToolsResult` and `CallToolResult`): `rmcp` clears them for a legacy peer (checked in the legacy `tools/list`, `tools/call` bytes).
12. **`Json<T>` output schema.** `Option<String>` (`next_cursor`) appears in the `outputSchema` as `["string","null"]` and not in `required`, while the value is omitted when `None`: valid. The schema has a `$schema` key (2020-12) and is generated by `schemars` 1.x, 330 bytes for this small type. The size matters for the catalog budget (finding 5 of the advisor).
13. **`rmcp` also has a Unix socket transport** (`transport/common/unix_socket.rs`, `tests/test_unix_socket_transport.rs`, `UnixSocketHttpClient`). Not used or tested here; relevant for the P2 bridge wire.
14. **A bad token and an `Accept`/body error are outside `rmcp`.** `bridge_call` returns the raw 401 body `{"error":"unauthorized"}` for an unknown token: the station bridge maps these to its own `ERR` codes today and must keep doing it.

## 6. API map (names that differ from the guide)

| Guide | `rmcp` 3.5.1 |
| --- | --- |
| `ServerInfo` | `ServerConfig` (alias of `InitializeResult`; `get_info() -> ServerConfig`) |
| `CallToolResult` returned by `call_tool` | `CallToolResponse` |
| `ctx.extensions` with `http::request::Parts` | as in the guide: `ctx.extensions.get::<http::request::Parts>()` |
| `StreamableHttpService::new(factory, LocalSessionManager::default().into(), config)` | as in the guide (`Arc<M>`) |
| `RequestContext<RoleServer>` cancellation | `ctx.ct: CancellationToken` (tokio-util) |
| `#[tool_router]` / `#[tool(annotations(...))]` / `Json<T>` | as in the guide; `annotations(read_only_hint = true, open_world_hint = false, destructive_hint = false, idempotent_hint = true)` all exist |

## 7. Files

- `src/lib.rs`: `Binding`, `ThingsPage`, `Probe`, `SpikeServer`, `binding_of`.
- `src/outer.rs`: `Outer` (the checks, the permit, the `rmcp` service), `serve`, `Running`, `bridge_call`.
- `tests/spike.rs`: T1 to T10 and the two print-only tests.
- `Cargo.toml`, `Cargo.lock`: standalone (`[workspace]` table); `.gitignore`: `/target/`.
