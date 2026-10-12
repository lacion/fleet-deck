# M3 lane B (orchestration) decisions

Lane B of M3 numbers its decisions DL-200 to DL-299. The format is the
format of [DECISIONS-LOG.md](../DECISIONS-LOG.md): context, options, choice,
reason, owner review.

## DL-200: Callsigns and workspace provisioning in the start effect (M3 6-1)

- Context: PLAN §6.6 asks for worktrees `<repo>--fd-<callsign>` on branch
  `fd/<callsign>`, a credential-safe clone, and filesystem work through the
  effect worker. v2 had no callsign. Mail (6-2), tasks (6-4) and the
  coordinator (6-5) need a short, unique address for a session. A larger
  `session.started` event made the `Event` enum too large for clippy
  (`large_enum_variant` on `EventBody` and on both provider cores).
- Options: (a) a new `session.provision` command and effect before the
  start; (b) an optional `workspace` field on `session.start`, planned and
  checked in the command transaction, executed by the start effect before
  the provider starts; (c) the board runs `git` through a shell.
- Choice: (b). `SessionCommand::Start` gets `workspace: Option<WorkspaceRequest>`
  (`{kind: "worktree"}` or `{kind: "clone", url}`; absent = the M2
  behaviour). The decision gives every new session a callsign: the v0
  rotation (`falcon` … `drift`, starting at the number of active sessions)
  plus the last four id characters, then the whole id when all twelve are
  taken; unique on the station. It is recorded by a new station event
  `session.named {callsign, worktree}` (older peers keep it as unknown), not
  by new fields on `session.started`. The worktree plan needs the top
  directory of a main checkout, a parent inside the allowed roots and a
  free path; the clone plan needs a URL without credentials (`https://`
  without user info, `ssh://` without a password, `user@host:path`, or
  `file://` inside the roots; `<helper>::` and other transports are
  `forbidden`) and a new plain directory name whose parent is inside the
  roots. The start effect runs `git worktree add -b fd/<callsign> -- <path>`
  or `git -c protocol.ext.allow=never clone --no-recurse-submodules -- <url>
  <path>` with the station's own Git configuration and credentials, before
  the provider starts. A failed Git step ends the session with
  `start_failed` ("the workspace was not created: …"); a repeated effect
  finds the worktree on its branch and continues.
- Reason: one command keeps the A9 idempotency (one command id, one
  receipt) and the PLAN §6.4 rule "filesystem work through the effect
  worker"; the decision only reads the file system. A separate event keeps
  the hot `Event` enum small and needs no change on the provider side.
- Owner review: pending

## DL-201: Repo catalog fields and repo grouping (M3 6-1)

- Context: PLAN §6.6 asks for a repo catalog (path, scrubbed origin,
  default branch) and repo grouping: a heuristic by normalized origin, and
  an explicit owner choice for repos with several remotes or none.
- Options: (a) a new `repo.list` link method; (b) optional fields on the
  existing `workspace.list` entries.
- Choice: (b). `WorkspaceEntry` gets optional `origin` (user info, query
  and fragment removed), `default_branch` (`refs/remotes/origin/HEAD`, else
  the current branch) and `group`. The station reads these facts from
  `.git/config` and `.git/refs` (no `git` process in a listing). `group` is
  the owner's choice from the new `[repo_groups]` table of `station.toml`
  (path → name), else the normalized origin (`host/path`, lower case,
  without `.git`) when the repo has exactly one remote, else absent.
- Reason: no new link method and no new board call; older boards ignore
  the fields. The conflict radar (6-7, lane C) reads `group`.
- Owner review: pending

## DL-202: Batch spawn and `Nx` are a board expansion (M3 6-1)

- Context: v0 batch spawn starts one agent for each line, `3x task` starts
  three, and each agent gets a forced worktree.
- Options: (a) a station `session.start_batch` command; (b) the board
  expands the text and sends one `session.start` for each prompt.
- Choice: (b). `batchPrompts` (`web/src/spawn/spawn.ts`) gives one prompt
  for each non-empty line; `Nx task` with N from 1 to 10 repeats the task.
  A batch always asks for a worktree. Each start has its own command id;
  the form stops at the first refusal and shows "k of n sessions started".
- Reason: every session stays one idempotent command; the station needs no
  new command and the cap check (`max_sessions`) works per start.
- Owner review: pending

## DL-204: `assign auto` is a deck method with a pure ranking (M3 6-3)

- Context: PLAN §7.3 asks for v0's SQL choice (idle first, least buried,
  right repo) plus station terms (connected, repo exists, under
  `max_sessions`, provider available), and "spawn on <best station>" when
  no session fits. The deck read model holds every card of every station.
- Options: (a) an SQL query on the deck read model; (b) a pure Rust ranking
  over `sessions()` and the station views; (c) a station method.
