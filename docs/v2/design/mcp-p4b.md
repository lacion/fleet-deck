# MCP P4b — live grant changes and live tool lists

- **Status:** Draft revision 1, 2026-10-20 (for the advisor and the adversary DESIGN REVIEW).
- **Program:** DL-251 (D1), DL-255 (P4a), owner decisions L1–L3 of 2026-10-20 (`~/fleetdeck-briefs/mcp-p3-research-notes.md` §"P4b owner decisions").
- **Evidence:** spike `spikes/s-events/` (HTTP C1–C5, X1–X3; stdio C6–C8). Claude 2.1.302 re-lists its tools 2–7 ms after `notifications/tools/list_changed`: over HTTP only on a `subscriptions/listen` stream (C1; no TTL refetch, C2), over stdio on a listen stream **or** as an unsolicited line (C6, C7). Codex 0.163.0 never re-lists inside a running process (X1A–X1C).
- **Out of scope:** live changes of the `desktop`, `capture` and `input` classes and their caps (they stay start-only); a live change of a grant's scope or stations; `resources/updated`, prompt list changes, progress and cancellation work (P5 or later); Codex live tool lists (impossible in 0.163.0).

## 1. Goals

1. **Give (L1).** The owner gives a grant to a running or hibernated session that has no owner grant (a session started without one, a resumed session, a session started before P4a, a session with a worker grant).
2. **Change (L1).** The owner adds or removes classes of a session's owner grant (`read`, `mail`, `drive`, `spawn`, `inspect`, `manage`, `answer`); every widening needs the owner's elevation, as at creation.
3. **Live tool list.** A running Claude session sees the change within seconds, on both hosts (direct HTTP and shim stdio, L3).
4. **Codex at the next resume (L2).** The deck applies the change at once; the Codex tool list follows when the session's process starts again. An owner grant survives a hibernation and the resume.
5. **The deck stays the authority.** Every class-gated tool call is decided by the deck from the stored grant, so the station's tool list only decides what a session *sees*; a change is effective on the deck at once, also when the station is offline.

## 2. Station: a change channel on bindings

- `Bindings` gets a per-entry change signal: `Entry.changes: tokio::sync::watch::Sender<u64>` (a version counter), created by `insert`. `Bindings::watch(digest) -> Option<watch::Receiver<u64>>`.
- Every in-place change of a binding's `classes`, `grant_id`, `binding_id` or `state` bumps the version (`send_modify`). An entry that is removed, replaced by `insert` (same `binding_id`, a rotation) or set to `Revoked`/`Expired` drops or closes its sender, so every watcher ends.
- Receivers coalesce: many changes before a watcher wakes give one notification.

## 3. Station: the live tool list

### 3.1 Capabilities
`StationMcp::get_info` adds `.enable_tool_list_changed()` (both eras; a legacy client that cannot use it ignores it — X1A, X1B).

### 3.2 HTTP: `subscriptions/listen` (direct host)
**Preferred option B (rmcp's listen, streamed by the station):**
- `StationMcp` implements `accepted_subscription_filter` (accept only `toolsListChanged`; everything else is refused by the intersection) and `listen(ctx)`: it reads the binding from `ctx.request_context()` extensions, subscribes to `Bindings::watch(digest)`, and loops: a version change → `ctx.sink().notify_tool_list_changed()`; the watch closed (revoke, rotation, removal) → return `Ok(())` (rmcp sends the final listen result); `ctx.cancelled()` (client closed) → return.
- `McpService::serve` gets a second form for a listen: when the request's JSON-RPC method is `subscriptions/listen` in the modern era (the ingress already parses the message before rmcp, `answer_message`), the station returns rmcp's response body as a stream instead of collecting it. The ingress response body type becomes `Either<Full<Bytes>, BoxBody>` (only the listen uses the stream).
- **Permits.** A listen does not hold one of the binding's 8 request permits. It takes a listen permit instead: at most 2 open listens per binding (a third gets JSON-RPC error `-32000` "too many listen streams") and at most 256 per station. The listen permit lives as long as the stream (not `PermitIo`'s first-flush release). The connection limit rises from 128 to 512 (a listen keeps its HTTP/1.1 connection).
- Keep-alive: rmcp's SSE keep-alive (15 s). No response time limit is added.
- A legacy-era `subscriptions/listen` keeps today's answer (method not found).

