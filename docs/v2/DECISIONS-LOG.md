# Fleet Deck v2 — decisions log

This log records each deviation from PLAN.md and each choice that the plan does not state. The goal loop runs without owner access, so each entry has "owner review: pending" until the owner accepts or changes it.

Format: context, options, choice, reason, owner review.

---

## DL-001 — Cargo workspace in Phase 0 holds only the spike crates

- **Context:** PLAN §12 places the workspace `Cargo.toml` at the repository root with the product crates under `crates/`. Phase 1 creates those crates. The M1 checks (`cargo fmt --all --check`, `cargo clippy --workspace`, `cargo nextest run --workspace`, `cargo deny check`) must run on the spike crates in Phase 0.
- **Options:** (a) a separate workspace in `spikes/Cargo.toml`; (b) the root workspace now, with members `spikes/*` only.
- **Choice:** (b). The root `Cargo.toml` lists only `spikes/*` members in M1. Phase 1 adds `crates/*` and decides if the spikes stay members or move to `exclude`.
- **Reason:** the checks in the goal run from the repository root. One workspace gives one `Cargo.lock`, one `deny.toml`, and one lint policy that Phase 1 reuses.
- **Owner review:** pending

## DL-002 — MSRV pin without a `rust-toolchain.toml`

- **Context:** PLAN §12 asks for "stable, edition 2024, pinned MSRV". The installed stable toolchain is 1.96.0. A `rust-toolchain.toml` with `1.96` makes rustup install a second named toolchain (network access, a slow first build).
- **Options:** (a) `rust-toolchain.toml` with `channel = "1.96"`; (b) `rust-version = "1.96"` in `[workspace.package]` only.
- **Choice:** (b) for M1. Phase 1 CI can add the toolchain file when CI installs toolchains anyway.
- **Reason:** `rust-version` pins the MSRV for cargo (it rejects an older compiler), and the local checks run on the installed stable 1.96.0 without a download.
- **Owner review:** pending

## DL-003 — Claude permission mode names differ from PLAN §6.2

- **Context:** PLAN §6.2 lists the Claude modes `default` / `acceptEdits` / `plan` / `bypassPermissions`. `claude --help` of 2.1.289 lists `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`, `plan`. There is no `default`.
- **Options:** (a) keep `default` and translate it; (b) use the native names of the installed CLI.
- **Choice:** (b). The S1 adapter offers exactly the native names (L8) and uses the most restrictive mode for an unknown stored mode. The capability matrix records the verified names. No PLAN decision changes: only the matrix value column changes.
- **Update (round 2):** live runs show that the wire name differs from the flag name. `--permission-mode manual` makes `system/init` report `"permissionMode":"default"`, and `--permission-mode default` is accepted as a hidden alias. The adapter therefore uses the wire names `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions` (one name for one meaning in events, the matrix, and `set_permission_mode`), and `default` is the safest mode. Evidence: fixture `spikes/s1-claude/fixtures/rust/set_permission_mode.jsonl`, test `adapter::tests::set_mode_accepts_only_native_modes`.
- **Reason:** L8 ("offer only the modes that the provider enforces natively"). A translated sentinel name is trap 1/2 of t3code-lessons §5.
- **Owner review:** pending

## DL-004 — The driven Claude session always passes `--permission-mode`

- **Context:** on this machine the user's Claude settings make `bypassPermissions` the default mode (a first live probe showed `"permissionMode":"bypassPermissions"` in `system/init`). A driven session that passes no mode inherits it.
- **Options:** (a) inherit the user's default; (b) always pass an explicit native mode, `manual` when none is stored.
- **Choice:** (b). `cli_args()` always adds `--permission-mode`.
- **Reason:** the goal forbids bypass mode outside a temporary directory, and L8 requires the most restrictive mode for an unknown stored mode. A station must not run with a mode that the owner did not choose.
- **Owner review:** pending

## DL-005 — Live recordings run with `--safe-mode`

