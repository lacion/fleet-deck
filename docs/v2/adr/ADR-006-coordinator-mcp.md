# ADR-006 — Coordinator MCP: ingress, credentials, grants and authorization

- **Status:** Proposed (M3 round 2, 2026-10-12). Not wired. The implementation starts only after the architect records an adversary DESIGN REVIEW PASS for this ADR in the "Design approvals" section of the gate-state file (M3 goal, GATE STATUS). DL-073 records an owner pre-approval; DL-074 records why the executor still waits for the review.
- **Date:** 2026-10-12
- **Deciders:** architect (goal loop, M3), owner (pre-approval, DL-073), adversary DESIGN REVIEW (pending)
- **Implements:** PLAN §7.1 "Coordinator API contract" (L14), §9 row "Coordinator agent overreach", §10 deck tables `coordinator_receipts` and `tokens`, Phase 6 "Required ADR-006 decisions"; ADR-004 §9.3 (G3 envelopes), §9.5 (G5 revocation), §9.6 (G6/G7 method table)
- **Evidence:** none yet. §8 lists the executable negative tests that come before the wiring.
- **Verified against:** Claude Code `--mcp-config` HTTP servers with `headers` and `${VAR}` expansion (Claude Code MCP documentation); Codex `mcp_servers.<name>.url` and `bearer_token_env_var` for Streamable HTTP (Codex configuration reference; openai/codex commit a43ae86, issues 19275 and 30125). Neither provider behavior is verified on this machine yet: §5.4 makes the live verification a precondition of the capability.

## 1. Context

A coordinator agent is a driven Claude or Codex session that uses tools to drive the fleet: it reads fleet state, sends mail, drives other sessions, and spawns sessions. v0 had a `/fleet` slash command and a fleet-doctrine skill. PLAN §16 replaces them with a coordinator API as an MCP server and a deck-served control skill.

The PLAN fixes the frame (§7.1, L14):

- The station serves a loopback MCP endpoint with mandatory per-session authentication. Loopback, a PID and tool-supplied identity are not authentication.
- Authority is bound to the station identity, the app session, the provider instance and the provider-native conversation identity (Claude `session_id`, Codex `threadId`) with an explicit generation. No authority exists before the native identity is known.
- The credential handoff is provider-supported and private: never argv, process-readable configuration, URLs, events, databases or logs. The same-UID boundary is explicit. A provider without such a handoff has no coordinator capability.
- Credentials expire when idle, end with the session, and become invalid when the process is lost or the native identity is replaced. A renewal never authorizes an old queued operation.
- The read, mail, drive and spawn classes and the spawn caps stay. Tool parameters select targets, never authority. Mutations go through durable receipts and the outbox with stable ids. Queries are scoped. The deck rechecks grant, pin, expiry and session identity in the decision transaction.

M2 already has parts of the wire: the link scope `coordinate` (station → deck), the method rows `coordinator.query` (not mutating) and `coordinator.call` (mutating, deck receipts, G7), `CoordinatorCallEnvelope { command_id, target_incarnation, retry_until, call }`, and the deck decision rules in `crates/fleetdeck-deck/src/store/coordinator.rs` (incarnation, `coordinate` grant in the transaction, same-id replay, `command_id_reused`, `expired`; every new call is a stored `unsupported` rejection, DL-053). This ADR fills in the rest and changes the M2 `CoordinatorCall` shape (§6.1). No M2 build ever accepted a coordinator operation, so the change breaks no stored receipt that has an effect.

## 2. Terms

| Term | Meaning |
| --- | --- |
| Grant | The deck's authorization record for one coordinator session: classes, target scope, spawn cap, mode ceiling, expiry. Deck table `tokens` (PLAN §10). The deck is the authority. |
| Credential | 32 random bytes, encoded `fdc1_<base64url>`. Minted by the station for one binding. Only the provider child receives it. |
| Binding | The station's record that ties one credential digest to one grant, app session, provider instance and native identity, with a generation and a state. |
| Generation | A counter of the binding. It is 0 while the native identity is unknown, 1 when it becomes known, and +1 at each renewal. An operation carries the generation of its intake. |
| Native identity | Claude `session_id` or Codex `threadId` of the provider process that holds the credential. |