- Choice: (b). `POST /api/assign {command_id, text, repo?, provider?}`
  (`session.assign`, drive scope). Eligible cards: `idle` (rank 0),
  `queued` (1), `working`/`verifying` (2); `needs_you`, `ended`,
  `unreachable` and `lost` take no task. Then the fewest undelivered mails
  (an empty map until 6-2), then the newest `updated_at`. A repo matches the
  session `cwd` when it is the repo, inside it, or a `<repo>--fd-<callsign>`
  worktree. The deck sends `[FLEETDECK ASSIGNMENT] <task>` as
  `session.prompt` to an idle session, else as `session.follow_up`, with the
  board's command id; a resend with that id returns the stored command and
  sends nothing. Unrouted: `spawn_on` is the reachable station with the
  most free places under `max_sessions` (a station without `station.info`
  is skipped) and a `signed_in` instance of the provider, whose
  `workspace.list` finds the repo. The board offers "Spawn on <station>",
  which opens the spawn form.
- Reason: the read model is already folded in memory per request; a pure
  function is testable without a database and the rules stay in one place.
  The busy rank keeps v0's behaviour (a busy session still takes a queued
  follow-up).
- Owner review: pending

## DL-203: Fast rounds and parallel lanes (owner decision)

- Context: the owner wants a usable M3 soon (2026-10-12) and will give
  feedback from real use.
- Choice (owner, 2026-10-12): each item gets one review; its findings go to
  a "Review follow-ups (after M3)" list for the iterations after M3. Normal
  rounds run only the format and lint checks; the full `make check` runs
  once at the end, in lane A. Four lanes build M3 at the same time; this is
  lane B (orchestration: 6-1, 6-3, 6-4, 6-2, 6-5).
- Owner review: approved (owner, 2026-10-12)

## DL-205: Fleet mail is deck-owned intent in the outbox (6-2)

- Context: PLAN §16 keeps fleet mail "through the durable outbox, delivered
  as native follow-up/steer". The M2 outbox (A5) stores routed command
  intent; it had no mail record that the board can list.
- Options: (a) post a `session.follow_up` directly from the board; (b) a
  deck `mail` table whose row and outbox item are inserted in one
  transaction (schema V6), the outbox payload being the framed command.
- Choice: (b). `POST /api/mail {mail_id, station, session_id, text,
  delivery?}` (`mail.send`, drive scope, audited) checks that the session is
  in the read model, stores the mail once (a resend with the same id
  returns the stored mail and queues nothing) and queues
  `[FLEETDECK MAIL from <sender>] <text>` as `session.follow_up` (default)
  or `session.steer`, origin `mail`, valid for 24 h. `GET /api/mail`
  (`mail.list`, read scope) shows the newest 200 with the delivery state of
  the outbox item. `assign auto` counts the undelivered mails of a session
  as "buried" (DL-204).
- Reason: the outbox already gives once-only delivery, station-reset retry
  and expiry; the mail row only adds the who/what for the board.
- Owner review: pending

## DL-206: Tasks with waitable completions (6-4)

- Context: PLAN §16 row "Tasks and waitable completions"; v0 had
  `complete_task` and a blocking wait in its coordinator tools.
- Choice: a deck `tasks` table (schema V6) with `open`, `done`, `failed`.
  `POST /api/tasks {task_id, title, station, session_id}` (`tasks.create`,
  drive) stores the task and its outbox item in one transaction and
  delivers `[FLEETDECK TASK <id>] <title>` plus a completion hint as a
  follow-up. `POST /api/tasks/{id}/complete {state, result?}`
  (`tasks.complete`, drive) ends an open task once; the same completion
  again returns the task, a different one is `409 task_ended`.
  `GET /api/tasks/{id}/wait?timeout_ms=` (`tasks.wait`, read) returns the
  task when it ends or at the deadline (default 30 s, at most 60 s).
- Reason: completion is first-writer-wins, so two reporters cannot flip a
  result. The wait polls the store every 100 ms: simple, and bounded by the
  60 s cap. The coordinator MCP (6-5) exposes the same calls to agents.
- Owner review: pending

## DL-207: Coordinator deck decision core (6-5, ADR-006 §4, §6.1–§6.4)

- Context: ADR-006 revision 4 (Accepted) defines grants, the binding
  check, the decision order, the tool/class matrix and the spawn-cap
  accounting. M2 stored every new `coordinator.call` as `unsupported`.
- Choice: `CoordinatorCall` takes the ADR §6.1 shape (`caller`, `binding`,
  `idempotency_key`, `op`; the M2 `token_digest` is gone). The ops are
  `spawn`, `send`, `interrupt`, `stop`, `assign`, `assign_auto` and
  `complete_task`. `RejectCode` gains `binding_stale` (never stored) and
  `spawn_cap_reached` (stored). Deck schema V8 adds the grant table `tokens`
  and `coordinator_lineage` (`crates/fleetdeck-deck/src/store/grants.rs`).
  `decide_coordinator` (`store/coordinator.rs`) follows §6.2 steps 1–6 in
  one transaction; the effects (`store/coordinator_ops.rs`) are deck-owned
  intent through the existing outbox (mail rows, task rows, `session.start`,
  `session.interrupt`, `session.stop`), so an accepted call and its intent
  commit together (A5). Spawn checks the grant stations, the family ceiling
  in `max_modes` (Claude permission-mode order; Codex approval policy ×
  sandbox, component-wise; an unknown mode fails closed), then
  `spawn_used < spawn_cap`, and increments in the same transaction.