- **Context:** the first live probe showed that the user's global Claude setup (v0 Fleet Deck hooks, plugins, `CLAUDE.md`, MCP servers) runs inside a driven session and adds `system/hook_*` frames and local paths to the output.
- **Options:** (a) record with the user's setup; (b) `--setting-sources` with a reduced set; (c) `--safe-mode` (no hooks, plugins, `CLAUDE.md`, MCP; authentication still works).
- **Choice:** (c) for `record-claude`. The scrubber also replaces the work directory, `$HOME`, and `$USER`, and redacts `account`, `email`, `organization`, and `messaging_socket_path`.
- **Reason:** fixtures must be reproducible on another machine and must not contain the owner's local setup. The product adapter (Phase 2) decides its own flags; `--safe-mode` is only a recorder choice.
- **Owner review:** pending

## DL-006 — "Prompt confirmed" for Claude is the `command_lifecycle` `started` frame

- **Context:** PLAN §6.4 needs a provider signal that moves an input from `claimed` to `confirmed`. A first adapter version started a turn on any `system/status` or stream frame; the round-1 review found that an unrelated frame could confirm a pending prompt.
- **Options:** (a) the first stream frame after a write; (b) the `isReplay` user echo of `--replay-user-messages`; (c) the `command_lifecycle` frame (`queued` → `started` → `completed`/`cancelled`/`discarded`/`refused`) that `claude` 2.1.289 emits for an input that carries a client `uuid`.
- **Choice:** (c). Each host input carries a client UUID; `started` with the same `command_uuid` confirms it and binds it to a turn. (b) stays only as a fallback for a CLI that sends no lifecycle frames. A turn that the CLI starts on its own (for example after a background task, trap 4) gets `origin: provider` and never confirms an input.
- **Reason:** (c) is the only signal that names the input. Evidence: fixtures `queued_turn`, `message_steering`, `turn_interrupt`; tests `adapter::tests::unrelated_frames_never_confirm_a_pending_prompt`, `provider_turn_does_not_confirm_a_pending_prompt`, `only_the_matching_uuid_confirms`.
- **Owner review:** pending

## DL-007 — Permission answers echo `toolUseID`, as the TypeScript SDK does

- **Context:** the SDK reference recordings (S1-3, `spikes/s1-claude/fixtures/sdk/`) show that `@anthropic-ai/claude-agent-sdk` 0.3.296 adds `toolUseID` (the `tool_use_id` of the `can_use_tool` request) to every permission answer. Our first adapter did not send it. The CLI accepted both forms in the recordings.
- **Options:** (a) keep the shorter answer; (b) send `toolUseID` when the request has a `tool_use_id`.
- **Choice:** (b). `control::permission_response` takes the request's `tool_use_id`. The Rust fixtures with approvals were re-recorded with the new frame.
- **Reason:** A8/D8 make the SDK the reference for the wire protocol. The field binds the answer to one tool call, so a late answer cannot apply to a different call. The deny answer keeps `interrupt: false`; the SDK omits it, and the CLI default is `false` (the reference replay treats both as equal).
- **Owner review:** pending

## DL-008 — SDK reference replay compares writes semantically

- **Context:** PLAN §14 S1 asks for "TS SDK frame recording + replay". SDK frames differ from ours in fields that do not change the CLI's behaviour: random request ids, `systemPrompt: [""]` in `initialize`, and no `uuid`/`session_id` on user messages.
- **Options:** (a) exact comparison (fails on every frame); (b) a semantic comparison of the behaviour-relevant fields, with the key differences reported as evidence.
- **Choice:** (b). `spikes/s1-claude/src/reference.rs` infers the host command from each SDK write, re-addresses CLI responses to the core's request ids, compares the projection of `reference::semantic`, and lists one-sided keys (`replay-claude --keys`).
- **Reason:** the replay must prove that the Rust core drives the CLI the same way as the reference client and handles every frame that the reference client receives (0 unknown frames). Exact bytes of ids are not part of the contract.
- **Consequence (evidence for DL-006):** the SDK sends no `uuid`, so the CLI sends no `command_lifecycle` frames and the SDK stream has no per-input confirmation. In the reference replay every turn is a provider-origin turn and no input reaches `started` (`tests/reference_fixtures.rs::sdk_prompts_are_never_confirmed`). The station adapter must send a `uuid` on every user message.
- **Owner review:** pending

## DL-009 — `allow_always` sends session-scoped allow rules only

