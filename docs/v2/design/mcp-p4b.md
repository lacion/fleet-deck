# MCP P4b — live grant changes and live tool lists

- **Status:** Draft revision 2, 2026-10-20 (for the adversary DESIGN REVIEW). Revision 1 (997b107d) was reviewed by the advisor (Codex, `~/fleetdeck-briefs/mcp-p4b-advisor-r1.md`, summarized in §12); revision 2 applies all ten corrections.
- **Program:** DL-251 (D1), DL-255 (P4a), owner decisions L1–L3 of 2026-10-20 (`~/fleetdeck-briefs/mcp-p3-research-notes.md` §"P4b owner decisions").
- **Evidence:** spike `spikes/s-events/` (HTTP C1–C5, X1–X3; stdio C6–C8). Claude 2.1.302 re-lists its tools 2–7 ms after `notifications/tools/list_changed`: over HTTP only on a `subscriptions/listen` stream (C1; no TTL refetch, C2), over stdio on a listen stream **or** as an unsolicited line (C6, C7). Codex 0.163.0 never re-lists inside a running process (X1A–X1C).
- **Out of scope:** live changes of the `desktop`, `capture` and `input` classes, of their caps and allowances, of a grant's scope or stations; `resources/updated`, prompt list changes, progress and cancellation work; Codex live tool lists (impossible in 0.163.0); undoing work that a station already received before a narrowing.

## 1. Goals

1. **Give (L1).** The owner gives a grant to a session that has no owner grant (running or not: a session started without one, a resumed session, a session started before P4a, a session with a worker grant).
2. **Change (L1).** The owner adds or removes the classes `read`, `mail`, `drive`, `spawn`, `inspect`, `manage`, `answer` of a session's owner grant. Every widening needs the owner's elevation, as at creation.
3. **Live tool list.** A running Claude session sees a change within seconds on both hosts (direct HTTP and shim stdio, L3).
4. **Grants outlive processes (L2).** An owner grant belongs to the session, not to one provider process: when the process ends (hibernation, crash, exit) and the session starts again (resume, auto-continue), the new process gets the grant at its start. So Codex sees a change at its next start.
5. **The deck stays the authority.** Every class-gated call is decided by the deck from the stored grant, and a narrowing also stops queued work that the removed classes authorized (§7). The station's tool list decides only what a session *sees*.

## 2. The model: a desired state per session

- **Deck.** The `tokens` row of an owner grant is the desired state. A new column `revision INTEGER NOT NULL DEFAULT 0` counts its changes (give = 1, each change +1, revoke = terminal). A new column `station_revision INTEGER NOT NULL DEFAULT 0` holds the highest revision that the station acknowledged. A new column `epoch INTEGER` (§5) is the execution generation of the current process.
- **Station.** A new durable table `session_grants` (station migration V42): `session_id PK`, `grant_id`, `revision`, `spec` (JSON: the station form of the `CoordinatorSpec`: classes, scope, control skill, expiry, kind, the allowances that `grant_classes` reads), `epoch`, `state` (`attached` | `ended`), `updated_at`. One row per session; an `ended` row is a tombstone for its `grant_id` (a later message for that grant never revives it). It is independent of credential rows (`shim_bindings`) and of shim cleanup.
- **The binding of a running process is derived from the record**: at every process start (start, resume, auto-continue resume) and on every accepted change. Credentials keep their own lifecycle (§4).
- A record exists only when the deck link negotiated `coordinator_live.v1` when the record was written (§9); without a record every rule of today applies unchanged.

## 3. One command: `session.coordinator_set`