- Deviation: `SessionSpec` has no grant field, so a spawn spec that names
  a grant has that key dropped at decode instead of an explicit refusal.
  No grant can reach a station from a coordinator either way; test
  `a_spawn_that_carries_a_grant_or_a_mode_above_max_mode_is_refused`
  asserts that the re-encoded op has no grant.
- Reason: the decision core is the authority boundary; it needs no station
  or provider change and is fully testable offline.
- Owner review: pending

## DL-208: Station coordinator ingress (6-5, ADR-006 §5.1–§5.3)

- Context: ADR-006 §5 defines a loopback MCP Streamable HTTP endpoint with
  limits before authentication and one 401 for every credential failure.
- Choice: `crates/fleetdeck-station/src/coordinator/` with `bindings.rs`
  (credential minting into `Secret<String>`, SHA-256 digest only, binding
  states, per-binding 8 concurrent / 20 rps), `tools.rs` (fixed catalog by
  class), `jsonrpc.rs` (`initialize` with the two protocol versions and the
  cached control skill, `ping`, `tools/list`, `tools/call`; pending binding
  → `not_bound`; tool outside the classes → refused without a forward) and
  `ingress.rs` (hyper http1 on `127.0.0.1:0`, connection limit with an
  immediate close, unauthenticated in-flight limit with an immediate 429,
  header and body time limits, 256 KiB body limit, Host/Origin/Content-Type
  checks, batch → 400, notification → 202). The forward to the deck is the
  `Forward` trait. The station crate now uses the workspace `hyper` and
  `hyper-util` dependencies (already used by the deck; no new dependency).
- Reason: the ingress is the station half of the trust boundary and has
  the executable negative tests of §8.1.
- Owner review: pending

## DL-209: 6-5 parts that stay open after round 2

- Context: the time box (16:20 CEST) does not allow the rest of ADR-006 in
  this lane run. Round 2 delivered the deck decision core (DL-207), the
  station ingress (DL-208), `coordinator.query` on the deck and the link,
  the `coordinator.bound`/`coordinator.ended` lifecycle events with their
  fold into the grant (tests 25c, 25d), the owner grant list and revoke
  (board and HTTP), and the provider argument builder of the handoff
  (`crates/fleetdeck-providers/src/coordinator.rs`, no credential in argv).
- Open (6-5 stays unchecked):
  - the start-path wiring of the handoff (mint, child env, binding on the
    first native id) and the secrecy tests of §8.2;
  - the `Forward` implementation over the link. The station does not know
    the deck's database incarnation today; it must learn it from the link
    hello before it can build an envelope. Then the station coordinator
    outbox (§6.5, §8.4);
  - the snapshot projection of the lifecycle state (test 25e);
  - `session.coordinator_renew` and `session.coordinator_revoke` (§7,
    26a–26e), and grant creation at an owner start with the control skill;
  - the live smoke (§8.5) that gates the `coordinator` capability.
- Choice: keep the capability false (`handoff_unverified`) as the ADR
  requires until §8.5 passes; `handoff()` answers that reason for every
  provider. No coordinator traffic can reach the deck without a binding, so
  the open parts fail closed.
- Owner review: pending

## DL-210: the station forwards coordinator tool calls over its deck link

- Context: DL-209 named the missing `Forward` over the link. The station
  did not know the deck's database incarnation, which every
  `coordinator.call` and `coordinator.query` envelope must target
  (ADR-004 §9.3, ADR-006 §6.1). The `hello` of both sides already carries
  the sender's incarnation, but `fleetdeck-link` dropped it after the
  handshake.
- Options: (a) a new link method that asks the deck for its incarnation;
  (b) a new field in `events.replay` params (`deny_unknown_fields`, breaks
  an N-1 station); (c) keep the peer's `hello` incarnation on `OpenLink`
  and `LiveLink` (`peer_incarnation`).
- Choice: (c). No wire change, no new round trip; ADR-004 §9.3 already
  says that each side learns the value to target from the `hello`.
- `crates/fleetdeck-station/src/coordinator/forward.rs` `LinkForward`:
  follows the current deck link (`StationLink.coordinator`, set in
  `link::serve`, cleared by serial when that link ends), maps a mutating
  tool to a `coordinator.call` envelope with a new random `command_id` and
  `retry_until` = now + 30 s, and a read tool to a `coordinator.query`
  envelope (deadline now + 25 s, `wait_task` now + 55 s). Without a deck
  link the call fails at once with `deck_unreachable`; the durable station
  coordinator outbox (§6.5) stays open (DL-209).
- Tests: `coordinator::forward::tests::*` (2) and
  `fleetdeck-deck::coordinator::a_station_forwards_tool_calls_to_the_deck_incarnation_of_its_link`
  (real link, stored receipt, query answer, stale binding, disconnect).
- Owner review: pending

## DL-211: the station node serves the coordinator endpoint; a grant adds `coordinate`; spawned sessions join the lineage

- Context: the ingress (DL-208) and the forwarder (DL-210) were not
  started by the station runtime, a paired station holds only `admin`
  (DL-050), so every real `coordinator.call` failed at the link layer
  with "needs scope coordinate", and a spawned session never got its
  session id into `coordinator_lineage` (round-2 review finding).