- **Context:** a `can_use_tool` request carries `permission_suggestions` (fixture `rust/tool_call_approve.jsonl`): an `addRules` entry with `destination: "localSettings"`, an `addDirectories` entry, and a `setMode` entry (`acceptEdits`). The first adapter echoed all of them as `updatedPermissions` for `allow_always`. That writes `.claude/settings.local.json` into the owner's workspace, widens the workspace, and switches the session mode, all from one click. PLAN §11 lists "allow once / allow always / deny" but does not define the scope of "always".
- **Options:** (a) echo the suggestions unchanged; (b) send only the allow rules, re-scoped to `destination: "session"`; (c) no `allow_always` for Claude.
- **Choice:** (b). `adapter::map::session_rules` keeps `addRules`/`replaceRules` entries with `behavior: "allow"` and sets `destination: "session"`. It drops `setMode` (L8: a mode changes only through `set_permission_mode`) and `addDirectories`. Without an allow rule the request offers no `allow_always`. The option label is "Allow always (this session)".
- **Reason:** doctrine of least surprise and fail-closed permissions: a click in the deck must not change files in the workspace or the session mode without an explicit owner action. A persistent rule is a later, explicit feature (owner decision).
- **Owner review:** pending

## DL-010 — Two more `approval.expired` reasons: `turn_ended` and `cancelled_by_provider`

- **Context:** PLAN §6.3 lists the reasons `not_resumable` and `process_lost` for `approval.expired`. The Claude CLI also ends an open request in two other ways: it sends `control_cancel_request` for a request id, and a turn can end (`result`) while a request is still open (for example after an interrupt).
- **Options:** (a) map both to `process_lost`; (b) leave the request open; (c) add the reasons `cancelled_by_provider` and `turn_ended`.
- **Choice:** (c), in the spike canonical events. ADR-001 §2.4 and §2.9 document them. Phase 1 adds them to the `approval.expired` reason enum of the protocol crate.
- **Reason:** (a) gives a false cause in the timeline. (b) leaves an answerable card for a request that the CLI no longer waits for, and an answer to it would be a stale reply. A precise reason is consistent with L2 (show the real state).
- **Owner review:** pending

## DL-011 — Spike event `background.updated` for provider background work

- **Context:** PLAN §6.3 reports background work only as `run.completed { background_pending }` (L7). The Claude CLI reports the background set while a turn runs and after it (`system/background_tasks_changed`), and a background task or an async subagent can start a new turn without host input (traps 4 and 5).
- **Options:** (a) report only `background_pending` at turn end; (b) a spike event `background.updated { tasks }` each time the set changes.
- **Choice:** (b) in the spike only. The spike has no `run` entity; Phase 1 derives `run.completed.background_pending` from the same set and decides whether `background.updated` becomes a protocol event (an additive change).
- **Reason:** the evidence must show when the set drains (after the turn), which a turn-end flag cannot show. No PLAN decision changes.
- **Owner review:** pending

## DL-012 — ADR-002 protocol source: the vendored generated JSON schema, not a git pin and not full codegen

- **Context:** PLAN §14 S2 asks ADR-002 to choose "depend on `codex-app-server-protocol` (git pin) **or** codegen from `codex app-server generate-json-schema`". Facts (round 6): the crate at tag `rust-v0.160.0` (79b1b666) depends on about 15 internal monorepo crates (`codex-protocol`, `codex-rollout`, `codex-secrets`, `codex-shell-command`, `rmcp`, `zstd`, ...), so a git pin pulls a large part of the Codex workspace into `Cargo.lock` and `cargo deny`. The crates.io release of the crate is stale (0.63.0). `generate-json-schema --experimental` of codex-cli 0.160.0 writes one 864 KB draft-07 bundle (167 client requests, 11 server requests, 83 notifications). The only maintained Rust generator for JSON Schema, `typify`, is at 0.10.0-alpha.
- **Options:** (a) git pin of the protocol crate; (b) full Rust codegen from the schema with `typify`; (c) vendor the generated schema of the pinned CLI version as the source of truth, hand-write serde access for the subset that the adapter uses, and test the subset against the schema.
- **Choice:** (c), a narrow form of the PLAN's "codegen" option. `spikes/s2-codex/schema/0.160.0/codex_app_server_protocol.schemas.json` is vendored unchanged. `tests/schema_methods.rs` checks that every method that the core sends or matches exists in the schema. S2-3 adds a conformance test that validates recorded frames and host writes against the schema. A CLI upgrade regenerates the bundle into a new version directory, and the tests show what changed.
- **Reason:** (a) adds a heavy, unpublished dependency tree for about 20 message shapes; (b) depends on an alpha generator and produces hundreds of unused types. (c) keeps the provider-native truth (doctrine: native protocol, no wrapper) with no new runtime dependency, and an upgrade is a diff of one generated file.
- **Owner review:** pending