**Alternative A (a station-owned listen):** the ingress answers a modern `subscriptions/listen` itself, before rmcp, and writes the SSE frames (`notifications/subscriptions/acknowledged`, `notifications/tools/list_changed` with the subscription id in `_meta`, keep-alive comments, the final result) from station code. Simpler streaming (no rmcp body plumbing) but the station re-implements protocol frames that rmcp already produces and that Claude accepted in C1.

### 3.3 Stdio: a push line through the bridge (shim host)
- **Socket.** A new first line `WATCH <token>` (instead of `AUTH <token>`) opens a watch connection. The station authenticates the token like `AUTH` (unknown, revoked, expired → `ERR unauthorized` and close; held → `ERR starting` and close), takes a watch permit (at most 2 per binding, 128 per station; `MAX_CONNECTIONS` rises from 64 to 256, and watch connections count in it), subscribes to `Bindings::watch(digest)`, and writes exactly the line `{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}` for every coalesced change. It closes the connection when the watch closes or the station stops. The station reads nothing more from a watch connection (EOF from the bridge closes it).
- **Bridge.** At start `mcp-bridge` spawns a watch task (token from `FLEETDECK_COORDINATOR_TOKEN`): connect, send `WATCH <token>`, read lines; each line that is exactly the notification above is written to stdout through the existing `emit` lock (other lines are dropped and logged). On any failure or close it reconnects with a backoff of 1 s doubling to 30 s while stdin is open; after every successful reconnect it writes one `notifications/tools/list_changed` (a change may have happened while it was away, for example a station restart). An `ERR unauthorized` stops the task for good (the credential is dead).
- **Listen over stdio.** A `subscriptions/listen` that arrives through the socket is answered at once with JSON-RPC error `-32601` (Claude retries 3 times and works on, C7); the push line is the mechanism.
- A shim session that started before the upgrade runs the old bridge and gets no push (it gets the new tools at its next resume).

### 3.4 Codex
No change: legacy era, no listen, no re-list. A change reaches Codex when its process starts again (§5).

## 4. Live grant commands

### 4.1 Proto and link
- Feature `coordinator_live.v1` (`version.rs`, after `WORKER_GRANT`).
- `SessionCommand::CoordinatorAttach { session_id, spec: Box<CoordinatorSpec> }` (`session.coordinator_attach`) and `SessionCommand::CoordinatorUpdate { session_id, grant_id, classes: Vec<String> }` (`session.coordinator_update`). Link methods table: scope Drive, feature `coordinator.v1` + `coordinator_live.v1`. An older station decodes neither; the deck never sends them on a link without the feature (§4.5).

### 4.2 Station: attach (give)
`guard` (paired deck) and `owner` (origin `Board`) as for revoke/renew. `kind` must be `coordinator`. Then, by the session's current state:
| The session has | Result |
| --- | --- |
| a fleet-only binding (running) | **in place** under the same digest (the process keeps its credential): `binding_id = "{s}:{grant}"`, `grant_id`, `classes = grant_classes(spec, features)` (no desktop/capture/input classes are allowed in an attach; refuse `invalid_grant`), `scope`, `control_skill`, `kind`; keep `native_id`, `provider_instance`; `generation = 1`; state unchanged (Pending stays Pending and binds later as today; Active appends `coordinator.bound` generation 1 now). Bump the change signal. |
| a worker-grant binding (running) | the same in-place swap; append `coordinator.ended { worker binding, generation, "replaced" }` before the new `coordinator.bound`. |
| an owner-grant binding not revoked | refuse `has_grant` |
| no binding (no MCP server: unverified handoff or bridge) | refuse `no_tools` |
| no process, hibernated (`summary.state == exited && summary.hibernated`) | store the grant as **parked** for the resume (§5); receipt accepted |
| no process, not hibernated (ended) | refuse `session_ended` |
Shim host: a new store function `shim_binding_regrant(digest, …)` rewrites the row's `binding_id`, `grant_id`, `classes`, `scope`, `control_skill`, `kind`, `generation`, `expires_at` in the decision transaction, so a station restart reloads the new grant (V42 is not needed: the columns exist). A parked grant on the shim host is a row in state `suspended` (§5).