- Options for `coordinate`: (a) give every paired station `coordinate`
  at pairing; (b) a separate owner switch per station; (c) the owner's
  grant creation adds `coordinate` to the caller's station in the same
  transaction and refreshes the link's grant cache.
- Choice: (c). The owner's grant is the explicit owner action of
  ADR-006 §4; pairing alone still grants nothing (G5), and the decision
  transaction keeps checking both the scope and the grant.
  `DeckNode::create_grant` = `DeckStore::create_grant` + `DeckPeers::refresh`.
- `StationNode` starts the loopback `Ingress` with the `LinkForward` of
  its link context (`coordinator_url()`, `coordinator_bindings()`); an
  endpoint start failure is `NodeError::Coordinator`.
- Lineage: a lineage row with `session_id IS NULL` matches a target
  session when the receipt of its start command (outbox item → command
  → `receipt.result.session_id`) names it. No new column, no new write
  path.
- Tests: `fleetdeck-deck::deck_link::a_bound_coordinator_reaches_the_deck_through_the_station_endpoint`
  (real station node, real link, 401 without a credential, accepted
  `send_mail` stored as mail and receipt),
  `store::coordinator::tests::lifecycle::a_spawned_session_joins_the_lineage_when_its_start_receipt_names_it`.
- Owner review: pending

## DL-212: renew and revoke link methods (ADR-006 §6.1, §7, tests 26a–26e)

- Context: ADR-006 §6.1 adds `session.coordinator_renew` and
  `session.coordinator_revoke` (mutating, `drive`, deck → station,
  feature `coordinator_v1`). The repository names features `<name>.v1`
  (`revert.v1`, `auto_continue.v1`).
- Choice: feature `coordinator.v1` (the ADR's `coordinator_v1` in the
  repository's spelling; ADR-004 feature table, envelope table and §9.6
  rows added). `SessionCommand::CoordinatorRenew { session_id, grant_id,
  generation }` and `CoordinatorRevoke { session_id, grant_id }`.
- Station (`crates/fleetdeck-station/src/manager/coordinator_command.rs`):
  the station-local socket is refused before the decision (`guard`), and a
  non-board origin is refused in it (`forbidden`, stored). A binding of
  another session or grant is a stored `invalid_command`. Revoke marks the
  binding `revoked` and appends `coordinator.ended { reason: "revoked" }`
  once; renew needs an idle-`expired` binding at `generation - 1`, makes
  it `active` at `generation` and appends `coordinator.bound`. The
  bindings now live in the station `Policy` (`coordinator_bindings()`),
  shared with the `StationNode` ingress.
- Deck: the owner's revoke route (`POST /api/coordinator/grants/{id}/revoke`)
  commits `revoked` first and then queues `session.coordinator_revoke` in
  the outbox for the grant's station (a second revoke queues nothing).
  Board renewal (§7, test 25b) stays open.
- Deviation: the deck's revoke outbox item is a separate transaction
  after the grant update (follow-up: one transaction, A5). The station's
  in-memory binding changes inside the decision closure, before the
  commit; a failed commit can leave a binding revoked (fail closed) or
  renewed without its event (follow-up).
- Tests: `fleetdeck-link methods::tests::coordinator_renew_and_revoke_from_the_wrong_role_without_the_feature_or_without_drive_are_refused` (26b);
  26a through `adr_revision2::every_session_command_is_a_mutating_deck_to_station_method_of_the_adr_table`,
  `method_table::the_code_method_table_equals_the_adr_method_table`,
  `mutation::tests::session_methods_are_exactly_the_session_command_methods`
  and `http::methods::tests::every_command_method_has_the_scope_of_its_link_method`;
  `manager::tests::coordinator_command::*` (26c, 26d, 26e).
- Owner review: pending

## DL-213: the owner's renewal waits for the station's matching `coordinator.bound` (ADR-006 §5.5, test 25b)

- Context: ADR-006 test 25b requires that a renewal needs an idle-expired
  binding and the matching station acknowledgment. Before this entry the
  deck accepted any higher generation that a station reported (review
  follow-up of round 2), and no owner renewal route existed.
- Options: (a) the deck sets the grant active at the owner's renew and
  trusts the station; (b) the deck records a pending renewal generation
  and activates the grant only when the station's `coordinator.bound`
  carries exactly that generation.
- Choice: (b). Deck schema V10 adds `tokens.renew_generation`.
  `DeckStore::renew_grant` accepts only an `expired`/`idle` grant inside
  its lifetime with a binding, and records `bound_generation + 1` (a repeat
  returns the same value). `record_lifecycle` and `bind_grant` accept a
  first binding of an active grant, or a higher generation only when it
  equals the pending renewal; the acknowledgment sets `active`, clears the
  reason and consumes the renewal. A revoked grant never renews.
  `POST /api/coordinator/grants/{grant_id}/renew` (`coordinator.renew`,
  drive scope, audited; 409 `not_renewable`, 404 `unknown_grant`) queues
  `session.coordinator_renew` with the generation; the board shows
  "Renew" on an idle-expired grant (`web/src/fleet/FleetPanel.tsx`).
- Reason: fail closed (PLAN §3): the deck never extends authority on a
  station claim alone, and never activates a grant that the station did not
  rebind. This also closes the round-2 follow-up "unsolicited generation
  increase".