## DL-013 — Codex sessions use the experimental API; mode names are `<approvalPolicy>/<sandbox>`

- **Context:** `item/tool/requestUserInput` (the RPC-reply question channel of L4) and `collaborationMode` exist only when `initialize` sets `capabilities.experimentalApi: true`. PLAN §6.2 names the Codex modes "`approvalPolicy` × `sandbox` per turn" without a wire name.
- **Options:** (a) stable API only (no RPC questions; questions only as async `agentMessage` questions); (b) experimental API on, with the stable subset used for everything else.
- **Choice:** (b) for the spike. The core names a mode `<approvalPolicy>/<sandbox>` (for example `untrusted/workspace-write`) with only the native values of the schema (L8). A new session with no stored mode uses `untrusted/workspace-write`; an unknown stored mode (or a sentinel such as `default`, trap 2) runs as `untrusted/read-only`, the most restrictive pair. `danger-full-access` is never used outside a temporary directory.
- **Reason:** L4 needs both response channels to be proven; the vendored schema (DL-012) marks the experimental methods, so Phase 1 can see each experimental dependency. The capability matrix records which rows need the experimental API.
- **Owner review:** pending

## DL-014 — Codex recordings run with the user's Codex home; fixtures are scrubbed

- **Context:** `codex app-server` has no equivalent of Claude's `--safe-mode`. A recording loads the user's `~/.codex` configuration: the global `AGENTS.md` (`instructionSources`), the configured MCP servers (`mcpServer/startupStatus/updated`), and the `sessionStart` hooks (`hook/started`, `hook/completed`). An isolated `CODEX_HOME` would need a separate login (secrets copied out of the user's home), which the run rules forbid.
- **Options:** (a) copy the auth file to a temporary `CODEX_HOME`; (b) record with the user's home, map the extra notifications to "ignored", and scrub local data.
- **Choice:** (b). The recorder redacts `email`, `account`, `planType`, `userId`, `accountId`, `serverName`, `installationId`, and replaces the work directory, the home directory and the user name. `tests/fixture_hygiene.rs` fails on a leaked path or account field. The core ignores MCP, hook and account notifications by name (the schema test checks the names).
- **Reason:** no secret leaves its file, and the core must handle these notifications on owner machines anyway. Phase 1 decides if product sessions pass `-c` overrides to turn off user hooks (ADR-002 §8).
- **Owner review:** pending

## DL-015 — Codex follow-ups wait host-side; the deny option depends on the policy

- **Context:** PLAN §6.2 lists "follow-up queue" for both providers. Codex 0.160.0: a `turn/start` sent while a turn runs joins the running turn (probe and fixture `message_steering` behaviour), and the queue API (`thread/queue/*`) is experimental. Separately, PLAN §13 `tool_call_deny` assumes a "deny, continue" answer; under the `untrusted` policy Codex 0.160.0 offers only `accept`, `acceptWithExecpolicyAmendment` and `cancel` (fixture `tool_call_deny`, note "offered decisions").
- **Options:** follow-ups: (a) use the experimental queue; (b) hold follow-ups in the core and send `turn/start` after `turn/completed`. Deny: (a) send `decline` anyway; (b) send the deny option that the request offers: `decline` when offered, else `cancel` (deny and end the turn).
- **Choice:** (b) for both. The matrix records "host-side queue" for Codex follow-ups and the offered native decision ids for approvals.
- **Reason:** L8/L9 (never send an option the provider did not offer; unknown options fail closed); the experimental queue has no stability promise. A held follow-up stays `queued` (§6.4) until it is sent, so a crash before the send retires it without an unknown outcome.
- **Owner review:** pending

## DL-016 — Link TLS stack: rustls with the ring provider plus three supporting crates

- **Context:** PLAN §12 names `rustls`, `rcgen`, `ed25519-dalek` and `tokio-tungstenite` for the link. Pinned-key mutual TLS also needs an async TLS stream, an X.509 parser to read the peer key, and a crypto provider. PLAN §12 does not name these.
- **Options:** (a) `aws-lc-rs` (the rustls default provider; needs cmake and a C toolchain); (b) the `ring` provider; (c) read the peer key from the certificate bytes by hand.
- **Choice:** (b) with `tokio-rustls` (async streams, from the rustls project) and `rustls-webpki` (the X.509 parser that rustls itself uses). `ring` also gives the system random generator and SHA-256 for fingerprints. No hand-written certificate parsing: a byte search for the key could be fooled by a certificate that carries a pinned key in an extension (test `a_forged_certificate_that_embeds_a_pinned_key_is_refused`).
- **Reason:** these crates are the closest maintained parts of the named stack (rustls project and its default parser), they add no C build dependency, and `cargo deny check` accepts their licenses.
- **Owner review:** pending

## DL-017 — The link refuses `0.0.0.0`/`::`, checks accepted peers, and never resumes TLS sessions

- **Context:** D13 lists the allowed address ranges for bind and dial. It does not state the "all interfaces" addresses, the source address of an accepted connection, or TLS session resumption.
- **Options:** for each item, allow (simpler) or refuse (stricter).
- **Choice:** refuse all three. (1) `bind` refuses `0.0.0.0` and `::` because they also listen on public interfaces; the owner gives a concrete LAN or tailnet address. (2) `accept` checks the TCP source address with the same D13 rules before the TLS handshake, because a router port forward can route a public peer to a LAN listener. (3) Both TLS configs turn off session tickets and the session cache. A resumed TLS 1.3 session skips the certificate check, so a revoked key could reconnect with an old ticket. The spike found this: `a_revoked_key_is_refused_on_the_next_handshake` failed before the fix.
- **Reason:** doctrine "fail closed": the cost is one full handshake per link connection, and links reconnect rarely.
- **Owner review:** pending

## DL-018 — Link WebSocket stack: tokio-tungstenite plus futures-util

- **Context:** PLAN §12 lists `tokio-tungstenite` for the link channels. Its `WebSocketStream` implements the `Stream`/`Sink` traits of the `futures` ecosystem, so the code needs those traits to send and receive.
- **Options:** (a) `futures-util` (sink + std features only); (b) the full `futures` crate; (c) a hand-written poll loop.
- **Choice:** (a) `tokio-tungstenite 0.30` with only the `handshake` feature (TLS comes from our own `tokio-rustls` stream, DL-016) and `futures-util 0.3` without default features.
- **Reason:** the smallest addition that uses the §12 crate as intended. `cargo deny check` passes.
- **Owner review:** pending

## DL-019 — Spike link scopes and method table

- **Context:** PLAN §8.3 and L15 say that every link method declares a scope and that the receiver checks it on each request. The plan names no scopes and no methods.
- **Options:** (a) one scope per method; (b) a small set of scopes grouped by risk; (c) no scopes in the spike.
- **Choice:** (b) five scopes: `read` (station.info, session.list, events.replay, session.snapshot, artifact.open), `drive` (session.start/prompt/interrupt/stop, mutating, need a `command_id`), `approve` (approval.answer, mutating), `admin` (station.configure), `coordinate` (coordinator.call, granted by a deck to a station). Each method can also need a negotiated feature. The check order is `unknown_method` → `feature_not_negotiated` → `forbidden` → `missing_command_id`, and a refused request never reaches the application. Grants are read on every request, so narrowing and revocation take effect at once.
- **Reason:** risk groups map to what the owner grants a device (D14: view, drive, approve). ADR-004 records the table; Phase 1 can rename the methods without a change to the mechanism.
- **Owner review:** pending

## DL-020 — Handshake details that PLAN §8.2 does not state

- **Context:** PLAN §8.2 gives the `Hello` fields, the transcript signature with the TLS exporter, the version rule, and "higher `link_epoch` wins". It does not state how the two channels of one link are tied together, how a refusal is reported, or the exporter label.
- **Options:** for each gap, the smallest addition that keeps the §8.2 rules.
- **Choice:**
  1. `Hello` has a `channel` field (`control` | `data`); the WebSocket path (`/fdlink/v1/control`, `/fdlink/v1/data`) must agree, and the transcript covers it.
  2. Handshake frames are `hello`, `auth` (Ed25519 signature over the transcript, with a side tag so a signature cannot be reflected), `ready` (listener → dialer: the epoch is accepted), and `close {reason, detail}`. Every refusal sends `close` with a stable reason code (`version_mismatch`, `role_mismatch`, `stale_link_epoch`, `no_control_channel`, ...) before the socket closes, so both sides report the same reason.
  3. The dialer chooses the `link_epoch`; the listener sends `0` and adopts the dialer's value. A data channel attaches only to the current control channel of the same peer and epoch.
  4. The exporter label is `EXPORTER-fleet-deck-link-v1` (32 bytes, no context), read before the WebSocket upgrade.
  5. The control channel has a `close {reason, detail}` frame next to the §8.3 frames (used for `superseded`).
- **Reason:** each item closes a gap that would otherwise make two implementations disagree; none changes a PLAN rule. ADR-004 records them.
- **Owner review:** pending

## DL-021 — Pairing details that PLAN §8.5 does not state

- **Context:** PLAN §8.5 gives the invitation shape `fdinv1:<deck-url>#<fingerprint>.<secret-128-bit>.<expiry>`, single use, 15 minutes, "the station proves the secret bound to the TLS transcript", and the mirror direction. It does not say how the proof travels, how an unpinned key gets through pinned mTLS, or what limits guessing.
- **Options:** (a) a pairing request on the normal control channel; (b) a separate `pair` channel (`/fdlink/v1/pair`) that reuses the link handshake (Hello, TLS-exporter transcript, signatures) and then exchanges one proof each way.
- **Choice:** (b), with these details:
  - Proofs are HMAC-SHA256 with the invitation secret over a role tag and the handshake transcript (TLS exporter, both hellos, both keys). The joiner proves first; the inviter answers with its own proof, so the joiner also knows that the inviter holds the secret. Verification is constant time (`ring::hmac::verify`).
  - The joiner checks the inviter's key against the fingerprint in TLS (`PeerPolicy::Fingerprint`) before it sends anything.
  - The listener accepts an unpinned client key in TLS only while at least one invitation is open (`PeerPolicy::PinnedOrInvited`), and an unpinned key may open only the `pair` channel (`not_paired` otherwise).
  - The `<deck-url>` part is `wss://<ip-literal>:<port>`. The address must pass the D13 dial rules when the invitation is parsed; host names are refused in the spike (Phase 1 can allow names with the resolved-address filter of `addr::filter_resolved`).
  - The expiry is a unix time in seconds. Both sides check it; the inviter's own clock decides.
  - After 5 wrong proofs the inviter closes all open invitations (`too_many_attempts`).
  - Each side grants the other the scopes that the local owner chose when the invitation was made or redeemed. The spike assigns no station names (Phase 1 board work).
- **Reason:** (b) keeps unpinned keys away from every request handler, reuses the audited handshake code, and binds the secret to this TLS session so that a relay cannot reuse a proof. D13 and the "no redirects" rule apply to the invitation address.
- **Owner review:** pending

## DL-022 — Reconnect backoff and link epoch source

- **Context:** PLAN §8.1 asks for exponential backoff with jitter by the dialing side; t3code-lessons caps it at 5 minutes and resets it only after the link stays up. §8.2 says the higher `link_epoch` wins but not where the number comes from.
- **Options:** epoch: (a) a counter persisted in the data directory; (b) the wall clock in milliseconds, strictly increasing inside the process. Jitter: full jitter vs. equal jitter.
- **Choice:** epoch (b) (`reconnect::EpochClock`). Backoff: equal jitter, delay uniform in `[c/2, c]` with `c = min(5 min, 500 ms · 2^attempt)`; the attempt counter resets only after a link stayed up for 60 s (`BackoffPolicy`).
- **Reason:** (b) needs no disk write before a dial, and a restarted dialer always starts above its previous epochs unless the clock steps back by more than the restart time (Phase 1 can add the persisted counter as a floor). Equal jitter never retries at once (no tight loops after a deck restart) and still spreads many stations apart.
- **Owner review:** pending