### 4.3 Station: update (change)
Find the binding of `(session, grant_id)` (active, pending or parked). The new classes are `parse(classes) ∪ {Own} ∪` the binding's internal classes (`Recording`, `InputEnabled`, `ClickEnabled`, `InputAny`); the set of `desktop`, `capture`, `input` must equal the old one (else refuse `invalid_grant`). Update in place (and the shim row), bump the change signal. No lifecycle event (the deck already applied the change).

### 4.4 Station: revoke keeps the fleet tools
With `coordinator_live.v1` negotiated, `revoke` of a running binding no longer kills the credential: the entry becomes the session's fleet-only binding in place (`binding_id = "{s}:fleet"`, empty `grant_id`, classes `{Own}`, scope `Lineage`, the fleet skill, kind `coordinator`, state Active with the same native identity) and the change signal is bumped; `coordinator.ended { …, "revoked" }` is appended as today. A session then keeps exactly what every grant-less session has (D2). A parked binding is ended (row `ended`, `coordinator.ended "revoked"`).

### 4.5 Deck
- **Give:** `POST /api/coordinator/sessions/{station_id}/{session_id}/grant`, body = the board's coordinator request (the same JSON as `spec.coordinator` of a board start, without desktop, capture, input, recording or click fields). Owner login only. Checks: the station link negotiated `coordinator_live.v1` and `coordinator.v1` (else `409 not_supported`); the session is known and not ended (or hibernated); no owner grant in effect for `(station, session)` (`409 has_grant`); then the creation checks of `owner_start` (classes, `take_spawn_fields`, target stations, the elevation rule, `coordinator.v2` for fleet control), refactored into one shared function `check_owner_grant` used by both. The grant is created with `session_id` set, `start_command_id` NULL, and a new column `attach_command_id` (deck migration V46) = the attach command's id. A worker grant in effect for the session is revoked in the same transaction (`replaced`, no separate revoke command). The attach goes through the commands table (like `session.start`), so its receipt is tracked; the router holds it until the link negotiated `coordinator_live.v1`, and `commands::finish` revokes the grant (`attach_failed`) on a failed or rejected attach.
- **Change:** `POST /api/coordinator/grants/{grant_id}/classes`, body `{ classes, spawn_cap?, max_modes?, answer_cap? }`. Owner login only; kind `coordinator` (a worker grant has fixed classes: `409 worker_grant`); not revoked; the `desktop`/`capture`/`input` membership unchanged; the class rules of creation (a new `spawn` takes `spawn_cap`/`max_modes` by `take_spawn_fields`; a new `answer` needs `answer_cap`; a new fleet-control class needs `coordinator.v2` on every station of the grant). **Widening** (any added class) needs elevation; removing classes needs none. In one transaction: update `tokens.classes` (and the caps), compare with the stored row, audit `coordinator.grant_classes` (old and new), `sync_coordinate_scopes`; then queue `session.coordinator_update` (commands table, held until `coordinator_live.v1`). The deck change is effective at once; a lost update only leaves the station's *visible* list stale (§1 goal 5).
- **Revoke:** unchanged on the deck.
- **GrantView** carries nothing new (classes are already there).

### 4.6 Board
- Session header menu item **Fleet access…** (owner only; hidden when the station lacks `coordinator_live.v1`): with no owner grant, a form with the spawn form's fleet fields without the desktop, screenshot and input switches (`FleetControlFields` with a `live` flag) and a **Give access** button; with an owner grant, the class checkboxes prefilled from the grant and **Save**. Widening asks for the elevation first (as the spawn form). For a Codex session the form says "Codex sees the change when the session resumes."
- The Fleet panel grant row gets **Change access** (opens the same form).

## 5. Owner grants survive a hibernation (L2)