## 3. Trust boundary (same UID)

The station, its database, its identity key and every provider child run as one OS user. Loopback gives no process isolation. This ADR therefore states the boundary exactly:

| Caller | Result |
| --- | --- |
| A process of another OS user that connects to the loopback port | No credential: `401`. The credential is the only accepted proof. |
| A browser page (DNS rebinding, CSRF to localhost) | Rejected by the `Host` and `Origin` checks (§5.2) before authentication. |
| A driven session without a grant | It has no credential: `401`. |
| A coordinator that names another session in a tool parameter | The caller is always the binding of its credential (§5.3); the parameter selects a target, and the grant decides if that target is allowed. |
| A tool subprocess or sub-agent of the coordinator agent | It inherits the environment and acts with the coordinator's own authority. This is accepted: the subprocess is part of the agent. |
| Any other process of the same OS user | It can read `/proc/<pid>/environ` of the child (subject to the kernel's ptrace rules) and can read the station database and key. **Not protected.** A same-UID attacker already controls the station. The operations guide (Phase 7 docs) states this. |

## 4. Grants (deck)

- **Creation.** Only the owner creates a grant, on the board, as part of a session start (the spawn form toggle "Coordinator", with classes, target stations, spawn cap, mode ceiling and expiry) or a resume. The credential must be in the child's environment at process start (§5.4), so a running process cannot become a coordinator. A coordinator operation can never create a grant: a `spawn` op whose spec carries a grant is a stored rejection `forbidden` (no transitive authority).
- **Fields** (`tokens` row): `grant_id` (ULID), `station_id`, `session_id`, `classes` ⊆ {`read`, `mail`, `drive`, `spawn`} (unknown names fail closed), `stations` (the target stations; default: the coordinator's own station), `spawn_cap` (0–50, default 3), `max_mode` (the most permissive native mode that a spawned child may get; default: the provider's default mode; unknown modes fail closed), `idle_ttl_ms` (default 30 min, maximum 24 h), `expires_at` (default start + 12 h, maximum start + 7 days), `state` (`active`, `revoked`, `expired`), `revoked_reason`, `bound_generation`, `bound_native_id`, `bound_provider_instance`, `binding_id`, `last_used_at`.
- **Wire.** `SessionSpec` gains `coordinator: Option<CoordinatorSpec { grant_id, classes, idle_ttl_ms, expires_at }>`. The station needs the classes only to list tools; the deck decides every call.
- **Owner changes.** Revoke (terminal), renew (§7), and a raise or a lower of the spawn cap are owner board actions. Each is a deck mutation with a receipt and an audit row. A lowered cap below the used count blocks further spawns; it never stops sessions.
- **Lineage.** The deck stores `spawned_by_grant` for every session that a coordinator spawn starts. The drive class reaches only sessions with the caller's `grant_id` in their lineage (§6.3).

## 5. Ingress and credentials (station)

### 5.1 Endpoint

- One HTTP listener per station process on `127.0.0.1` with an ephemeral port. It never binds a non-loopback address. The port is not secret.
- Transport: MCP Streamable HTTP, `POST /mcp` with JSON-RPC 2.0; the station answers with `application/json` (no server-sent stream). `GET /mcp` and `DELETE /mcp` answer `405`. Other paths answer `404`.
- Protocol versions: the station accepts the client's `protocolVersion` if it is `2025-06-18` or `2025-03-26` and answers with the same value; another value gets a JSON-RPC error. The station may issue an `Mcp-Session-Id`, but it is never authentication.
- Limits: request body ≤ 256 KiB; ≤ 8 concurrent requests and ≤ 20 requests per second per binding (`429` above); ≤ 64 unauthenticated requests per second per listener (`429`, then a 1 s delay).
- Implementation uses the workspace's existing HTTP and JSON dependencies. A new runtime dependency needs its own DECISIONS-LOG entry.

### 5.2 Checks before authentication

1. `Host` must be `127.0.0.1:<port>` or `localhost:<port>`; otherwise `403`.
2. A request with an `Origin` header gets `403` (no browser is a valid caller).
3. `Content-Type` must be `application/json`; otherwise `415`.

### 5.3 Authentication

- The request must carry `Authorization: Bearer fdc1_<43 base64url chars>`. The station computes SHA-256 of the presented value and looks up the binding by that digest.
- Missing header, bad format, unknown digest, and a binding in state `revoked` or `expired` all get the same `401` with the same body (`{"error":"unauthorized"}`) and `WWW-Authenticate: Bearer`. No response tells which case applied. The station log records the case and, when known, the `binding_id` (never the credential, never the digest).
- A binding in state `pending` (native identity unknown) authenticates, but every tool call gets the tool error `not_bound` and `tools/list` lists no tool. `initialize` and `ping` work, so the client keeps its connection.
- The caller identity is the binding: station, `session_id`, provider instance, native identity, generation, grant. No header, tool parameter or MCP field can change it.
- A successful authenticated tool call updates `last_used_at` (idle expiry, §7).

### 5.4 Private credential handoff

- **Minting.** At the start of a session whose spec has a grant, the station creates 32 bytes from the OS RNG into a `Secret<String>`, stores only the SHA-256 digest in the binding row (state `pending`, generation 0), and puts the raw value into the provider child's environment as `FLEETDECK_COORDINATOR_TOKEN`. The station then drops the raw value. It never writes it to argv, a file, a URL, an event, a database, a log or a read model. `Debug`/`Display` of the environment builder redact every value of this variable.
- **Claude.** The adapter adds `--mcp-config` with an inline JSON object: `{"mcpServers":{"fleetdeck":{"type":"http","url":"http://127.0.0.1:<port>/mcp","headers":{"Authorization":"Bearer ${FLEETDECK_COORDINATOR_TOKEN}"}}}}`. Claude expands `${VAR}` in `headers`. argv holds the variable name only.
- **Codex.** The adapter adds the overrides `mcp_servers.fleetdeck.url="http://127.0.0.1:<port>/mcp"` and `mcp_servers.fleetdeck.bearer_token_env_var="FLEETDECK_COORDINATOR_TOKEN"` (`-c` arguments of `codex app-server`). Codex reads the token from its environment. A missing variable makes Codex send no `Authorization` header (issue 30125); the station then answers `401`, which fails closed.
- **Capability.** An adapter declares the handoff only for a provider family and a minimum provider version for which the opt-in live smoke (§8.5) recorded a passing handoff (an authenticated `initialize` with the right binding). A probed version below that minimum, an unknown version, or a provider without a declared handoff has the capability `coordinator` set to false with the reason `handoff_unverified`; the board then hides the "Coordinator" toggle and the deck refuses such a spec (`unsupported`). Until §8.5 runs, both providers have the capability disabled.
- **Station without a deck.** The station-local CLI path (DL-038) cannot create grants; a local start with a `coordinator` spec is rejected `invalid_command`.

### 5.5 Binding states

| From | Event | To | Effect |
| --- | --- | --- | --- |
| — | session start with a grant | `pending` (gen 0) | credential minted |
| `pending` | first `session.started` with a native id | `active` (gen 1) | event `coordinator.bound` |
| `active` | owner renew accepted | `active` (gen + 1) | event `coordinator.bound`; idle timer reset |
| `active` | idle time > `idle_ttl_ms` | `expired` | event `coordinator.ended { reason: idle }`; renewable until `expires_at` |
| `pending`/`active`/`expired` | `expires_at` passed | `revoked` | `coordinator.ended { reason: expired }` |
| any but `revoked` | owner revoke; session stop or end; process exit or loss; native id replaced (a `session.started` with another native id for the same process, a resume or fork that creates a new native id); deck pin removed (G5); station restart without a durable host | `revoked` (terminal) | `coordinator.ended { reason }`; queued unsent calls fail `revoked` |

A `revoked` binding never returns. With ADR-005 `ShimHost`, a verified reattachment of the same process with the same native identity keeps the binding (state and generation unchanged); any doubt about the process identity revokes it (`process_lost`).

## 6. Wire and authorization

### 6.1 Envelope changes (ADR-004 §9.3 and §9.6 rows stay; params change)

- `CoordinatorCall { caller, token_digest, op }` becomes `CoordinatorCall { caller: SessionId, binding: CoordinatorBinding, idempotency_key: Option<String>, op: CoordinatorOp }` with `CoordinatorBinding { binding_id, grant_id, generation: u32, provider_instance, native_id }`. The digest never leaves the station: the deck needs no secret-derived value, because the pinned link authenticates the station and the binding facts are checked against the deck's own projection (§6.2).
- `CoordinatorOp` (tag `op`, `deny_unknown_fields`, unknown op rejected): `spawn { station, spec, prompt? }`, `send { to, text, delivery: follow_up | steer }`, `interrupt { station, session_id }`, `stop { station, session_id }`, `assign { task, to }`, `assign_auto { task, requirements }`, `complete_task { task_id, result }`.
- `coordinator.query` params: `CoordinatorQueryEnvelope { query_id, caller, binding, deadline: i64, query: CoordinatorQuery }`. `deadline` ≤ arrival + 30 s, or ≤ arrival + 60 s for `wait_task`. Queries are not mutating and keep no receipt (G7).
- Link feature `coordinator_v1` (L16). A station enables the capability only on a link that negotiated it; a deck accepts the new shapes only on such a link. An M2 peer never negotiates it, so N−1 peers keep the M2 behavior (`unsupported`).
- Station events (no secret, decodable by older peers as `EventBody::Unknown`, L16): `coordinator.bound { binding_id, grant_id, generation, provider_instance, native_id }`, `coordinator.ended { binding_id, reason }`.

### 6.2 Deck decision order for `coordinator.call` (extends the M2 rules)

All of steps 2–5 run in one deck write transaction:

1. Another `target_incarnation` → `incarnation_mismatch`, not stored (M2 rule).
2. The station is paired and holds `coordinate` (M2 rule); else `forbidden`, not stored (G5).
3. Binding check: the grant exists for (`station`, `caller`), is `active`, is not past `expires_at`, and its `bound_generation`, `bound_native_id`, `bound_provider_instance` and `binding_id` equal the envelope's `binding`. The deck sets the `bound_*` fields only from `coordinator.bound` events of that station's stream; a `coordinator.ended` sets the grant `expired` or `revoked`. A mismatch → `binding_stale`, not stored (the station may retry the same envelope until `retry_until`; a generation that is lower than the deck's never becomes valid again). Grant `revoked`/`expired` → `forbidden`, not stored. Idle check: `now − last_used_at > idle_ttl_ms` → `forbidden`, not stored.
4. Same id + method + caller + payload hash → the stored receipt; same id with another payload → `command_id_reused` (M2 rule). Because steps 2–3 come first, a replay after a revocation gets `forbidden`, never an old acceptance: a queued or replayed operation never keeps authority.
5. `retry_until` passed → `expired`, not stored (M2 rule).
6. Authorization (§6.3) and the op's own checks; the receipt (accepted with its intent, or a stored rejection) and `last_used_at` are written in the same transaction. An accepted op writes its deck intent (a deck command to the target station, mail, task row) in that transaction too, so the existing deck outbox delivers it with a stable command id (ADR-004 §9.3).

### 6.3 Tool and class matrix

| MCP tool | Class | Wire | Target rule |
| --- | --- | --- | --- |
| `fleet_status` | read | query `status` | stations in the grant |
| `list_sessions` | read | query `sessions` | sessions on stations in the grant |
| `read_session` | read | query `session_tail { session_id, max_items ≤ 200 }` (≤ 64 KiB) | same |
| `list_tasks`, `wait_task` | read | query `tasks`, `wait_task { task_id }` | tasks created by this grant or assigned to the caller |
| `command_status` | read | query `command { command_id }` | commands of this grant only |
| `send_mail` | mail | call `send` | target sessions on stations in the grant |
| `create_task` | mail | call `assign` | same |
| `complete_task` | mail | call `complete_task` | tasks assigned to the caller |
| `interrupt_session`, `stop_session` | drive | call `interrupt`, `stop` | sessions with this grant in their lineage, on stations in the grant |
| `spawn_session` | spawn | call `spawn` | stations in the grant; mode ≤ `max_mode`; no grant in the spec; `cwd` inside the station's allowed roots (checked again by the station) |
| `assign_auto` | mail, plus spawn if it may start a session | call `assign_auto` | stations in the grant; a start counts against the cap |

- `tools/list` lists only the tools of the granted classes. A call of an unlisted tool gets the tool error `not_granted` from the station and is never forwarded. The deck checks the class again (step 6); the station check is not authority.
- Coordinators never answer approval or question requests in M3: no tool exists for it, and the deck refuses any such op. The event value `approval.resolved by: coordinator` stays reserved.
- The deck refuses a target outside the rule with a stored rejection `forbidden` (the parameters selected a target that the grant does not cover; the call itself was authentic).

### 6.4 Spawn-cap accounting

- Used count = the number of accepted `spawn` receipts, plus the accepted `assign_auto` receipts that started a session, for the grant. The check `used < spawn_cap` and the insert of the accepted receipt run in the same deck write transaction. The deck serializes write transactions (one writer connection), so two concurrent spawns can never both see the last free slot.
- A cap rejection is a stored rejection `spawn_cap_reached`; a repeat of the same id returns it, also after the owner raises the cap.
- A duplicate (same id and payload) returns the stored receipt and counts once.
- A later failure of the spawned session does not return its slot.

### 6.5 Duplicates, retry and ambiguity (station side)

- Intake: an authorized tool call that maps to `coordinator.call` gets a new `command_id` (ULID). The station stores the complete envelope (with the binding of the intake) in a `coordinator_outbox` row in the same transaction as the intake, before any send. The stored envelope never changes (G3). `retry_until` = intake + 15 min.
- Idempotency: the optional tool parameter `idempotency_key` (1–64 chars) is unique per binding. A second call with the same key and the same arguments returns the first call's `command_id` and state; the same key with other arguments gets the tool error `idempotency_key_reused`.
- The outbox worker sends the stored envelope; after a link loss or a timeout it sends the same envelope again until `retry_until` (the deck deduplicates, §6.2 step 4). After `retry_until` without a receipt, the row becomes `outcome_unknown` and is never sent again.
- The MCP tool call waits at most 30 s for the receipt. It then returns `{ command_id, state: pending }`; the agent uses `command_status`. A tool result never reports a pending or unknown outcome as a success or a failure.
- A revocation, an expiry or a generation change marks every queued, never-sent row `failed` (`revoked`, `expired`, `binding_stale`) without a send. A row that was sent and then gets `forbidden` or `binding_stale` becomes `outcome_unknown` (an earlier send may have been accepted before the revocation).

### 6.6 Queries and their expiry

- The station forwards a query only for an `active` binding and a granted `read` class. The deck runs §6.2 steps 1–3 for each query (not stored), then filters every result to the target rule of §6.3.
- A query that arrives after its `deadline` → `expired`. The deck answers by the deadline or with `timeout`.
- `wait_task` rechecks steps 2–3 when it wakes and at the deadline; a revocation or expiry during the wait ends it with `forbidden`.
- Every call and every query writes an audit row on the deck (grant, caller, tool, target, outcome; no payload text above 256 bytes).

## 7. Lifecycle, expiry, renewal and revocation

- Idle expiry: the station and the deck both check it; the station check stops the call before the forward, the deck check is the authority.
- Renewal: an owner board action on an `expired` (idle) grant before `expires_at`. The deck stores it and sends a session command `coordinator_renew { grant_id }` to the station (a new `SessionCommand` op with a station receipt). The station raises the generation and emits `coordinator.bound`. Every operation queued under the old generation fails (§6.5); a renewal never authorizes it.
- Revocation: the owner action commits `revoked` on the deck first; from that transaction on, every decision is `forbidden`. The deck then sends `coordinator_revoke { grant_id }` so the station marks the binding `revoked` and answers `401`. If the station is offline, the deck refusal is enough; the station applies the command when the link returns.
- Session end, process loss and native-identity replacement are station facts; the station revokes the binding and emits `coordinator.ended`; the deck marks the grant `revoked` from that event.
- Deck database replacement (another incarnation): every call fails `incarnation_mismatch`; grants of the old database are gone, so all bindings stay useless until the owner creates new grants.

### 7.1 Control skill

The deck holds the control skill text (versioned with the deck build). The station gets it once per binding through `coordinator.query { skill }` and serves it as the MCP `instructions` of `initialize` and as the resource `fleetdeck://control-skill`. The text holds no secret and no grant data.

## 8. Executable negative tests (written before the wiring)

The implementation order is: types and these tests first, then the wiring. Each test name is binding; the file is the planned location.

### 8.1 Station ingress — `crates/fleetdeck-station/src/coordinator/ingress_tests.rs`

1. `a_request_without_a_credential_gets_401`
2. `an_unknown_an_expired_and_a_revoked_credential_get_the_same_401_as_a_missing_one`
3. `a_request_with_an_origin_header_or_a_foreign_host_is_rejected_before_authentication`
4. `a_tool_parameter_that_names_another_session_does_not_change_the_caller`
5. `a_pending_binding_lists_no_tool_and_refuses_every_call_with_not_bound`
6. `a_tool_outside_the_granted_classes_is_not_listed_and_is_refused_without_a_forward`
7. `an_idle_binding_expires_and_a_renewal_does_not_authorize_an_operation_queued_before_it`
8. `process_exit_session_stop_and_a_replaced_native_identity_each_revoke_the_binding`
9. `the_rate_and_body_limits_refuse_excess_requests`

### 8.2 Credential secrecy — `crates/fleetdeck-cli/tests/coordinator_secret.rs` (real processes)

10. `the_raw_credential_appears_only_in_the_child_environment`: start a station and a fake agent with a grant; search the child's `/proc/<pid>/cmdline`, every file under the station directory (database, WAL, SHM, config), the event store, the read model, the captured station log and the deck database for the credential bytes; only `/proc/<pid>/environ` of the child may contain them.
11. `the_provider_arguments_name_the_variable_and_never_the_value` (Claude and Codex argument builders).
12. `a_provider_without_a_verified_handoff_has_no_coordinator_capability_and_the_deck_refuses_the_spec`
13. `a_station_local_start_with_a_coordinator_spec_is_rejected`

### 8.3 Deck decision — `crates/fleetdeck-deck/src/store/coordinator_tests.rs`

14. `a_revoked_or_expired_grant_is_forbidden_in_the_decision_transaction_and_not_stored`
15. `a_replay_after_a_revocation_is_forbidden_and_never_returns_the_old_acceptance`
16. `a_stale_generation_a_foreign_native_id_or_another_binding_is_binding_stale_and_not_stored`
17. `a_caller_session_of_another_station_is_forbidden`
18. `a_target_outside_the_grant_stations_or_lineage_is_a_stored_forbidden`
19. `a_spawn_that_carries_a_grant_or_a_mode_above_max_mode_is_refused`
20. `concurrent_spawns_never_exceed_the_cap` (32 parallel decisions with distinct ids, cap 3 → exactly 3 accepted)
21. `a_duplicate_spawn_returns_the_stored_receipt_and_counts_once`
22. `a_reused_command_id_with_another_payload_is_rejected`
23. `a_call_after_retry_until_is_expired_and_not_stored`
24. `an_expired_query_is_rejected_and_a_wait_ends_forbidden_when_the_grant_is_revoked`
25. `queries_return_only_targets_inside_the_grant`
26. `a_peer_without_the_coordinator_feature_gets_the_m2_behavior`

### 8.4 Station outbox — `crates/fleetdeck-station/src/coordinator/outbox_tests.rs`

27. `a_call_sent_before_a_link_loss_is_sent_again_with_the_same_envelope_and_deduplicated`
28. `a_call_without_a_receipt_after_retry_until_is_outcome_unknown_and_never_sent_again`
29. `the_same_idempotency_key_returns_the_first_command_and_another_payload_is_refused`
30. `a_revocation_fails_queued_unsent_calls_without_a_send_and_makes_sent_ones_outcome_unknown`

### 8.5 Opt-in live handoff smoke — `crates/fleetdeck-providers/tests/live_smoke.rs` (DL-067 gate)

31. `claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff`: one tiny session per provider with a test endpoint that records the `Authorization` header digest and answers `initialize` and `tools/list`; the test checks the digest, the argv and the absence of the value in the provider's own configuration files. It records the provider versions that pass; the adapters take those as the minimum versions (§5.4). It is not part of `make check`.

## 9. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| Link down | Calls stay in the station outbox and are sent again with the same envelope until `retry_until`; then `outcome_unknown`. Queries fail at once with `deck_unreachable`. |
| Deck restart | Receipts and grants are durable; the same envelope gets the stored receipt. |
| Station restart (direct host) | The providers end (SIGKILL test, DL-074); every binding becomes `revoked` (`process_lost`) at recovery; queued unsent calls fail `revoked`; sent ones without a receipt keep sending until `retry_until` and get `forbidden` → `outcome_unknown`. |
| Provider ignores the handoff | Its requests get `401`; the agent sees failing tools; nothing gains authority. |
| Clock skew | `retry_until`, `deadline` and expiry use the deck clock in the decision; the station's own checks only refuse earlier. The existing `CLOCK_SKEW_MS` admission rule applies. |

## 10. Alternatives

- **A credential in argv, in a config file, or in the URL.** Rejected by PLAN §7.1: other local users can read argv, and files and URLs leak into logs and backups.
- **A stdio MCP server process started by the provider (a `fleetdeck mcp` subcommand).** The provider would start it with the same environment, so it would need the same env handoff, plus a second hop from that process to the station socket. No gain; one more process per session.
- **Unix socket peer credentials (`SO_PEERCRED`) as authentication.** They give a PID and a UID; a PID alone is not identity and the UID is the same for every session.
- **Token digest on the wire (M2 shape).** Removed: the deck gains nothing from a secret-derived value (the pinned link authenticates the station), and one less copy of a credential-derived value leaves the station.
- **Grants issued to a running session.** Impossible without a new process: the handoff is in the start environment.

## 11. Risks

- Same-UID processes can read the credential (§3). Accepted by the PLAN; documented in the operations guide.
- The provider behaviors in §5.4 come from documentation, not yet from a local run. The capability stays disabled until §8.5 passes; if a provider changes its behavior, an authenticated call stops working (fail closed), and the live smoke shows it.
- Coordinators can spend money and disk by spawning sessions: the cap, the mode ceiling, the station allowed roots and the owner's revoke bound it.
- Long `wait_task` queries hold deck resources: bounded by the 60 s deadline and the per-binding concurrency limit.

## 12. Open points (owner-visible; none blocks the review)

- Coordinators cannot answer approvals in M3 (§6.3). A later owner decision may add a class for it.
- Default values (idle 30 min, expiry 12 h, cap 3) are executor choices; the owner may change them in settings.