- Deviation: the renewal and its outbox item are two transactions, as for
  revoke (DL-212 follow-up).
- Tests: `store::coordinator::tests::lifecycle::renewal_needs_an_idle_expired_binding_and_the_matching_station_acknowledgment`,
  `board_fleet::the_owner_lists_and_revokes_a_coordinator_grant_and_a_viewer_cannot_revoke`
  (renew: 403 viewer, 409 active grant, 404 unknown),
  `web/src/fleet/fleet.test.ts` "a renew posts to the grant and a refusal says so (ADR-006 25b)".
- Owner review: pending

## DL-214: `coordinator.call` and `coordinator.query` need the `coordinator.v1` feature (ADR-006 §6.1, test 26)

- Context: ADR-006 §6.1 says that a deck accepts the coordinator shapes
  only on a link that negotiated `coordinator_v1`, and that an M2 peer
  keeps the M2 behavior. The link method table gated only
  `session.coordinator_renew`/`session.coordinator_revoke`;
  `coordinator.call` and `coordinator.query` had no feature.
- Options: (a) keep them ungated and rely on the `coordinate` scope;
  (b) gate both with `coordinator.v1` in the code table and the ADR-004
  §9.6 table.
- Choice: (b). Both builds offer `coordinator.v1` (`feature::ALL`), so
  paired M3 peers are unchanged; a peer without the feature gets
  `feature_not_negotiated` before the scope check, as for every other
  gated method.
- Reason: the ADR text is binding; fail closed on links that do not know
  the coordinator shapes.
- Tests: `fleetdeck-link methods::tests::a_peer_without_the_coordinator_feature_gets_the_m2_behavior`,
  `method_table::the_code_method_table_equals_the_adr_method_table`,
  `method_table::a_changed_adr_row_breaks_the_agreement` (updated rows).
- Owner review: pending

## DL-215: Coordinator spec on `session.start`, start checks in the station decision, and the handoff value (ADR-006 §4, §5.4, §8.2 tests 10–13)

- Context: ADR-006 §4 adds `SessionSpec.coordinator`. §8.2 test 12 says
  that the deck refuses a spec `unsupported` while the provider handoff is
  unverified; the deck does not know the provider version, the station
  does (`station.info` probe), and the handoff answer
  (`fleetdeck_providers::coordinator::handoff`) lives next to the adapter.
- Options: (a) check only in the deck with the capability of the station
  snapshot; (b) check in the station decision, so the refusal is a stored
  receipt that the deck relays; (c) both.
- Choice: (b) now, (a) as a follow-up for an earlier board message.
  `SessionSpec.coordinator: Option<Box<CoordinatorSpec { grant_id, classes,
  idle_ttl_ms, expires_at, control_skill: ControlSkill { version, text } }>>`
  (boxed for clippy `large_enum_variant` in `CoordinatorOp::Spawn`;
  `serde(default, skip_serializing_if)`, so M2 peers and stored specs
  decode unchanged). The station decision refuses a spec:
  - from the station-local socket (`invalid_command`, caller guard and
    origin `local`, test 13);
  - from a coordinator or mail origin (`forbidden`, ADR-006 §4: a
    coordinator `spawn` never passes a grant);
  - on an import or with an incomplete spec (`invalid_command`);
  - for a provider without a verified handoff (`unsupported`, message
    `handoff_unverified`, test 12).
  `fleetdeck_station::coordinator::Handoff` mints the credential
  (`Secret<String>`), keeps its digest for the binding and puts the raw
  value only into the child environment (`FLEETDECK_COORDINATOR_TOKEN`).
- Reason: the station owns the child and its provider version; a stored
  rejection is the doctrine path for refused commands (PLAN §3).
- Not yet: the start effect does not call `Handoff` yet, because no
  provider passes the handoff check until the §8.5 live smoke records a
  version; the wiring into the adapter start is the next 6-5 slice.
- Tests: `fleetdeck-cli coordinator_secret::the_coordinator_credential_reaches_the_child_only_through_its_environment` (10, real child, `/proc` argv and environ),
  `coordinator_secret::each_provider_handoff_names_the_variable_and_needs_a_loopback_endpoint` (11),
  `fleetdeck-station manager::coordinator_spec_tests::a_coordinator_spec_for_an_unverified_handoff_is_refused_unsupported` (12),
  `manager::coordinator_spec_tests::only_an_owner_start_may_carry_a_complete_coordinator_spec`,
  `manager::tests::a_station_local_start_with_a_coordinator_spec_is_rejected` (13).
- Owner review: pending

## DL-216: Coordinator handoff in the start effect (ADR-006 §5.4)

- Context: DL-215 added the coordinator spec and the `Handoff` value, but
  the start effect did not use it. The ingress URL is known only after the
  link runtime starts the ingress; the adapter trait had no way to pass
  extra arguments and a credential for one start.
- Options: (a) put the credential into `SessionSpec` (no: the spec is
  stored in effects and receipts); (b) a new adapter method
  `start_coordinated(spec, sink, CoordinatorLaunch)` with a refusing
  default, and the station prepares the launch in the start effect.