- Proto: `SessionCommand::CoordinatorSet { session_id, grant_id, revision: u64, spec: Option<Box<CoordinatorSpec>> }` (`session.coordinator_set`). `Some(spec)` is the full desired state (give or change); `None` is the end (revoke). The existing `session.coordinator_revoke` stays for links without the feature.
- Station rules, in the decision transaction:
  1. `guard` (paired deck) and `owner` (origin `Board`); `kind` must be `coordinator`; no `desktop`/`capture`/`input` class may differ from the record's (a give may not carry them at all).
  2. Read the record. `revision <= record.revision` for the same grant → accepted as a duplicate, no change. An `ended` record of the same `grant_id` → refuse `grant_ended`. A record of **another** `grant_id` in state `attached` → refuse `has_grant` (the deck replaces only through an end first, §6.1).
  3. Write the record (`attached` with the new spec and revision, or `ended`), then apply it to the running binding, if any (§4), in the same transaction (shim: the row rewrite; direct: memory after the commit).
  4. The receipt's result carries the applied revision; the deck sets `station_revision = max(station_revision, revision)`.
- Ordering: the deck queues at most one pending `coordinator_set` per grant (a newer one supersedes and cancels the older pending command in the same transaction). Revisions make any reordering harmless; the tombstone makes a delayed give after a revoke harmless.
- Reconcile: when a link that negotiated `coordinator_live.v1` comes up, the deck queues the latest desired state for every grant of that station whose `station_revision < revision` and that has no pending set.

## 4. The running binding