- **Park.** When a hibernation stops a process (the stop effect of a `hibernate:` command; the summary has `hibernated`), the station **parks** every binding of kind `coordinator` with a grant: direct host — the entry gets the new state `Parked` (`authenticate` refuses it as unauthorized; the old token is dead); shim host — the row becomes `suspended` (no `ended`); **no** `coordinator.ended` is appended, so the deck keeps the grant. Worker bindings and fleet-only bindings end as today. Any other process end (exit, crash, stop) ends every binding as today.
- **Resume.** `tools_generic` with `resumed` looks for a parked binding of the session whose `expires_at` is in the future: it prepares the grant binding again with a **new credential** under the same `binding_id`, `grant_id` and `generation` (direct: `Handoff::prepare` + `insert`, which drops the parked entry; shim: `rotate_out` of the parked row + `store_tools` with the grant row). The native identity check of the first bind applies as today (a changed root id revokes). No new lifecycle event: the deck's grant is unchanged and already bound at that generation. Otherwise the resume gets the fleet-only binding as today.
- **Deck.** A parked grant is an active grant with no traffic: its idle time counts from `last_used_at` and its lifetime from creation. If it idle-expires while parked, calls after the resume are refused as for any idle-expired grant and the owner renews it (today's renew). The `session.ended { process_exit }` that a hibernation appends does not touch owner grants (the P4a rule revokes worker grants only).
- **End of a parked grant:** the owner's revoke (§4.4), the lifetime (`binding_start::load` already expires rows; the direct host checks `expires_at` at the resume), or the session's removal on the station.

## 6. Authorization invariants (for the review)

1. Only the owner login gives or changes a grant (deck route `owner_only`; station `owner` = origin `Board` from the paired deck). No coordinator tool can give or change a grant.
2. Every widening needs the owner's elevation, compared with the stored values in the transaction; the creation rules apply to every added class.
3. The deck decides every class-gated call from the stored grant (the station's tool list is visibility only); a narrowing is effective on the deck at once.
4. A worker grant never becomes an owner grant: a give replaces it (revoked `replaced`), it is never widened.
5. The in-place change keeps the session's credential; a revoke leaves exactly the grant-less tool set (D2); a park kills the credential and a resume mints a new one.
6. Listen and watch connections are authenticated like requests, bounded per binding and per station, and end when the binding ends or rotates.

## 7. Tests (names are binding)

Station: 1. `a_change_bumps_the_watch_and_a_revoke_or_rotation_closes_it`; 2. `a_listen_stream_sends_list_changed_and_ends_with_the_binding` (HTTP, modern era, the ack, a change, the end; the listen permit limits; a legacy listen still gets method-not-found); 3. `a_watch_connection_pushes_one_line_per_change_and_closes_on_revoke` (socket; unauthorized; limits); 4. `the_bridge_writes_the_push_line_and_reconnects_after_a_station_restart`; 5. `an_attach_upgrades_the_fleet_binding_in_place_on_both_hosts` (the same token works with the new tools; a worker swap with `replaced`; `has_grant`, `no_tools`, `session_ended`; a station restart reloads the regranted row); 6. `an_update_changes_the_classes_in_place_and_keeps_desktop_classes_fixed`; 7. `a_live_revoke_keeps_the_fleet_tools`; 8. `an_owner_grant_parks_at_a_hibernation_and_returns_with_a_new_token_at_the_resume` (direct and shim; worker and fleet bindings end; an expired parked grant gives the fleet binding; a crash ends the grant).
Deck: 9. `the_owner_gives_a_grant_to_a_running_session` (elevation rules, `has_grant`, `not_supported`, a worker replaced, desktop classes refused, the attach held until the feature, a failed attach revokes); 10. `class_changes_need_elevation_for_every_widening_and_apply_on_the_deck_at_once`; 11. `a_member_or_a_token_cannot_give_or_change_a_grant`; 12. migration V46.
Board: 13. the Fleet access form (give, change, elevation, the Codex note, hidden without the feature).
Live (architect): a running Claude session on a shim station gets `spawn` from the board and lists the spawn tools without a restart; a Codex session gets them after a hibernation and resume.

## 8. Phases
- **P4b-1 (station infrastructure):** §2, §3 (listen, watch, bridge), tests 1–4.
- **P4b-2 (commands and survival):** proto §4.1, station §4.2–§4.4, §5, tests 5–8.
- **P4b-3 (deck and board):** §4.5, §4.6, tests 9–13.