- Choice: (b).
  - `fleetdeck_providers::coordinator::CoordinatorLaunch { args, token:
    Secret<String> }`; `ProviderAdapter::start_coordinated` defaults to
    `ProviderError::CoordinatorHandoff("handoff_unverified")`. The Claude
    adapter implements it (arguments after the configured extra
    arguments, credential only in the child `ChildEnv`). The Codex adapter
    keeps the refusing default (follow-up).
  - `Bindings` keeps the ingress URL (`set_endpoint`, set by the link
    runtime after `Ingress::start`) and the handoff check
    (`Bindings::handoff`, the provider answer; station tests may set a
    verified check).
  - `manager::coordinator_start::prepare` rechecks the handoff and the
    endpoint, refuses a resume and a shim-hosted start, mints the
    credential and inserts a `pending` binding (generation 0, binding id
    `<session>:<grant>`) under its digest. A refused prepare fails the
    start (`session.ended`); an adapter start failure revokes the binding.
- Reason: the credential never enters a stored record; fail closed when any
  piece of the handoff is missing (PLAN §3).
- Tests: `fleetdeck-station manager::coordinator_start_tests::a_deck_start_with_a_grant_hands_the_credential_to_the_child_environment_only`
  (real fake-Claude child: argv names the variable and holds no credential,
  the child environment holds it, the pending binding has its digest, and
  no byte of the station database or WAL holds it),
  `manager::coordinator_start_tests::the_start_effect_rechecks_the_handoff_and_the_endpoint`.
- Owner review: pending

## DL-217 — Owner coordinator start on the deck and the station binding lifecycle (6-5)

- Date: 2026-10-12 (lane B round 9)
- Context: ADR-006 §4 says the owner creates the grant as part of a
  session start, and §7.1 says the deck puts its control skill into
  `CoordinatorSpec.control_skill`. The station assigns the session id in
  its decision, so the deck cannot know the session of the grant when it
  creates it. The station bindings stayed `pending` (DL-216 follow-up),
  and a failed `process_started` did not revoke the binding (round 8
  review).
- Options for the session id: (a) the deck chooses the session id (no:
  the station owns the id, M2); (b) create the grant with an empty
  session id and its start `command_id`, and let the first
  `coordinator.bound` name the session when the deck's receipt of that
  start names no other session; (c) create the grant only after the
  receipt (no: the credential must be in the child environment at
  process start, §5.4).
- Choice: (b).
  - Deck schema V11: `tokens.start_command_id` (unique when set).
    `NewGrant.start_command_id`; `create_grant` returns the existing grant
    for the same start command (a resend creates no second grant).
  - `POST /api/commands/session.start` with `spec.coordinator`:
    `http::grant_routes::owner_start` creates the grant (classes from the
    request, unknown → 400 `invalid_grant`; `idle_ttl_ms`/`expires_at` 0 →
    deck defaults), refreshes the link grants, and replaces the board's
    grant id, classes, expiry and control skill with the deck's
    (`CONTROL_SKILL_VERSION` "1", `CONTROL_SKILL` from
    `crates/fleetdeck-deck/src/control_skill.md`).
  - `record_lifecycle`: a grant with an empty session id accepts the first
    `coordinator.bound` of its station when the start receipt names no
    session or this session, and stores that session id.
  - Station: `session.started` of a session binds each binding of the
    session (`Bindings::session_native`): `pending` → `active` generation 1
    with `coordinator.bound`; another root identity → `revoked` with
    `coordinator.ended`. The pump revokes every binding at the process end
    (`Bindings::end_session`, `coordinator.ended` reason `process_exit`).
    A failed `process_started` revokes the session's bindings.
  - Board: spawn form toggle "Coordinator" (classes read, mail, drive;
    deck defaults for TTLs); not sent with an import.
- Reason: the station keeps its session ids; the station binds only the
  grant id that the deck put into the start, and the deck still checks the
  station and the start receipt (PLAN §3: the deck is the authority).
- Tests: `fleetdeck-deck::http_commands::an_owner_coordinator_start_creates_one_grant_and_sends_its_id_and_the_control_skill`,
  `fleetdeck-station manager::coordinator_start_tests::a_deck_start_with_a_grant_hands_the_credential_to_the_child_environment_only`
  (extended: `coordinator.bound` generation 1 with the native id, then
  `coordinator.ended` `process_exit` after a stop),
  `manager::tests::coordinator_command::*` (helper binds with the real
  native id), `store::schema::tests::the_migrations_are_valid`,
  `web/src/spawn/spawn.test.tsx` coordinator toggle test.
- Owner review: pending

## DL-218: Durable station coordinator outbox (M3 6-5, ADR-006 §6.5, §8.4)