### 4.1 In-place changes keep the credential
The provider process holds its token in its environment, so a change of a running session never mints a credential. The binding entry under the same digest is rewritten:
- **give or change** (record `attached`): `binding_id = "{s}:{grant}"`, `grant_id`, `classes = grant_classes(record.spec, features)`, `scope`, `control_skill`, `kind`, `expires_at` (new field of `Binding`, from the spec). The native identity and the state stay as they are: a `Pending` binding stays `Pending` (it binds later as today); an `Active` one stays `Active`.
- **end** (record `ended`, a revoke): the entry becomes the session's fleet-only binding in place (`"{s}:fleet"`, empty `grant_id`, classes `{Own}`, scope `Lineage`, the fleet skill, no expiry, every internal allowance cleared), state unchanged. A session then keeps exactly what every grant-less session has (D2).
- **Shim host, durably:** a new store function rewrites the `shim_bindings` row of the digest (`binding_id`, `grant_id`, `classes`, `scope`, `control_skill`, `kind`, `expires_at`) in the decision transaction; memory is published only after the commit; a failed save leaves the old entry gated (the request answers `starting`) until the next reload. Recovery loads the rewritten row, never the old authority.
- **Lifecycle events:** a give or a change of an `Active` binding appends `coordinator.bound { binding_id, grant_id, generation: epoch, … }` only for the first attach of that grant in this process (the deck's first bind); a change appends nothing; an end appends `coordinator.ended { old binding_id, epoch, "revoked" }`. A worker binding that a give replaces appends `coordinator.ended { worker binding, generation, "replaced" }` first (§6.1).
- Never: an in-place rewrite of a `Revoked` or `Expired` entry, of an entry whose shim row is `ended` or `suspended`, or of a fenced digest. Those stay dead; a session whose credential is dead gets the record at its next process start.

### 4.2 Hard invalidation stays
Rotation (resume), the process end, a fence, a changed native identity, a station-local refusal and the station's own expiry check keep killing the credential exactly as today. Only the deck's revoke of an owner grant with a record becomes the in-place downgrade. A leaked token therefore keeps fleet-only access until its process ends (the same as a grant-less session) and would see a later give to that session; this is accepted (the token never leaves the provider's environment).

## 5. Process starts: the execution epoch (L2)

- At every process start of a session whose record is `attached` and whose `spec.expires_at` is in the future, `tools_generic` prepares the **grant** binding (not the fleet-only one), with a new credential, `generation = record.epoch + 1`, state `Pending`, and stores the new epoch in the record. The shim host uses the grant row in `store_tools` as at a start; `rotate_out` ends only the previous execution's rows of this session, with reason `rotated` and **without** a `coordinator.ended` for a grant that has a record.
- When the new process's native identity is known, the binding becomes `Active` and the station appends `coordinator.bound { generation: new epoch }`. For a resume the expected native identity is the session's stored native id: a mismatch revokes (as today) and ends the record (`coordinator.ended "identity_changed"`); until the identity is verified the binding is `Pending` and every gated call is refused (as today for `Pending`).
- At the process end the binding ends as today (credential dead), but for a grant with a record the station appends `coordinator.ended { …, "process_exit" }` **only** when the record is `ended`; otherwise it appends `coordinator.detached { binding_id, grant_id, generation }` (new event, decoded by an older deck as `Unknown`, only sent on a `coordinator_live.v1` link).
- **Deck:** `coordinator.detached` of the bound generation sets the grant's binding to detached (the state stays `active`; the idle clock and the lifetime keep running; no call can arrive, there is no credential). A later `coordinator.bound` from the same station and session with `generation > ` the detached generation rebinds the grant (no owner renewal): `epoch = generation`, `invalidate_stale_generation` runs, the usage counters and the idle clock are kept. Every other higher generation still needs the owner's pending renewal (today's rule).
- **Renewal:** an idle-expired grant with a record renews as today; the station's renew accepts a binding of the current epoch in state `Expired`, and a record whose process is gone (renew then only updates the record; the next process start uses it).
- **Late or stale work:** every station outbox item and every deck operation is keyed by `(grant_id, generation)`; a new epoch invalidates the old generation's pending work on both sides (`invalidate_stale_generation` on the deck; the station drops outbox items of an older generation of the grant).

## 6. Deck routes

### 6.1 Give: `POST /api/coordinator/sessions/{station_id}/{session_id}/grant`
- Owner login only (`403 owner_only` for a member or a token). Body: the board's coordinator request (the JSON of `spec.coordinator` of a board start) without desktop, capture, input, recording or click fields (`400 invalid_grant` "desktop, screenshot and input access is given only at start").
- Checks: the station link negotiated `coordinator.v1` and `coordinator_live.v1` (`409 not_supported`); the session is known on the deck and not deleted; then the creation checks of `owner_start`, moved into one shared function `check_owner_grant` used by both (classes, `take_spawn_fields`, target stations, the elevation rule, `coordinator.v2` for fleet control).
- **One owner attachment per session**, reserved in the transaction: a new deck table `session_owner_grant(station_id, session_id PK, grant_id)` holds the session's current owner grant until that grant is revoked or its lifetime ends. A give for a session that has an entry → `409 has_grant` (also when the grant is idle-expired: the owner renews it or revokes it first). Concurrent gives: the second transaction finds the entry.
- In one transaction: create the grant (`session_id` set, `start_command_id` NULL, `revision = 1`), insert `session_owner_grant`, revoke a worker grant in effect for the session (`replaced`) and queue for it the `coordinator_set { worker grant, end }`-equivalent (the existing worker revoke), queue `coordinator_set { grant, 1, spec }` (commands table; held by the router until the link negotiated `coordinator_live.v1`), audit `coordinator.grant_created` with `live=true`.
- A failed or rejected give (`commands::finish`) revokes the grant (`attach_failed`) and deletes its `session_owner_grant` entry. A `coordinator.bound` for a revoked owner grant queues `session.coordinator_set { end }` (the P4a late-bound rule, extended to owner grants with a record).

### 6.2 Change: `POST /api/coordinator/grants/{grant_id}/classes`
- Owner login only. Body `{ classes, spawn_cap?, max_modes?, answer_cap? }`. The grant: kind `coordinator` (`409 worker_grant` for a worker grant: its classes are fixed), not revoked, has a `session_owner_grant` entry or its station negotiated `coordinator_live.v1`.
- Rules: `desktop`/`capture`/`input` membership unchanged; at least one class; a newly added `spawn` requires `spawn_cap` and `max_modes` (`take_spawn_fields` rules); a newly added `answer` requires `answer_cap`; a new fleet-control class needs `coordinator.v2` on every station of the grant and the fleet-control switch; the caps fields are allowed only with their newly added class (every other cap change goes through the caps route).
- **Widening, compared with the stored row in the transaction:** any added class; a cap that is higher than the stored one; a new provider family in `max_modes`; a ceiling that is not `mode_within` the stored one (the rules of `grant_spawn_caps.rs`). Widening needs elevation; anything else needs none.
- In one transaction: write the classes and the caps (the usage counters are never reset; a removed and re-added class keeps its counters), `revision += 1`, audit `coordinator.grant_classes` (old and new classes and caps), `sync_coordinate_scopes`, apply §7 (stop queued work of removed classes), and queue `coordinator_set { grant, revision, spec }` superseding any pending set.

### 6.3 Revoke
Unchanged route. For a grant with a `session_owner_grant` entry on a `coordinator_live.v1` link: `revision += 1`, delete the entry, queue `coordinator_set { end }` instead of `session.coordinator_revoke`.

## 7. Narrowing stops queued work (advisor correction 2)

- Every deck coordinator operation and intent that the deck sends to a station on behalf of a grant records `grant_id`, the generation, and the **class that authorized it** (today non-fleet intents carry no command authority: they get it).
- Before every send and resend (the router and the outbox), and before the deck executes a queued deck-side step (land, move, assign), the deck re-checks that the grant is in effect, at that generation, and still has that class; otherwise the item fails with `authority_removed` and is not sent.
- A narrowing (§6.2) or a revoke fails, in its own transaction, every pending item of the grant whose class was removed.
- Work that a station already received is not undone (out of scope); the station executes only commands from its paired deck.

## 8. Live tool list transport

### 8.1 A change signal bound to the credential
- `authenticate` returns, under the bindings lock, a stable handle: the digest, an entry incarnation number (bumped by `insert`), the current version, and a `watch::Receiver<u64>` registered at the same moment. `BindingExt` carries the handle, not only the snapshot.
- In-place changes (§4.1) bump the version and keep the watch open (the listener notifies). Hard invalidation (§4.2: revoke of a credential, rotation, removal, expiry, process end, fence) closes the watch; every listen or watch stream on it ends.
- Receivers coalesce (one notification for many changes).

### 8.2 HTTP: rmcp's `subscriptions/listen` (option B)
- `get_info` adds `.enable_tool_list_changed()`. `StationMcp` implements `accepted_subscription_filter` (only `toolsListChanged`) and `listen(ctx)`: it takes the handle from the request extensions, sends one `notify_tool_list_changed()` right after the acknowledgment when the handle's version is newer than the version the request authenticated with (a change during the reconnect), then loops: version change → notify; watch closed → return `Ok(())` (the final result); `ctx.cancelled()` → return.
- `McpService::serve` returns, for a modern `subscriptions/listen` only, rmcp's response with its headers (`Content-Type`, `Cache-Control`, `X-Accel-Buffering`) and its body unbuffered; every other request keeps today's collected reply and `PermitSlot` flush rule. The ingress response body is `Either<Full<Bytes>, BoxBody>`.
- **Admission:** authentication and the rate check are separate from the 8 ordinary request permits. A listen takes a listen permit (at most 2 per binding, 256 per station, acquired atomically) that the **body** owns for its whole life (not `PermitSlot`); the unauthenticated intake permit is released after the handshake. The connection limit becomes 512, of which listens may hold at most 256.
- **Stalls:** a notification frame that cannot be written within 30 s ends the stream; rmcp's 15 s keep-alive stays; there is no lifetime limit for a healthy listener.
- A legacy-era `subscriptions/listen` keeps today's method-not-found answer.

### 8.3 Stdio: a push line through the bridge
- **Socket.** First line `WATCH <token>`. The station answers one line: `OK watch.v1` (registered, with the handle of §8.1), `ERR unauthorized` (dead credential), `ERR starting`, `ERR limited`; an older station answers `ERR bad_request`. Limits: 2 watches per binding, 128 per station, `MAX_CONNECTIONS` 64 → 256 with at most 128 watches. After `OK` the station writes only the line `{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}` per coalesced change (a write stalled for 30 s closes); it reads with a bounded buffer and closes on EOF or on any client byte. A closed watch closes the connection.
- **Bridge.** It starts the watch task only after it relayed the first successful answer to `server/discover` or `initialize`. Then: connect, `WATCH`, expect `OK watch.v1`; write each received line that is exactly the notification (at most 256 bytes; anything else closes the connection) to stdout through `emit`, coalescing (at most one pending). On `ERR unauthorized` or `ERR bad_request` (unsupported) the task stops for good; on `ERR starting`, `ERR limited` or a connection error it retries with backoff 1 s → 30 s plus jitter; after a reconnect that follows a lost **established** watch it writes one notification. The task stops when stdin closes.
- A `subscriptions/listen` that arrives through the socket is answered at once with JSON-RPC error `-32601` (before the collecting path). A shim session started before the upgrade runs the old bridge and gets no push; it gets the record's tools at its next process start.

### 8.4 Codex
Unchanged: legacy era, no listen, no re-list; the record reaches it at its next process start (§5).

## 9. Versions and feature gating

- Feature `coordinator_live.v1` (`version.rs`, after `WORKER_GRANT`). The deck sends `coordinator_set` only on a link that negotiated it; the router holds it otherwise; the give and change routes refuse with `409 not_supported`; the board hides the actions.
- The station writes a record only from a `coordinator_set` (or from a start whose deck link negotiated the feature: a start-time owner grant then gets a record too, so it also outlives its process); the station's behaviour for a grant **without** a record is exactly today's (process end → `coordinator.ended process_exit`, revoke kills the credential). So every new lifecycle rule is tied to a record that only a `coordinator_live.v1` deck creates, and is persisted with it.
- `coordinator.detached` is sent only for grants with a record; the projection of the snapshot gets no new variant (a detached grant is absent from the binding projection; the deck keeps its state).
- WATCH is negotiated by its own `OK watch.v1` answer; an old station never causes synthetic refreshes.

## 10. Authorization invariants

1. Only the owner login gives, changes or ends a grant; no coordinator tool can.
2. Every widening (class, cap, family, ceiling) needs the owner's elevation, compared with the stored row in the transaction; a removed and re-added class keeps its usage counters.
3. The deck decides every class-gated call from the stored grant; a narrowing fails queued work of the removed classes before any send; already-sent work is not undone.
4. A worker grant is never widened; a give replaces it.
5. One owner grant per session (reserved in the transaction); a tombstone stops a delayed give after a revoke; revisions order every change.
6. In-place changes keep the credential; hard invalidation keeps killing it; a dead credential is never revived; a new process gets a new credential and a new epoch that the deck acknowledges.
7. Listen and watch streams are authenticated, bounded per binding and per station, own their permits, detect stalls and EOF, and end with their credential.

## 11. Tests (names are binding)

Station:
1. `a_change_bumps_the_watch_and_hard_invalidation_closes_it` (in-place keeps it open; revoke-of-credential, rotation, expiry, process end close; registration is race-free with the authenticate handle).
2. `a_listen_stream_notifies_and_ends_with_the_credential` (HTTP modern era: ack, a change, a downgrade keeps it open, a rotation ends it with the final result; permits: 8 busy request permits do not block a listen, the 3rd listen of a binding is refused, the 257th of the station is refused; a stalled reader ends after 30 s; a legacy listen gets method-not-found; the socket listen gets -32601).
3. `a_watch_connection_pushes_lines_and_closes_with_the_credential` (OK, unauthorized, starting, limited, bad_request from an old station; EOF and client bytes close; limits).
4. `the_bridge_starts_its_watch_after_discovery_and_reconnects_with_backoff` (no line before the first answer; one line after a lost established watch; stops on unauthorized, on bad_request and on stdin close; coalescing; oversize line closes).
5. `coordinator_set_orders_revisions_and_keeps_tombstones` (older revision ignored, duplicate accepted, `grant_ended` after an end, `has_grant`, desktop classes refused, `kind worker` refused).
6. `a_set_rewrites_the_running_binding_in_place_on_both_hosts` (the same token sees the new tools; a pending binding stays pending; a worker binding is replaced with `replaced`; a revoke downgrades to fleet-only and keeps the token; a dead credential is never rewritten; a station restart reloads the rewritten row; a failed row save stays gated).
7. `a_grant_with_a_record_returns_at_every_process_start_with_a_new_epoch` (hibernate + resume, crash + auto-continue resume, both hosts; `coordinator.detached` at the end; a changed native identity revokes and ends the record; an expired record gives the fleet binding; a grant without a record behaves as today).
8. `the_old_epoch_work_is_dropped_after_a_new_process_start`.
Deck:
9. `the_owner_gives_a_grant_to_a_running_or_hibernated_session` (elevation rules from `check_owner_grant`; `has_grant` also for an idle-expired grant; concurrent gives; a worker grant replaced; desktop fields refused; held until the feature; a failed give revokes and frees the session).
10. `class_and_cap_changes_need_elevation_for_every_widening` (an added class, a higher cap with the same classes, a new family, an incomparable ceiling; counters kept across remove and re-add; worker grants refused).
11. `a_narrowing_fails_queued_work_of_the_removed_class` (a queued `manage` command after `manage` is removed; a resend; a deck-side land or move step).
12. `detached_and_rebound_epochs_keep_the_grant_without_a_renewal` (detached → bound at a higher generation rebinds; any other higher generation still needs a renewal; idle expiry while detached; the late-bound rule queues an end for a revoked owner grant).
13. `revisions_reconcile_after_a_reconnect_and_a_newer_set_supersedes` (reordered sets; the reconnect queues the latest state).
14. `a_member_or_a_token_cannot_give_change_or_end_a_grant`.
15. Migrations: deck `tokens.revision`, `station_revision`, `epoch`, `session_owner_grant`; station V42 `session_grants`.
Board: 16. the Fleet access form (give, change, elevation for widening, the Codex note, hidden without the feature).
Live (architect): a running Claude session on a shim station gets `spawn` from the board and lists the spawn tools without a restart; a Codex session gets them after a hibernation and resume; a revoke leaves the fleet tools.

## 12. Advisor review map (revision 1 → 2)

| Advisor correction | Section |
| --- | --- |
| 1. Elevation bypass through caps and ceilings | §6.2 widening rule; test 10 |
| 2. Narrowing must stop queued work | §7; test 11 |
| 3. Do not reuse `suspended` for parking | §2 the `session_grants` record; §5 (no parking state at all) |
| 4. Explicit execution transition at a resume | §5 the epoch, `coordinator.detached`, deck rebind; tests 7, 8, 12 |
| 5. Durable downgrade, keep hard invalidation | §4.1, §4.2; test 6 |
| 6. Ordered desired state, one attachment per session | §2, §3 revisions and tombstones, §6.1 `session_owner_grant`; tests 5, 9, 13 |
| 7. Race-free registration; downgrade keeps the stream | §8.1; tests 1, 2 |
| 8. Separate admission, owned permits, stalls, EOF | §8.2, §8.3; tests 2, 3, 4 |
| 9. Gate lifecycle semantics; negotiate WATCH; no early notification | §9, §8.3; tests 3, 4 |
| 10. Adversarial tests; module splits | §11; §13 |

## 13. Phases and module splits

- **P4b-0 (refactor, no behaviour change):** split `coordinator_start.rs` (799 lines), `bindings.rs` (778) and `grant_routes.rs` (748) by topic before any feature work; move `owner_start`'s checks into `check_owner_grant`.
- **P4b-1 (station transport):** §8 (handle, listen, watch, bridge); tests 1–4.
- **P4b-2 (station desired state):** proto §3, §9; station §2, §3, §4, §5; tests 5–8.
- **P4b-3 (deck):** §6, §7, deck side of §5; tests 9–15.
- **P4b-4 (board):** the Fleet access form; test 16.