- Context: until now a coordinator tool call went straight over the deck link and failed at once with `deck_unreachable` when no deck was connected (DL-209). ADR-006 §6.5 requires a durable station outbox with G3 states, a 15 min `retry_until`, `sent_at` before the network write, idempotency keys and the fence.
- Options: (a) reuse the station effect outbox (`effects`), whose rows target a provider process; (b) a separate `coordinator_outbox` table with its own worker; (c) keep the direct forward and retry in memory.
- Choice: (b). Station schema V17 `coordinator_outbox` (envelope stored once, `state` in `pending|done|failed|ambiguous`, `sends`, `sent_at`, unique `(binding_id, idempotency_key)`). `store::coordinator_outbox` holds the state rules; `coordinator::outbox` holds the worker: intake commits the row before any send; every transmission checks the fence (the binding must be the active binding of the row's generation), commits `sent_at`, then sends the stored bytes; `LinkForward::flush` ends rows past `retry_until` and resends the rest; the station node runs it every 2 s. A receipt answer of `incarnation_mismatch`, `expired`, `forbidden` or `binding_stale` is `failed` on the first transmission and `ambiguous` when an earlier transmission exists. Station start recovery ends every `pending` row (`station_restart`; sent → `ambiguous`, never sent → `failed`), because no binding survives a station restart without a durable host. `CALL_RETRY` is now 15 min. The deck link is behind a `Transport` trait so that the tests use a fake deck.
- Reason: (a) mixes process effects with deck mutations and has other terminal states; (c) loses calls on a crash. (b) keeps one envelope per command id, so the deck deduplication of §6.2 step 4 applies to every resend.
- Deviations: the lifecycle watermark of §6.1 is enforced through the local fence only: a binding that the station invalidated never transmits again, so no resend can pass the `coordinator.ended` event; the station does not wait for the deck acknowledgment of that event before sending other bindings' rows. The tool returns after the first transmission (`pending` without a link) instead of waiting up to 30 s for the receipt. An intake needs the incarnation of a deck that was connected at least once since the station started (else `deck_unreachable`). Follow-ups in the lane log.
- Tests: `coordinator::outbox_tests::*` (27, 28, 28a, 28b, 28c, 29, 30, 30a of ADR-006 §8.4), `coordinator::forward::tests::*` (`retry_until` = intake + 15 min).
- Owner review: pending

## DL-219: Coordinator snapshot projection and the live handoff smoke (M3 6-5, ADR-006 §6.1, §8.3 25e, §8.5)

- Context: a forced snapshot skips coordinator lifecycle events, so the deck must learn binding facts from the snapshot (ADR-006 §6.1). The capability stays off until the opt-in live smoke records a passing handoff (§5.4, §8.5).
- Options for the projection: (a) a new control frame; (b) a field on `SessionSnapshot`; (c) an optional field on `snapshot_end`. Options for the gate: (i) keep it off; (ii) enable the verified provider family from its probed version.
- Choice: (c) and (ii).
  - Proto: `ControlFrame::SnapshotEnd.coordinator: Option<Vec<CoordinatorBindingState>>` (`binding_id`, `grant_id`, `session_id`, `generation`, `provider_instance`, `native_id`, `state` `pending|active|expired|revoked`; no secret). Older peers ignore the field; it is omitted when `None`.
  - Station: a link with `coordinator.v1` makes its stream attach `Bindings::projection()` (read right after the snapshot summaries) to each snapshot end (`link::stream::Stream::with_coordinator`).
  - Deck: `DeckStore::apply_snapshot_with` applies the projection in the snapshot transaction, with the summaries and the cursor, before the link acknowledges. The deck takes a projection only from a link with `coordinator.v1`. `None` (ordinary summary snapshot) changes no grant. `grants::apply_projection` uses the event rules (`apply_bound`, `apply_ended`, shared with `record_lifecycle`): pending → nothing; active → the `coordinator.bound` rules (generation 1 only for a grant that awaits its first binding; a higher generation only for the recorded renewal; never a revoked grant); expired/revoked → the bound rules, then the `coordinator.ended` rules; a station-revoked binding of a never-bound grant revokes the grant (fail closed).
  - Live smoke: `crates/fleetdeck-providers/tests/live_smoke.rs` `claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff` (opt-in, `#[ignore]`, `FD_LIVE_SMOKE=1`). A loopback MCP probe records the SHA-256 of each `Authorization` header. Claude starts through `start_coordinated` (the real adapter path), discovers the probe tool, calls it once and repeats its answer; the credential is in no process command line and in no Claude settings file (`~/.claude.json`, `~/.claude/settings*.json`, the temporary directory).
  - Gate: `fleetdeck_providers::coordinator::VERIFIED = [(Claude, "2.1.296")]`; `handoff()` is `Ok` for a dotted version at or above that minimum. The station records the probed version of each provider instance (`Bindings::record_version`, from `SessionManager::providers`, which the station node now runs once at start). A start passes only when every probed instance of the family is verified; an unprobed family stays `handoff_unverified`.
- Deviations:
  - The live smoke cannot use `--safe-mode`, because it also drops `--mcp-config` servers. It uses `--strict-mcp-config --disable-slash-commands --allowedTools mcp__fleetdeck__fleet_ping`. `--setting-sources project` lost the sign-in ("OAuth session expired"), so the user settings load.
  - The station half of test 31 (tool discovery while the binding is pending, then the native binding) is covered offline by `a_deck_start_with_a_grant_hands_the_credential_to_the_child_environment_only`, not live.
  - Codex has no `start_coordinated` yet, so its handoff stays unverified (the live test prints this). Its Codex half is a follow-up.
  - The station projection is taken in memory after the summaries are read, not at the exact watermark. A later event replays through the same rules, so the result can only be more restrictive.
  - The board does not hide the "Coordinator" toggle by capability yet; the deck/station refusal (`unsupported`, `handoff_unverified`) stays the enforcement.
- Tests: `fleetdeck-deck store::coordinator::tests::lifecycle::a_snapshot_recovery_with_an_unapplied_lifecycle_event_commits_the_grant_transition_before_any_coordinator_traffic_resumes` (25e deck side), `fleetdeck-station::link_service::a_forced_snapshot_carries_the_coordinator_binding_projection_without_the_credential` (25e station side), `fleetdeck-providers coordinator::tests::the_capability_stays_off_until_the_live_smoke_passes`, `fleetdeck-station manager::coordinator_spec_tests::the_handoff_check_needs_a_verified_probed_version_of_every_instance`, live: test 31 passed once (Claude 2.1.296, 34 s).
- Owner review: pending

## DL-220: Live test 31 through the station for both providers, and the station lifecycle fence (M3 6-5, ADR-006 §5.1, §6.1, §8.3 25e, §8.5)

- Context: the review of round 11 asked for test 31 for both providers through the station pending-to-active binding, and for the station half of the 25e fence (no coordinator traffic before the deck acknowledges the lifecycle). Running the station path live found two real gaps: Claude 2.1.296 sends MCP `protocolVersion` `2025-11-25`, which the ingress refused (ADR-006 §5.1 lists only `2025-06-18` and `2025-03-26`), so Claude saw no tool; Codex 0.160.0 asks a native approval for every MCP tool call, which `never` refuses.
- Options: protocol: (a) keep the ADR list (Claude never connects); (b) add `2025-11-25`; (c) answer every unknown version with the newest supported one. Native approval: (i) leave it (every coordinator call becomes an owner approval, and fails under `never`); (ii) the handoff arguments declare the `fleetdeck` server's tools approved. Fence: (x) count only the local binding fence (DL-218); (y) hold every call and query of a link until the deck acknowledges the station sequence of the last lifecycle event.
- Choice: (b), (ii) and (y).
  - `coordinator::jsonrpc::PROTOCOL_VERSIONS = ["2025-11-25", "2025-06-18", "2025-03-26"]`; another value still gets the JSON-RPC error. The station uses only `initialize`, `ping`, `tools/list` and `tools/call`, which `2025-11-25` does not change.
  - `fleetdeck_providers::coordinator::mcp_args`: Claude adds `--allowedTools mcp__fleetdeck`; Codex adds `-c mcp_servers.fleetdeck.default_tools_approval_mode="approve"`. The owner-authorized grant is the authority (the station checks the classes, the deck the scope and target), so a second native approval adds nothing; other tools keep their approval rules.
  - Codex `CodexAdapter::start_coordinated` (`start_session_with`): the `-c` overrides follow `app-server`, the credential goes only into the child environment (`CoordinatorLaunch::env_for`, shared with Claude). The Codex poll task moved to `codex/driver_poll.rs` (module size).
  - `VERIFIED = [(Claude, "2.1.296"), (Codex, "0.160.0")]`.
  - Fence: `Store::coordinator_lifecycle_seq` (the last `coordinator.bound`/`coordinator.ended` station sequence); each deck link records its highest valid `ack` (`LinkForward::acknowledged`, from `link::serve`); `LinkForward::fenced` is true while that ack is below the watermark. A query then answers `deck_syncing`; an outbox row stays `pending` and a later flush sends it. A new link starts at 0. On a `coordinator.v1` link every snapshot carries the projection, so its acknowledgment opens the fence; an ordinary snapshot never reaches a coordinator link.
- Reason: (a) and (i) make the coordinator unusable with the verified CLIs; (c) answers versions that were never tested. (y) is the ADR-006 §6.5 rule; (x) let other bindings' rows leave before the deck knew the lifecycle.
- Deviations: ADR-006 §5.1 protocol list extended by `2025-11-25` (owner review). The live station test uses a recording forward in place of a deck (the deck half of a call is covered offline by `deck_link::a_bound_coordinator_reaches_the_deck_through_the_station_endpoint`). The live tests use the user settings of each CLI (Claude: no `--safe-mode`, see DL-219; Codex: mode `never/read-only` in a temporary directory).
- Tests: live (opt-in, passed once each): `fleetdeck-station manager::coordinator_live_tests::a_live_claude_coordinator_binds_and_calls_the_station_ingress_once` and `a_live_codex_coordinator_binds_and_calls_the_station_ingress_once` (pending binding, active generation 1 with the native id of `session.started`, `coordinator.bound` in the log, exactly one forwarded call with that binding, the answer in the transcript, revocation at stop, the credential in no command line, station store file or provider settings file); `fleetdeck-providers live_smoke::claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff` (both providers, exactly one `tools/call`). Offline: `coordinator::outbox_tests::a_reconnect_holds_calls_and_queries_until_the_deck_acknowledges_the_lifecycle_watermark`, `coordinator::tests::the_capability_stays_off_until_the_live_smoke_passes`, `coordinator::tests::the_provider_arguments_name_the_variable_never_a_credential_and_need_a_loopback_url`, `manager::coordinator_spec_tests::the_handoff_check_needs_a_verified_probed_version_of_every_instance`.
- Owner review: pending
