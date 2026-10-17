# ADR-004 — Link protocol v1

- **Status:** Accepted (owner, 2026-10-11)
- **Date:** 2026-10-11 (Revision 2: 2026-10-11, M1 gate, §9)
- **Deciders:** owner (accepted 2026-10-11), architect (goal loop, M1)
- **Implements:** PLAN §8 (link protocol), D13 (network scope), L15 (scoped methods), L16 (feature flags), §14 Phase 0 row S4
- **Evidence:** `spikes/s4-link/` (crate `s4-link`), tests in `spikes/s4-link/tests/` and module tests in `spikes/s4-link/src/`
- **Verified against:** rustls 0.23 (ring provider), tokio-rustls 0.26, tokio-tungstenite 0.30, rcgen 0.14, ed25519-dalek 3, Rust 1.96, loopback only (no Docker, no Tailscale)

## 1. Context

A Fleet Deck **link** connects one deck and one station. Either side can dial: the station dials the deck when the station is behind NAT, and the deck dials the station when the station has a listener. After the link is up, the direction does not matter: both sides send requests (§8.1).

PLAN §8 fixes the shape: two WebSockets (control and data) over mutual TLS with pinned Ed25519 keys, a `Hello` exchange with versions and feature flags, a transcript signature bound to the TLS session, `link_epoch` for duplicate links, the control frames of §8.3, scoped methods, single-use `fdinv1:` invitations, and jittered backoff. PLAN leaves several details open. The spike `s4-link` makes them concrete and tests them. DL-016 to DL-023 log every choice that PLAN does not state.

This ADR describes link protocol **v1** as the spike implements it. Phase 3 takes it into `crates/fleetdeck-link`. **Revision 2 (§9) changes the contract after the M1 gate (`docs/v2/GATE-M1.md`, G1–G7). Where §2–§7 and §9 differ, §9 is binding; the spike keeps the Revision 1 behaviour.** The PLAN requires an adversary DESIGN REVIEW of §8 before Phase 3 freezes the protocol. The goal loop cannot run that review (§8 of this ADR).

## 2. Decision

### 2.1 Layers

| Layer | Spike module | Content |
| --- | --- | --- |
| Address scope (D13) | `src/addr.rs` | Allowed ranges, checks at bind, dial and accept |
| Identity | `src/identity.rs`, `src/secret.rs` | Ed25519 key, self-signed certificate, fingerprint, key file mode 600 |
| TLS | `src/tls.rs`, `src/net.rs` | Mutual TLS 1.3, pinned-key verifiers, no resumption |
| Handshake | `src/hello.rs`, `src/link/handshake.rs` | `Hello`, negotiation, transcript signatures, `ready`/`close` |
| Control channel | `src/frames.rs`, `src/link/control.rs` | §8.3 frames, method table, scope checks |
| Data channel | `src/mux.rs`, `src/link/data.rs` | Multiplexed streams with credit flow control |
| Link table | `src/link/mod.rs` | One live link per peer, epochs, data channel bound to its control channel |
| Pairing | `src/pairing.rs`, `src/link/pair.rs` | `fdinv1:` invitations, proofs, pins and grants |
| Reconnect | `src/reconnect.rs` | Backoff with jitter, epoch clock, dial supervisor |
| Test node | `src/bin/fd-link-node.rs` | One deck or station process with its own data directory |
| Test transport | `src/transport.rs` | TCP (product), or FIFO pairs for loopback addresses on hosts without sockets (tests only, DL-025) |

### 2.2 Network scope (D13)

- **Allowed:** loopback, RFC 1918, IPv6 ULA (`fc00::/7`), link-local (`169.254.0.0/16`, `fe80::/10`), and the Tailscale ranges `100.64.0.0/10` and `fd7a:115c:a1e0::/48`. An IPv4-mapped IPv6 address uses the IPv4 rules. Everything else is refused (`addr::classify`).
- **Bind:** a public address is refused. `0.0.0.0` and `::` are refused too, because a wildcard bind also listens on public interfaces (DL-017).
- **Dial:** a public address is refused before any packet. A host name is resolved, and only the allowed addresses are kept (`addr::filter_resolved`).
- **Accept:** the listener checks the peer address before TLS starts (`addr::check_peer`).
- **Tailscale:** an address in a Tailscale range is only reachability. It is never authentication. Mutual TLS with pinned keys stays required.

### 2.3 Identity and TLS

- Each node has one Ed25519 identity key in `<data-dir>/identity.ed25519` (mode 600). `Identity::load` refuses a file that other users can read.
- The node's certificate is self-signed, made from the identity key, with the name `node.fleet-deck.invalid`. Only the key counts: the **fingerprint** is SHA-256 of the raw 32-byte public key, written as 64 lowercase hex characters.
- TLS 1.3 only, signature scheme Ed25519 only, a single certificate (no chain). Client authentication is mandatory.
- **Pinned-key verifiers:** each side accepts the peer only if the peer key is in its pin set with the expected role (`PeerPolicy::Pinned`). The joiner of a pairing accepts only the key named in the invitation (`PeerPolicy::Fingerprint`). An inviting listener also accepts unpinned keys while an invitation is open, but only on the pair channel (`PeerPolicy::PinnedOrInvited`, §2.8).
- **No session resumption** on either side. Without resumption, every connection runs a full certificate check, so a revoked key is refused at the next handshake (DL-017).
- **Revoke** = unpin. The next handshake of that key fails.

### 2.4 Channels

- The dialer opens up to three WebSocket paths over separate TLS connections:

| Path | Channel | Content | Max message |
| --- | --- | --- | --- |
| `/fdlink/v1/control` | control | JSON frames (§2.6) | 1 MiB |
| `/fdlink/v1/data` | data | binary mux frames (§2.7) | 64 KiB + 16 bytes |
| `/fdlink/v1/pair` | pair | pairing only (§2.8) | 1 MiB |

- An unknown path gets HTTP 404 before the handshake.
- The **control** channel opens first. The **data** channel opens only if both sides negotiated `data_streams.v1`. The data channel uses the same `link_epoch` and attaches only to the live control channel of the same peer and epoch. Otherwise it is closed with reason `no_control_channel`. The listener waits for the data channel for at most `handshake_timeout`, then closes the link with reason `no_data_channel`.
- **One lifetime:** when the control channel ends (close, superseded, error), the data channel ends with the same reason, and pending writes fail. When the data channel ends first, the control channel closes with reason `data_channel_lost`. Before it uses that reason, the control side waits up to 500 ms for the peer's `close` frame, because the two TCP connections can end in either order (`link::control`). Dropping the local data handles does not end the link.
- **Writes never block reads:** a separate writer task sends the data frames. Both sides can write a full credit window at the same time without a deadlock, also when the socket buffer is smaller than the window (DL-025).

### 2.5 Handshake

Each channel runs the same handshake after the TLS handshake and the WebSocket upgrade:

1. **TLS exporter.** Before the WebSocket upgrade, each side reads 32 bytes of TLS keying material with the label `EXPORTER-fleet-deck-link-v1` and no context.
2. **Hello** in both directions (JSON text frame `{"type":"hello", ...}`):

   ```json
   {"type":"hello","proto_min":1,"proto_max":1,
    "features":["control.v1","data_streams.v1","command_receipts.v1","snapshots.v1"],
    "role":"station","node_id":"<64-hex fingerprint>","link_epoch":1791687660645,
    "nonce":"<64 hex>","channel":"control"}
   ```

   Unknown fields are ignored (L16). The `channel` field must match the WebSocket path.
3. **Negotiation** (`hello::negotiate`). Each side checks that:
   - `node_id` equals the fingerprint of the key that TLS proved;
   - `role` is the expected peer role (a deck links only to stations, and the reverse);
   - the nonce is 32 random bytes and is not its own nonce sent back;
   - the version ranges overlap.

   The protocol version is the highest common version. The features are the intersection of both lists, and each side's required features (`control.v1`) must be in it. A failure sends `{"type":"close","reason":...,"detail":...}` and closes the socket. Both sides report the same stable reason code. For example, `version_mismatch` carries the detail "no common link protocol version: this node speaks 1..=1, the peer speaks 2..=3".
4. **Transcript signature.** The transcript is the tag `fleet-deck link transcript v1\0`, the 32-byte exporter value, the dialer's raw `Hello` text and the listener's raw `Hello` text (each with a length prefix), then the 32-byte dialer key and the 32-byte listener key. The raw hello texts contain both roles, both nonces, the epoch and the channel. Each side signs the transcript with a side tag (`dialer`/`listener`) and sends `{"type":"auth","signature":"<128 hex>"}`. The peer verifies the signature with the TLS-proven key and the other side's tag. The signature binds the link to this TLS session: a replayed or relayed hello fails.
5. **Ready.** The listener applies the link table (§2.9) and then sends `{"type":"ready"}`, or `close` with `stale_link_epoch`. The dialer considers the link up only after `ready`.

Every step after TLS has the bound `NodeConfig::handshake_timeout` (default 10 s, DL-023): the WebSocket upgrade on both sides, the hello/auth exchange, the `ready` wait, the pairing proofs, and each handshake write. A timeout gives the reason `timeout`, and the error names the step (for example `the wait for ready took longer than 300ms`). The side that times out sends `close` with reason `timeout` before it closes. Each cleanup write (`close` frames and WebSocket close) has its own bound of 1 s, so a peer that reads nothing cannot hold a task.

**Feature flags of v1** (`hello::feature`):

| Flag | Meaning when both sides offer it |
| --- | --- |
| `control.v1` | Control frames of §2.6 (required) |
| `data_streams.v1` | The data channel and the methods that need it (`artifact.open`) |
| `command_receipts.v1` | `command.receipt` (§8.4 receipts, Phase 3) |
| `snapshots.v1` | `Snapshot` frames and `session.snapshot` |
| `revert.v1` | `session.revert` (M3, DL-087; PLAN §6.6 conversation rollback plus workspace restore) and `session.resolve_revert` (DL-097; the owner's resolution of a revert in `reconciliation_required`) |
| `rename.v1` | `session.rename` (M3, DL-095; the board title of a session, event `session.renamed`) |
| `auto_continue.v1` | `session.set_auto_continue` (M3, ADR-007, DL-127; the per-session restart auto-continuation preference, events `session.auto_continue_changed` and `session.auto_continue_skipped`, input origin `continuation`) |
| `import.v1` | `session.import` (M3, DL-308; a new driven session that continues an existing native CLI session id) |
| `coordinator.v1` | `session.coordinator_renew`, `session.coordinator_revoke`, `coordinator.query`, `coordinator.call` (M3, ADR-006 §6.1 `coordinator_v1`, DL-212, DL-214; the owner's renewal and revocation of a coordinator binding) |
| `coordinator.v2` | `coordinator.authority_check`, `coordinator.policy_now` (ADR-008 fleet control: the new coordinator ops and queries, `authority` on routed session commands, `coordinator.authority_check` and `coordinator.policy_now`) |
| `files.v1` | `files.list`, `files.read`, `files.search`, `files.diff` (M3, DL-088; PLAN §6.6 file browse, search, read and per-turn diffs) |
| `workspace.worktrees.v1` | `workspace.worktrees` (DL-142; the read-only inventory of the worktrees that the station created for sessions) |
| `workspace.preflight.v1` | `workspace.preflight` (DL-144; the Git access check of a clone URL, before a clone) |
| `workspace.precheck.v1` | `workspace.precheck` (DL-168; the shell precheck of a schedule, run on the station) |
| `integrations.v1` | `integrations.status`, `issues.search`, `issues.get`, `issues.asset`, `prs.list`, `prs.get`, `prs.checks`, `prs.fetch_head`, `prs.create`, `prs.comment`, `prs.review`, `issues.comment`, `issues.transition` (DL-164; GitHub, GitLab, Jira and Linear through the station) |
| `accounts.v1` | `accounts.list`, `accounts.forge_login`, `accounts.forge_login_status`, `accounts.logout`, `accounts.app_set`, `accounts.token_set`, `accounts.oauth_start`, `accounts.oauth_finish`, `issues.containers` (DL-173; the connected accounts of a station and the credential relay) |
| `files.write_temp.v1` | `files.write_temp` (DL-147; a pasted image stored in the station's paste directory) |
| `files.session_diff.v1` | `files.session_diff` (DL-159; the working tree of a session against its base commit) |
| `history.v1` | `session.history` (DL-189; the earlier conversation of a session, from the station's snapshot of the provider transcript) |
| `subagent.v1` | `session.subagent` (DL-191; the log of one subagent of a session, from the provider's own transcript of that subagent) |
| `native_sessions.v1` | `station.native_sessions`, `station.native_preview` (B7; list the native sessions of the station's provider homes, and preview one native session without importing it) |
| `session_branches.v1` | `session.branches`, `files.branch_diff` (DL-191; the branches of the repository of a session and the committed diff of one) |
| `assist.v1` | `assist.generate` (DL-167; model help on the station, R10) |
| `settings.v1` | `settings.get`, `settings.set`, `settings.secret_set`, `station.restart` (DL-172; the settings of a station, the credential relay for a settings secret, and the restart of the station process) |
| `workspace.trash.v1` | `session.worktree_trash`, `session.worktree_restore`, `session.worktree_purge`, `session.worktree_release` (DL-150; a worktree moved to a trash folder, restored, deleted for good, or released) |
| `artifacts.v1` | `artifacts.publish`, `artifacts.list`, `artifacts.get`, `artifacts.file` (DL-165; R12 artifacts: versioned files that a session or a person publishes, stored in the station's data directory) |
| `shim.resume.v1` | `session.execution_check`, `session.reconcile_execution` (DL-182; the termination check and the owner's reconciliation for resume on the shim host) |
| `shell_command.v1` | `session.shell_command`, `session.release_shell` (B8, DL-196; a `!` shell command in a session folder, and the owner's release of a folder) |
| `desktop.v1` | `desktop.info`, `desktop.act` (R13 §6, DL-201; desktop control on the station: Hyprland and Omarchy info, and a closed list of actions with typed argv) |
| `fleet_git.v1` | `fleet_git.projects_set`, `fleet_git.push_now`, `fleet_git` on `session.start`, and the events `workspace.base_stale`, `workspace.include_skipped`, `session.push`, `session.cleanup` (Fleet Git phase 1, ADR-009; the Git settings of a project, fetch before a start, push, cleanup and worktree bootstrap) |
| `fleet_git.homes.v1` | `project.ensure_home`, `project.home_status`, `fleet_git.artifact_authorize`, `fleet_git.share_verdict`, the file root `artifacts` of `files.v1`, and the events `artifacts.ready`, `artifacts.unavailable`, `artifacts.revoked` (Fleet Git phase 2, ADR-009; homes of a project on many stations, the artifact share verdicts) |
| `fleet_git.land.v1` | `project.radar`, `session.land`, `fleet_git.land_status`, `fleet_git.land_abandon` and the event `session.land` (Fleet Git phase 3, ADR-009; the predictive conflict radar and the land flow) |
| `pair_request.v1` | The pair-channel frames `pair_request`, `pair_pending`, `pair_approved` and `pair_resume` (DL-178, §10): a station asks a deck to pair without an invitation and the owner approves on the board |

A method whose feature was not negotiated gets the error `feature_not_negotiated`. Behaviour depends on the negotiated features, not only on the version.

### 2.6 Control frames

The control channel carries JSON text frames of at most 1 MiB (`frames::encode`/`decode` refuse a larger frame). The tag is `type`:

```text
request   { id: u64, command_id?: ULID, method, params }
response  { id, ok: value } | { id, err: { code, message } }
event     { station_seq, session_id, session_seq, event }
ack       { station_seq }                         // deck → station after the read model commits
snapshot  { session_id, as_of_station_seq, state }
ping      { nonce }   pong { nonce }
close     { reason, detail }
```

- `id` is chosen by the sender of the request and is unique per direction. A `response` to an unknown id is a protocol error and ends the link.
- `ping` is answered with `pong` automatically.
- `close` is the last frame before the socket closes. For example, the old link of a duplicate gets `superseded`.

**Scopes and methods (L15, DL-019).** The receiver authorizes every request before the application sees it. The checks run in this order:

1. `unknown_method`
2. `feature_not_negotiated`
3. `forbidden`: the scope is not granted to the peer key
4. `missing_command_id`: a mutating method without `command_id`

The grants are read again at each request, so a narrowed or revoked grant applies at once. v1 scopes: `read`, `drive`, `approve`, `admin`, `coordinate`. The spike method table (`frames::METHODS`):

| Method | Scope | Feature | Mutating |
| --- | --- | --- | --- |
| `station.info`, `session.list`, `events.replay` | read | — | no |
| `session.snapshot` | read | `snapshots.v1` | no |
| `artifact.open` | read | `data_streams.v1` | no |
| `session.start`, `session.prompt`, `session.interrupt`, `session.stop` | drive | — | yes |
| `approval.answer` | approve | — | yes |
| `station.configure` | admin | — | yes |
| `command.receipt` | read | `command_receipts.v1` | no |
| `coordinator.call` | coordinate | — | no |

Phase 3 extends the table. Every new method must declare a scope and, if needed, a feature. **Command semantics (§8.4):** a mutating request carries a ULID `command_id`. After a reconnect the deck resends the **same** `command_id`, and the station returns the stored receipt. The spike enforces the presence of `command_id`. The receipt store is Phase 3 (Phase 0 does not need it).

### 2.7 Data channel

- Binary WebSocket messages. Each message is one mux frame `[kind: u8][stream: u32 big-endian][payload]`:

| Kind | Payload |
| --- | --- |
| `1` open | UTF-8 purpose (for example `artifact:<id>`) |
| `2` data | ≤ 64 KiB of bytes |
| `3` credit | `u32` additional bytes the sender may send |
| `4` close | — (half close by the sender) |
| `5` reset | UTF-8 reason (abort both halves) |

- Stream ids: the dialer uses odd ids and the listener uses even ids, so both sides can open streams without a collision. At most 256 open streams per link.
- **Flow control:** each stream starts with 256 KiB of credit in each direction. A sender never sends more than its credit. A reader grants new credit after it has consumed half of the window. A reader that consumes nothing stops the writer at exactly 256 KiB.
- **Violations end the link:** data beyond the credit, data after close, an unknown stream, a wrong-parity stream id, a credit overflow, or a malformed frame. A late credit frame for a finished stream is ignored.

### 2.8 Pairing (§8.5, DL-021)

- **Invitation text:** `fdinv1:wss://<ip-literal>:<port>#<fingerprint-64-hex>.<secret-32-hex>.<expiry-unix-seconds>`.
  - Only IP literals are allowed, so the joiner checks the address against D13 before it dials. A host name cannot redirect it.
  - The secret is 128 random bits and stays in `Secret<[u8;16]>`. `Debug` never prints it, and the secret is in the URL fragment.
  - The inviter sets the expiry (default 15 minutes).
- **Single use:** the inviter keeps the open invitations in memory. A successful proof consumes its invitation. After 5 wrong proofs, every open invitation closes (`too_many_attempts`). An expired invitation fails on both sides: the joiner refuses to dial with it, and the inviter refuses its proof.
- **Flow** (the joiner always dials; this works in both directions):
  1. The joiner opens `/fdlink/v1/pair` with `PeerPolicy::Fingerprint(<fingerprint>)`, so it talks only to the invited key.
  2. The listener accepts the unpinned joiner key at the TLS level only while an invitation is open (`PinnedOrInvited`). An unpinned key on the control or data path gets `not_paired`. The listener reads that dialer's Hello first (bounded by `handshake_timeout`), so the dialer receives the reason and not a broken pipe.
  3. Both sides run the normal handshake (§2.5) with epoch 0.
  4. The joiner sends `{"type":"pair_proof","proof": HMAC-SHA256(secret, "fd-pair-v1 joiner\0" ‖ transcript)}`.
  5. The inviter verifies the proof in constant time, consumes the invitation, pins the joiner key with the joiner's role, grants the scopes chosen when the invitation was made, and answers `{"type":"paired","proof": HMAC-SHA256(secret, "fd-pair-v1 inviter\0" ‖ transcript)}`.
  6. The joiner checks the inviter proof, pins the inviter key, and grants its own locally chosen scopes.
  7. The pair connection closes, and the joiner then dials the normal control/data channels.
- **What the proofs prove:** both proofs are bound to the TLS transcript (§2.5). A party that does not hold the secret cannot pair. A relay cannot reuse a proof on another TLS session.
- **Names:** naming the station is a deck read-model concern (Phase 3), not a protocol field.
- **Who adds dial targets:** only the owner adds them (CLI flags `--join`/`--dial` in the spike). There are no redirects.

### 2.9 Duplicate links and epochs

- The dialer chooses `link_epoch` from `reconnect::EpochClock`: wall-clock milliseconds, strictly increasing in the process (DL-022). The listener sends epoch 0.
- The listener keeps one live link per peer key:
  - A new control channel with a **higher** epoch replaces the live link. The old link gets `close` `superseded`, and its data channel ends too.
  - A new control channel with a **lower or equal** epoch is refused with `stale_link_epoch`.
- A restarted dialer starts above its earlier epochs, so it always wins against a half-open old link.

### 2.10 Reconnect

- Only the dialer reconnects (`reconnect::spawn_dialer`). It dials, reports `Linked`, waits until the control channel closes, then reports `Lost` and dials again.
- **Backoff:** equal jitter. The delay is uniform in `[c/2, c]` with `c = min(5 min, 500 ms · 2^attempt)`. The attempt counter resets only after a link stayed up for 60 s, so a link that fails at once does not dial in a tight loop.
- Each attempt uses a new, higher epoch.
- A failed attempt is reported with its stable reason, for example `timeout`, `stale_link_epoch`, `version_mismatch` or a TCP error, and then retried after the backoff.

### 2.11 Error reasons

Every refusal has a stable reason code that both sides can show:

| Reasons | Raised by |
| --- | --- |
| `version_mismatch`, `role_mismatch`, `node_id_mismatch`, `channel_mismatch`, `missing_feature`, `bad_hello`, `bad_signature`, `protocol_error` | handshake |
| `not_paired`, `stale_link_epoch`, `superseded`, `no_control_channel`, `no_data_channel`, `data_channel_lost` | link table and channels |
| `bad_invitation`, `address_refused`, `invitation_expired`, `invalid_invitation`, `too_many_attempts`, `inviter_proof_failed` | pairing |
| `window_closed`, `busy`, `already_paired`, `request_id_conflict`, `denied`, `expired`, `cancelled`, `unknown_request`, `revoked`, `fingerprint_mismatch`, `unsupported` | pairing requests (§10) |
| `unknown_method`, `feature_not_negotiated`, `forbidden`, `missing_command_id` | request errors in a `response` |
| `timeout` | any bounded step |
| `self_connect` | a dial whose TCP socket connected to itself (no listener on a loopback port) |

## 3. Failure behaviour

| Failure | Behaviour | Evidence |
| --- | --- | --- |
| Peer stalls after TLS (no upgrade, no hello, no `ready`, no pairing proof) | The step fails with `timeout` after `handshake_timeout`, and the stalled peer gets `close` `timeout`. The listener keeps accepting other dialers, the dial supervisor retries with backoff, and a stalled pairing pins nothing and keeps the invitation open. | `tests/timeouts.rs`, `tests/stalled_handshake.rs` |
| No listener on a loopback port (deck restarting) | A dialing socket that the kernel connects to itself (TCP simultaneous open) is dropped at once (`self_connect`), so it never holds the port that the restarting listener needs. | `net::tests::a_socket_connected_to_itself_is_refused` |
| Deck restarts | The stations see `Lost`, retry with backoff, and relink with a higher epoch. The pins persist in the data directory. | `multiprocess::nat_stations_pair_dial_and_reconnect_after_a_deck_restart` |
| Old half-open link plus new link | The higher epoch wins. The old link and its data channel close with `superseded`. | `link::a_higher_link_epoch_supersedes_and_a_lower_one_is_refused`, `link::a_superseded_link_loses_its_data_channel_too` |
| Data channel TCP drops | The control channel closes with `data_channel_lost`, and the dialer relinks. | `link::closing_the_control_channel_ends_the_data_channel_on_both_sides` (reverse path in `link::control`) |
| Slow reader | The writer stops at the credit window. Nothing is buffered without a bound. | `link::a_reader_that_gives_no_credit_stops_the_writer` |
| Key revoked | The next handshake is refused, because no session resumption exists. | `tls::a_revoked_key_is_refused_on_the_next_handshake`, `tls::configs_never_resume_sessions` |
| Invitation stolen after use / expired | Refused. Nothing is pinned. | `pairing::an_invitation_works_once`, `pairing::expired_invitations_fail_on_both_sides`, `multiprocess::nat_stations_pair_dial_and_reconnect_after_a_deck_restart` |

## 4. §14 Phase 0 S4 coverage

| Must prove | Section | Evidence (test names) |
| --- | --- | --- |
| Both directions | 2.4, 2.8 | `link::the_station_dials_the_deck`, `link::the_deck_dials_the_station`, `pairing::a_station_without_a_listener_joins_the_deck_and_dials_it`, `pairing::the_deck_joins_a_station_invitation_and_dials_the_station`, `multiprocess::the_deck_joins_a_station_invitation_and_dials_it` |
| mTLS with pinned keys | 2.3 | `tls::pinned_mtls_works_with_the_station_dialing`, `tls::pinned_mtls_works_with_the_deck_dialing`, `tls::an_unpinned_listener_is_refused_by_the_dialer`, `tls::an_unpinned_dialer_is_refused_by_the_listener`, `tls::a_pinned_key_with_the_wrong_role_is_refused`, `tls::a_forged_certificate_that_embeds_a_pinned_key_is_refused`, `tls::a_dialer_without_a_certificate_is_refused`, `tls::the_fingerprint_policy_accepts_only_the_named_key` |
| Invitation pairing | 2.8 | `pairing::*` (8 tests), `pairing.rs` unit tests (`invitations_round_trip_and_never_print_the_secret`, `bad_invitations_are_refused`, `a_proof_redeems_its_invitation_once`, `expired_invitations_are_refused`, `wrong_proofs_close_every_invitation`), `pairing::the_joiner_accepts_only_the_invited_key`, `pairing::a_wrong_secret_pairs_nothing`, `pairing::an_unpaired_key_may_only_use_the_pair_channel`, `timeouts::pairing_with_a_server_that_never_upgrades_times_out` |
| Control + data channels | 2.4, 2.6, 2.7 | `link::the_station_dials_the_deck` (requests both ways, event/ack, ping), `link::requests_are_checked_against_the_method_table_and_the_granted_scopes`, `link::a_data_channel_needs_its_control_channel`, `link::the_data_channel_streams_a_mebibyte_each_way_with_flow_control`, `link::a_reader_that_gives_no_credit_stops_the_writer`, `link::closing_the_control_channel_ends_the_data_channel_on_both_sides`, `link::dropping_the_data_handles_keeps_the_link_up`, `frames.rs` unit tests (`frames_have_the_plan_shapes`, `oversized_frames_are_refused_both_ways`, `authorization_checks_method_feature_scope_and_command_id`), `mux.rs` unit tests (`a_sender_stops_at_the_credit_and_continues_after_a_grant`, `protocol_violations_are_errors`, `decode_never_panics`) |
| Reconnect | 2.9, 2.10 | `pairing::the_dialer_reconnects_with_backoff_and_a_higher_epoch`, `multiprocess::nat_stations_pair_dial_and_reconnect_after_a_deck_restart`, `link::a_higher_link_epoch_supersedes_and_a_lower_one_is_refused`, `timeouts::the_reconnect_supervisor_keeps_retrying_a_stalled_server`, `reconnect.rs` unit tests (`delays_grow_with_jitter_up_to_the_cap`, `only_a_stable_link_resets_the_backoff`, `epochs_always_grow`) |
| Feature-flag handshake | 2.5 | `link::features_are_negotiated_and_gate_methods_and_channels`, `link::no_common_version_fails_on_both_sides_with_the_same_reason`, `hello_tests` (`both_sides_negotiate_the_same_result`, `the_highest_common_version_wins`, `no_common_version_is_a_clear_error`, `features_are_the_intersection_and_required_ones_are_enforced`, `unknown_hello_fields_are_ignored`, `the_peer_identity_role_channel_and_nonce_are_checked`, `signatures_bind_the_tls_session_the_hellos_and_the_side`) |
| NAT case (station only dials) | 2.8, 2.10 | `pairing::a_station_without_a_listener_joins_the_deck_and_dials_it`, `multiprocess::nat_stations_pair_dial_and_reconnect_after_a_deck_restart` (two stations without a listener, separate processes and data directories) |
| Public-address rejection | 2.2 | `addr.rs` unit tests (`bind_dial_and_peer_checks`, `only_listed_ranges_are_allowed_v4`, `only_listed_ranges_are_allowed_v6`), `tls::public_and_unspecified_addresses_are_refused_before_any_packet`, `multiprocess::public_and_wildcard_addresses_are_refused` |
| Tailnet address rules (unit tests) | 2.2 | `addr.rs` unit tests `tailnet_ipv4_range_edges`, `tailnet_ipv6_prefix_is_exactly_a_48`, `ipv4_mapped_ipv6_uses_the_ipv4_rules`, `resolved_names_keep_only_allowed_addresses`. A real tailnet check is an owner step (§8). |
| Stalled peers do not block setup | 2.5 | `timeouts::a_dial_to_a_server_that_never_upgrades_times_out`, `timeouts::a_client_that_never_sends_the_upgrade_does_not_block_the_listener`, `stalled_handshake::a_listener_that_never_sends_hello_times_out_the_dial`, `stalled_handshake::a_listener_that_never_sends_ready_times_out_the_dial`, `stalled_handshake::a_dialer_that_never_sends_hello_times_out_on_the_listener`, `stalled_handshake::an_inviter_that_never_answers_the_proof_times_out_the_joiner`, `stalled_handshake::a_joiner_that_never_sends_its_proof_times_out_and_keeps_the_invitation` |

Offline command (no network): `cargo nextest run -p s4-link`.

## 5. Alternatives considered

- **One WebSocket with multiplexed control and data.** Rejected by PLAN §8.1. Large artifact transfers would add head-of-line latency to control frames and approvals.
- **QUIC.** Rejected for v1. It needs UDP reachability through home routers, and the PLAN stack (§12) is WebSocket over TLS.
- **A CA-based PKI.** Rejected. Pinned self-signed keys need no CA, and revoke is unpin (PLAN §8.2).
- **SPAKE2/PAKE pairing instead of an HMAC proof.** Not needed: the joiner already authenticates the inviter by fingerprint, so a password-strength secret is not involved. The 128-bit secret only proves that the joiner got the invitation.
- **Host names in invitations.** Rejected for v1 (DL-021). An IP literal can be checked against D13 before the dial, and DNS cannot redirect the joiner.
- **A persisted epoch counter.** Deferred (DL-022). Phase 3 can add it as a floor under the wall-clock epoch.

## 6. Risks

- **Clock steps back** by more than the restart time: the restarted dialer's epoch is lower than its old one, and the listener refuses it with `stale_link_epoch` until the old link closes (the old link's TCP dies or the backoff outlasts it). Mitigation in Phase 3: a persisted epoch floor.
- **Data channel loss vs. control close race:** handled with a 500 ms grace (§2.4). Under extreme load the reason can be `data_channel_lost` instead of `superseded`. Both reasons lead to the same recovery.
- **Key rotation (§8.5)** is not implemented in the spike: "a paired node signs a new key with its old key". Phase 3 adds a `link.rotate_key` method (scope `admin`) that carries the new key signed by the old key (§9.5).
- **In-memory invitations:** a restart of the inviter drops the open invitations. This is safe (fail closed), and the owner makes a new invitation.
- **No real tailnet test:** the Tailscale ranges are unit-tested only.

## 7. Phase 3 work that this ADR does not cover

The `command_id` receipt store and the `ambiguous` state (§8.4), the event replay with `station_seq`/`ack` and snapshots across reconnects (§7), station names, key rotation, LAN discovery suggestions, and persistent grants with owner-chosen scopes.

## 8. Owner open points

1. PLAN Phase 0 asks for an adversary DESIGN REVIEW of ADR-004. The goal loop forbids subagents, so the review has not run. Run it before Phase 3 freezes the protocol.
2. A real tailnet check (two nodes on Tailscale addresses) is an owner step.
3. Approve DL-016 to DL-023, especially the spike scope names (DL-019), the pair channel and IP-literal invitations (DL-021), and the wall-clock epoch (DL-022).

## 9. Revision 2 (M1 gate)

- **Date:** 2026-10-11 (M2, item G-C). **Source:** `docs/v2/GATE-M1.md` §3, items G1–G7 (adversary DESIGN REVIEW, verdict FAIL; the architect decided each fix). **Log:** DL-040.
- **Scope:** this section changes the protocol contract. `crates/fleetdeck-link` implements it in Phase 3 (items P3-1 to P3-10 of `docs/v2/PROGRESS.md`). The spike `spikes/s4-link` stays at Revision 1 and is not changed (GATE-M1: "Do not fix them in the spike").
- **Supersedes:** §2.5 step 5 and §2.9 (epochs and duplicate links), §2.6 frame list, method table and command semantics, §2.7 stream limits and the `purpose` string, §2.8 "after 5 wrong proofs, every open invitation closes" (G11), §5 "a persisted epoch counter is deferred", and §6 risk "clock steps back".

### 9.1 Link table, epochs and attempts (G1)

- **One node-wide link table** keyed by the peer key fingerprint. It holds inbound and outbound links of every listener and every dialer of the node. A node never has two Ready links to one peer for longer than the resolution below takes.
- **`link_epoch`** is a persistent per-peer counter of the dialer. The dialer increments it and commits it to its SQLite database (`link_epochs(peer_key PRIMARY KEY, epoch)` in station.db or deck.db) **before** each dial. It never comes from the wall clock. The listener sends epoch 0.
- **`attempt_id`:** each dial attempt also has a random 128-bit `attempt_id` (32 lowercase hex characters). The dialer sends it in the `Hello` of every channel of the attempt (new field `attempt_id`; unknown fields stay ignored, L16). The data channel binds to the control channel with the same peer, epoch **and** `attempt_id`; any other data channel is closed with `no_control_channel`.
- **Highest epoch per peer.** The listener persists the highest epoch that it accepted from each dialer. An attempt with an epoch lower than or equal to it is refused with `close { reason: "stale_link_epoch", min_epoch }`. The dialer then raises its counter above `min_epoch`, commits it and dials again. So a stale epoch after a restart is refused, and a dialer whose database was reset recovers after one attempt.
- **Resolution** when a second link to the same peer reaches Ready (both sides compute the same result):
  1. Same dialer: the higher epoch wins.
  2. Opposite dialers (a reciprocal dial): the link dialed by the node with the **lower key fingerprint** (bytewise compare of the 32-byte SHA-256 values) wins.
- The losing link closes with `superseded`. Frames of a superseded attempt are dropped before they reach the application.
- **Cleanup** (G10, implementation): the table entry is removed by a generation-aware guard that is installed right after the insert and runs on every failure path.

### 9.2 Data channel budgets and stream tokens (G2, G6)

Per link (these values replace §2.7 "at most 256 open streams"):

| Budget | Value | Violation |
| --- | --- | --- |
| Open streams | 64 | `flow_control_violation` |
| Per-stream window (each direction) | 256 KiB | `flow_control_violation` |
| Buffered inbound bytes, whole connection | 4 MiB, counted until the application consumes them, also after a stream reset | `flow_control_violation` |
| Control inbound queue / outbound queue | 256 messages each; pending writes count against the budget | `flow_control_violation` |
| Outstanding requests (G8) | 128 per link | `too_many_requests` error response |

- An **empty data frame** is a protocol error (`protocol_error`). A **reset for an unknown stream** is ignored and makes no event.
- A stream reset fails every pending write of that stream at once; stream ids are never reused within one attempt (G9, implementation).
- **Stream tokens** replace the free `purpose` string. A control response that authorizes a stream (for example `artifact.open`) returns `stream_token`: 128 random bits, single use, bound to the link's `attempt_id`, valid for 30 s. The `open` mux frame carries the token as its payload. An unknown, used, expired or foreign token resets the stream with `bad_stream_token`.
- The reader and the writer stay independent tasks (§2.4).

### 9.3 Command envelopes, incarnation and deadline (G3)

- Every mutating method (§9.6) carries `command_id` in the frame **and** an envelope in `params`. Every envelope has the same admission fields (`command_id`, `target_incarnation`, `retry_until`) and one payload field. The frame's `command_id` and the envelope's must agree; a session command must belong to the method (`SessionCommand::method`); else `invalid_command`. An unknown payload operation or field is `invalid_command` (fail closed).
- **Envelope table.** `fleetdeck_proto::mutation::ENVELOPES` must equal this table; `crates/fleetdeck-proto/tests/adr_revision2.rs` compares them.

| Methods | `params` type | Payload field | Holder |
| --- | --- | --- | --- |
| `session.start`, `session.resume`, `session.import`, `session.fork`, `session.prompt`, `session.steer`, `session.follow_up`, `session.interrupt`, `session.set_mode`, `session.set_model`, `session.stop`, `session.revert`, `session.rename`, `session.resolve_revert`, `session.set_auto_continue`, `session.coordinator_renew`, `session.coordinator_revoke`, `session.worktree_trash`, `session.worktree_restore`, `session.worktree_purge`, `session.worktree_release`, `session.reconcile_execution`, `session.shell_command`, `session.release_shell`, `session.land`, `approval.answer` | `CommandEnvelope` | `command` | station |
| `station.configure` | `StationConfigureEnvelope` | `change` | station |
| `link.rotate_key` | `KeyRotationEnvelope` | `rotation` | receiver |
| `shell.open`, `shell.resize`, `shell.close`, `shell.handover`, `shell.release` | `ShellEnvelope` | `change` | station |
| `coordinator.call` | `CoordinatorCallEnvelope` | `call` | deck |

- **Holder** is the node that decides the command in one transaction, keeps its receipt, and whose database incarnation `target_incarnation` names. Every node's database (station.db and deck.db) has an incarnation id; a reset database gets a new one. The `hello` of Revision 2 carries the sender's incarnation (new field `incarnation`), so each side knows the value to target.
- `target_incarnation` is optional only in `CommandEnvelope`, and only on the station-local socket (PLAN §6.4, DL-038). Over the link it is required for every envelope.
- Payloads:
  - `CommandEnvelope.command`: a `SessionCommand` (tag `op`); `origin` says who sent it (`board`, `mail`, `coordinator`).
  - `StationConfigureEnvelope.change`: a `StationChange` (tag `op`): `set_max_sessions { max_sessions }` (at most the station.toml value) or `set_offline_approval_policy { deny_after_ms? }`. Allowed roots, provider instances, shells and setup commands stay station-local; the deck cannot widen what a station may touch (DL-041).
  - `KeyRotationEnvelope.rotation`: `{ old_key, new_key, signature }` (§9.5).
  - `CoordinatorCallEnvelope.call`: `{ caller, token_digest, op }`: the session of the coordinator agent, the SHA-256 of the token that the agent showed to its station (the token never crosses the link), and a `CoordinatorOp` (tag `op`): `spawn { station, spec, prompt? }`, `send { to, text }`, `stop { station, session_id }`, `assign { task, to }`. The deck checks the token class and spawn cap in the decision transaction.
- The receipt hash covers the method, the caller and the payload field only, so a resend with the same payload is a duplicate (`Mutation::canonical_payload`).
- `retry_until` is the sender's UTC time in Unix ms; the default is accepted-at + 15 min (`RETRY_WINDOW_MS`).
- The sender stores the envelope before it sends it. A stored envelope never changes; a resend sends the same bytes.
- The holder rejects, **without storing a receipt**:
  - `incarnation_mismatch`: `target_incarnation` is not its current incarnation (for example after a database reset);
  - `expired`: `retry_until` is in the past by more than the 2 min skew allowance (`CLOCK_SKEW_MS`).
- The sender never resends after `retry_until`. Deck command states (`DeckCommandState`); a station that sends `coordinator.call` keeps the same states:
  - sent and not acknowledged before `retry_until` → `ambiguous`;
  - rejected with `incarnation_mismatch` or `expired` after an earlier send → `ambiguous`;
  - rejected that way and never sent before → `failed` (`station_reset`, or `deck_reset` for a coordinator call).
- Nothing is redispatched automatically. Every holder keeps receipts for at least 24 h (`RECEIPT_RETENTION_MS`), more than the retry horizon.
- The response to a mutating request is the `Receipt` (accepted or rejected). Execution outcomes arrive later as events (PLAN §8.4).

### 9.4 Incarnation on every stream frame and snapshot batches (G4)

Control frames (`fleetdeck_proto::link::ControlFrame`, tag `type`), replacing the §2.6 list:

```text
request           { id, command_id?, method, params }
response          { id, ok } | { id, err: { code, message, data? } }
event             { incarnation, station_seq, session_id, session_seq, at, proto_version, raw_ref?, event }
ack               { incarnation, station_seq }
snapshot_begin    { incarnation, watermark, session_count }
snapshot_session  { incarnation, snapshot: { summary } }
snapshot_end      { incarnation, watermark }
ping              { nonce }
pong              { nonce }
close             { reason, detail }
```

- A snapshot is a batch: `snapshot_begin`, then `snapshot_session` × `session_count`, then `snapshot_end` with the same watermark. The deck applies the whole batch and sets its cursor to `watermark` in **one** transaction at `snapshot_end`. Sessions of that station that are not in the batch are marked `removed`. A batch without its end (link drop, wrong count, other incarnation) has no effect.
- An `ack` with an incarnation that is not the station's current one, or with a `station_seq` above the highest sequence delivered on this link (event or snapshot watermark), is a protocol error that closes the link.
- An `event` or snapshot frame with an incarnation that differs from the one the deck holds for that station starts a new cursor: the deck asks for a snapshot (`session.snapshot`) and does not mix the two sequences.

### 9.5 Revocation and key rotation (G5)

- A revocation (`fleetdeck station forget-deck`, "remove station" on the deck) removes the pin and every grant of that key in **one** transaction and increments a persistent revocation generation.
- Every active link of that key closes within 1 s (reason `revoked`).
- The decision transaction of each command checks, in the same transaction, that the caller's pin and the needed grant still exist. A command that was queued from a revoked link is rejected (`forbidden`).
- Key rotation (`link.rotate_key`, scope `admin`, `KeyRotationEnvelope`): `old_key` is the fingerprint of the pinned key (64 hex digits), `new_key` the new Ed25519 public key (64 hex digits), `signature` an Ed25519 signature by the old key (128 hex digits) over `KeyRotation::signed_message`: `fleetdeck-rotate-key-v1`, old key, new key, the receiver's fingerprint and the `command_id`, each ended by a zero byte. The receiver checks that `old_key` is the link's pinned peer key and the signature, then replaces the key in one transaction. The new key gets only the role and the grants of the old key. The old key is then revoked as above.

### 9.6 Roles, directions and method table (G6, G7)

- Every frame is checked against the roles that were **bound at pairing** (`station` or `deck`), never against the dial direction.
- Fixed directions: `event`, `snapshot_begin`, `snapshot_session`, `snapshot_end` go station → deck; `ack` goes deck → station. A frame in the wrong direction is a protocol error that closes the link. `request`, `response`, `ping`, `pong`, `close` go both ways.
- Snapshot frames need the negotiated `snapshots.v1`; without it they are a protocol error.
- Request checks, in this order: `unknown_method`, `wrong_role` (the caller or the receiver role does not match the table), `feature_not_negotiated`, `forbidden` (scope not granted, read in the same transaction as the decision for a mutating method, §9.5), `missing_command_id`.
- **Method table v1 (Revision 2).** The generated table in `fleetdeck-link` must equal this table; a table-agreement test (P3-2) compares them.

| Method | Caller → receiver | Scope | Feature | Mutating |
| --- | --- | --- | --- | --- |
| `station.info` | deck → station | read | — | no |
| `session.list` | deck → station | read | — | no |
| `events.replay` | deck → station | read | — | no |
| `session.snapshot` | deck → station | read | `snapshots.v1` | no |
| `command.receipt` | deck → station | read | `command_receipts.v1` | no |
| `workspace.list` | deck → station | drive | — | no |
| `files.list` | deck → station | drive | `files.v1` | no |
| `files.read` | deck → station | drive | `files.v1` | no |
| `files.search` | deck → station | drive | `files.v1` | no |
| `files.diff` | deck → station | drive | `files.v1` | no |
| `workspace.worktrees` | deck → station | drive | `workspace.worktrees.v1` | no |
| `workspace.preflight` | deck → station | drive | `workspace.preflight.v1` | no |
| `workspace.precheck` | deck → station | admin | `workspace.precheck.v1` | no |
| `integrations.status`, `issues.search`, `issues.get`, `issues.asset`, `prs.list`, `prs.get`, `prs.checks` | deck → station | read | `integrations.v1` | no |
| `prs.fetch_head`, `prs.create`, `prs.comment`, `prs.review`, `issues.comment`, `issues.transition` | deck → station | drive | `integrations.v1` | no (the `command_id` is in the params; see below) |
| `files.write_temp` | deck → station | admin | `files.write_temp.v1` | no |
| `files.session_diff` | deck → station | read | `files.session_diff.v1` | no |
| `session.history` | deck → station | read | `history.v1` | no |
| `session.subagent` | deck → station | read | `subagent.v1` | no |
| `station.native_sessions` | deck → station | read | `native_sessions.v1` | no |
| `station.native_preview` | deck → station | read | `native_sessions.v1` | no |
| `session.branches`, `files.branch_diff` | deck → station | read | `session_branches.v1` | no |
| `files.coordinator_diff` | deck → station | read | `coordinator.v2` | no |
| `assist.generate` | deck → station | drive | `assist.v1` | no |
| `settings.get`, `settings.set`, `settings.secret_set`, `station.restart` | deck → station | admin | `settings.v1` | no |
| `artifact.open` | deck → station | read | `data_streams.v1` | no (returns a `stream_token`) |
| `artifacts.publish` | deck → station | drive | `artifacts.v1` | no |
| `artifacts.list`, `artifacts.get`, `artifacts.file` | deck → station | read | `artifacts.v1` | no |
| `accounts.list`, `accounts.forge_login`, `accounts.forge_login_status`, `accounts.logout`, `accounts.app_set`, `accounts.token_set`, `accounts.oauth_start`, `accounts.oauth_finish` | deck → station | admin | `accounts.v1` | no (the credential relay; see below) |
| `issues.containers` | deck → station | read | `accounts.v1` | no |
| `session.start`, `session.resume`, `session.fork`, `session.prompt`, `session.steer`, `session.follow_up`, `session.interrupt`, `session.set_mode`, `session.set_model`, `session.stop` | deck → station | drive | — | yes |
| `session.revert` | deck → station | drive | `revert.v1` | yes |
| `session.resolve_revert` | deck → station | drive | `revert.v1` | yes |
| `session.rename` | deck → station | drive | `rename.v1` | yes |
| `session.set_auto_continue` | deck → station | drive | `auto_continue.v1` | yes |
| `session.import` | deck → station | drive | `import.v1` | yes |
| `session.coordinator_renew` | deck → station | drive | `coordinator.v1` | yes |
| `session.coordinator_revoke` | deck → station | drive | `coordinator.v1` | yes |
| `session.worktree_trash`, `session.worktree_restore`, `session.worktree_purge`, `session.worktree_release` | deck → station | admin | `workspace.trash.v1` | yes |
| `session.execution_check` | deck → station | read | `shim.resume.v1` | no |
| `session.reconcile_execution` | deck → station | admin | `shim.resume.v1` | yes |
| `session.shell_command` | deck → station | admin | `shell_command.v1` | yes |
| `session.release_shell` | deck → station | admin | `shell_command.v1` | yes |
| `desktop.info` | deck → station | read | `desktop.v1` | no |
| `desktop.act` | deck → station | drive | `desktop.v1` | no |
| `approval.answer` | deck → station | approve | — | yes |
| `station.configure` | deck → station | admin | — | yes |
| `link.rotate_key` | either → either | admin | — | yes |
| `shell.list` | deck → station | drive | `shell.v1` | no |
| `shell.attach` | deck → station | drive | `shell.v1` | no (returns a `stream_token`) |
| `shell.open`, `shell.resize`, `shell.close` | deck → station | drive | `shell.v1` | yes |
| `shell.handover`, `shell.release` | deck → station | drive | `shell.handover.v1` | yes |
| `coordinator.query` | station → deck | coordinate | `coordinator.v1` | no |
| `coordinator.call` | station → deck | coordinate | `coordinator.v1` | yes |
| `coordinator.authority_check` | station → deck | coordinate | `coordinator.v2` | no |
| `coordinator.policy_now` | deck → station | read | `coordinator.v2` | no |
| `fleet_git.projects_set` | deck → station | admin | `fleet_git.v1` | no |
| `fleet_git.push_now` | deck → station | drive | `fleet_git.v1` | no |
| `project.ensure_home` | deck → station | admin | `fleet_git.homes.v1` | no |
| `project.home_status` | deck → station | read | `fleet_git.homes.v1` | no |
| `fleet_git.artifact_authorize` | station → deck | admin | `fleet_git.homes.v1` | no |
| `fleet_git.share_verdict` | deck → station | read | `fleet_git.homes.v1` | no |
| `project.radar` | deck → station | read | `fleet_git.land.v1` | no |
| `session.land` | deck → station | drive | `fleet_git.land.v1` | yes |
| `fleet_git.land_status` | deck → station | read | `fleet_git.land.v1` | no |
| `fleet_git.land_abandon` | deck → station | drive | `fleet_git.land.v1` | no |

- **`workspace.list` (added in M2 P4-4, DL-061, under the extension rule of §3: "every new method must declare a scope and, if needed, a feature"):** the directory picker of the board's spawn form. It lists the sub-directories of a path inside the station's allowed roots (`WorkspaceQuery` → `WorkspaceListing`). It reads only, but it needs `drive` because only a caller that may start a session needs the station's directory names. Errors: `missing_workspace`, `outside_roots`, `timeout`.
- **`workspace.worktrees` (DL-142; feature `workspace.worktrees.v1`):** `workspace.worktrees` (DL-142) is read only: it lists the worktrees that the station created for sessions, with what Git reports about each (dirty and ignored entries, commits on no fetched remote ref, registration, last commit). It runs no Git command that writes.
- **`workspace.preflight` (DL-144; feature `workspace.preflight.v1`):** params `{url}` (`PreflightQuery`), answer `{ok, message, hint}` (`Preflight`). The station checks the URL like a clone URL (https, ssh, scp-like, or `file://` inside the allowed roots; no credentials in the URL), then runs `git ls-remote --exit-code --heads` with its own Git credentials, no prompt and a 20 s limit. An empty remote counts as `ok`. A failed check is a normal answer with `ok: false`, the scrubbed end of the Git error in `message` (at most 2000 characters) and a `hint` for the usual causes. It reads only, but it needs `drive` for the reason of `workspace.list`. Errors: `invalid_url` (the URL has unusual characters or no host, or an unknown form), `forbidden` (credentials in the URL, an unknown transport, or a `file://` URL outside the allowed roots), `internal`.
- **`session.history` (DL-189; feature `history.v1`; scope `read`):** params `{session_id, before?, limit?, member?}` (`SessionHistoryQuery`), `limit` 1 to 100 (default 50), answer `{items, before, state}` (`SessionHistory`). The items are oldest first inside a page; `before` is the cursor of the next older page, `null` at the start. `state` is `complete`, `partial {skipped}`, `empty` or `unavailable {reason}`. The station reads only its own snapshot of the provider transcript, taken when the session was imported (design `docs/v2/design/session-history.md`); the caller gives no path. With `member: true` every tool `summary` is empty. An answer has at most 512 KiB. Errors: `invalid_params`, `unknown_session`, `stale_cursor`, `busy`.
- **`session.subagent` (DL-191; feature `subagent.v1`; scope `read`):** params `{session_id, tool_use_id}` (`SubagentQuery`; `tool_use_id` 1 to 128 of `A-Za-z0-9_-`), answer `{items, state}` (`SubagentLog`; `items` oldest first, at most 300, `state` as in `session.history`). The station finds the transcript from its own binding row of that tool use (the agent id comes from the `tool.completed` event of the provider stream, with the launch facts of the process), opens it without following a link, reads it once up to the size at open, and requires every record to name the parent session and the agent (`not_trusted` otherwise). A Codex session gives `unavailable {unsupported_format}`; a tool use without a binding gives `unavailable {not_found}`. The serialized answer has at most 512 KiB. Errors: `invalid_params`, `unknown_session`, `busy`, `internal`.
- **`session.branches` (DL-191; feature `session_branches.v1`; scope `read`):** params `{session_id}` (`BranchesQuery`), answer `{head, branch, branches, truncated}` (`RepoBranches`; `head` is `null` for an unborn `HEAD`, `branch` is `null` for a detached one; `branches` holds at most 200 `{name, head, ahead, behind, at, subject}` newest first, without the current branch). The station runs Git in the working directory of the session through a hardened runner (no pager, external diff, text conversion, lazy fetch or prompt), under one 10 s deadline, one request for a session at a time. A Git below 2.45 gives the error `git_too_old`. Errors: `unknown_session`, `git_too_old`, `not_a_repository` (the session folder is not a Git repository), `busy`, `internal`, `missing_workspace`, `outside_roots`.
- **`files.branch_diff` (DL-191; feature `session_branches.v1`; scope `read`):** params `{session_id, head}` (`BranchDiffQuery`; `head` is the full hex tip of a listed branch), answer a `SessionDiff` with `scope = committed` and `base_oid` the merge base with `HEAD`. `head` that is not the tip of a listed branch gives `invalid_params`. The serialized answer has at most 512 KiB (`truncated`). Errors as `session.branches`, and `invalid_params`, `no_common_history` (`merge-base` finds no common commit with `HEAD`).
- **`files.session_diff` (DL-159; feature `files.session_diff.v1`; scope `read`):** params `{session_id, max_bytes?}` (`SessionDiffQuery`), answer `{session_id, base_oid, summary, patch, truncated}` (`SessionDiff`). The station compares the working tree of the session (tracked content through the private snapshot index of the checkpoints, so no clean filter or external diff runs) with the base commit that `session.base` recorded (`git diff-tree -p -r -M --no-ext-diff --no-textconv`), then renders each untracked file that is not ignored as a new file (a binary or a large file is a header line only). Private paths (`.git`, credential directories) are left out. The patch has at most 256 KiB; a cut sets `truncated`. Errors: `unknown_session`, `no_base` (the session has no base commit, or the commit is not in the repository), `no_diff` (the directory is not a Git worktree, or its tracked content is out of the snapshot scope), `outside_roots`, `missing_workspace`, `timeout`.
- **`files.coordinator_diff` (ADR-008 §4.4, DL-183; feature `coordinator.v2`; scope `read`):** params `{session_id}` (`CoordinatorDiffQuery`, no other field), answer `SessionDiff`. It is the diff of a session for a coordinator, built per file; **no byte of the old side leaves the station**. Privacy comes first: a changed path with a private name or a member-deny match is dropped, the work-tree file is opened without following a link, the open file is checked (private registry, hard-link rule) and the new side is read from that descriptor. The old side (`<base>:<path>`) only feeds `git diff --no-index` over two copies in a private temp directory (mode 0700, removed after); the output is rewritten: each run of removed lines is one line `-[<n> lines removed]`, `@@` headers are rebuilt from numbers only, and no `index`, mode or similarity line is kept. A file shows as `(hidden: private file)`, `(hidden: diff unavailable)`, `(binary file)`, `(deleted file)` or `(renamed) <old> -> <new>`. Same limits as `files.session_diff`. Errors: those of `files.session_diff` and `privacy_unknown` (an identity check could not run; the whole query fails).
- **`workspace.precheck` (DL-168; feature `workspace.precheck.v1`):** params `{cwd, cmd}` (`PrecheckQuery`), answer `{exit_code, timed_out, output_tail}` (`PrecheckResult`). The station runs `cmd` under `/bin/sh -c` in `cwd` (inside the allowed roots) with the scrubbed station environment, in its own process group, with no input, and a limit of 60 s; on a timeout or when the answer is built, the group is stopped (TERM, then KILL). It keeps the last 4 KiB of the output (stdout and stderr together, scrubbed of URL credentials). A command that exits non-zero is a normal answer. It runs the owner's shell code, so it needs `admin` and a station with `allow_setup_cmd = true`. It is not a receipt mutation: the deck decides what a non-zero exit means (a skipped run). Errors: `unsupported` (`allow_setup_cmd = false`), `invalid_params` (an empty command, more than 4096 characters, or a NUL), `missing_workspace`, `outside_roots`, `internal` (the command did not start).
- **`assist.generate` (DL-167; feature `assist.v1`):** params `{kind, input}` (`AssistRequest`; `kind` is `title`, `digest`, `pr_text`, `issue_digest`, `race`, `conflict` or `plan_gist`; `input` has at most 24000 characters and is already fenced where it holds third-party text; for `digest` it is the session id and the station builds the text from the last 20 timeline lines), answer `{text, generated: true, cached}` (`AssistAnswer`, `text` at most 2000 characters). The station runs one short headless CLI call (`claude -p` with Haiku, or `codex exec`) on its own provider login, with a per-call output cap, a daily token cap and a cache by the hash of kind, model and prompt. A `digest` also stores the text with the event `session.digest`. It changes no session, so it is not a receipt mutation, but it spends the owner's tokens, so it needs `drive`. Errors: `disabled` (`[model_help] enabled = false`), `budget` (the daily cap is reached), `no_provider` (no signed-in provider instance), `invalid_params` (an empty or too long input, an unknown session for `digest`; an unknown `kind` is a `protocol_error`), `failed` (the call failed, timed out or returned nothing). A caller always has a plain fallback; none of these is shown as an error state.
- **`settings.get`, `settings.set`, `settings.secret_set`, `station.restart` (DL-172; feature `settings.v1`):** the settings of a station. `settings.get {}` answers `SettingsView`. `settings.set {change, elevated}` (`SettingsSetParams`; `change` is a `SettingsChange` without `elevation`) answers `SettingsSaved`. `settings.secret_set {secret, revision, elevated}` (`SecretSetParams`) answers `SettingsSaved`: it is the credential relay (ADR-006 doctrine rule 5, DL-173), so the value crosses the link once, is never stored by the deck, never goes through `commands`, the outbox or receipts, and is never logged. `station.restart {stop_direct_sessions, elevated}` (`RestartRequest`) answers `{restarting, would_stop}` (`RestartAnswer`; `would_stop` is a list of `{kind: session|shell|setup, id, title}`; when `restarting` is false, `would_stop` is not empty). The deck checks the owner's elevation and passes `elevated`; the station refuses a danger change or a restart without it. An error answer has a `code` equal to the `SaveError` code (`settings_changed`, `settings_changed_on_disk`, `invalid`, `cannot_apply`, `settings_write_failed`, `secret_store_unavailable`, `secret_via_relay`, `elevation_required`) and, when the code has keys, `data` is `{"keys": [..]}`. A station without the handlers answers `unsupported`. None is a receipt mutation. They need `admin`.
- **`files.write_temp` (DL-147; feature `files.write_temp.v1`):** params `{upload_id, ext, data_base64}` (`PasteImage`; `upload_id` is 8 to 64 characters of `[A-Za-z0-9_-]`, `ext` is `png`, `jpg`, `gif` or `webp`), answer `{path}` (`PastedImage`, the absolute path). The station decodes the data (1 byte to 700 KiB), checks that the magic bytes match `ext`, and writes the file `<paste dir>/<first 32 hex of sha256(upload_id)>.<ext>` with mode 0600 in a directory with mode 0700. It keeps at most 49 files and 24 MiB there and deletes files older than 24 h. It is not a receipt mutation: the upload id makes a retry return the same path (the same id with the same bytes answers the stored path). It needs `admin` because it writes to the station. Errors: `invalid_params`, `too_large`, `conflict` (the id was used for another image), `no_space` (less than 512 MiB free), `unsupported` (the station has no paste directory), `internal`.
- **`session.worktree_trash`, `session.worktree_restore`, `session.worktree_purge`, `session.worktree_release` (DL-150; feature `workspace.trash.v1`):** commands `worktree_trash { session_id }`, `worktree_restore { session_id }`, `worktree_purge { session_id, confirm_name }` and `worktree_release { session_id }`. They move and delete folders, so they need `admin`, and the station accepts them only with origin `board` (else `forbidden`). The receipt is only the admission; the outcome is the state of the `worktree_ops` row, which `workspace.worktrees` returns as `WorktreeInfo.trash` (`trashing`, `trashed`, `restoring`, `purging`, `failed` with a `phase` and a `message`) and the verdict `trashed`. A trash moves the folder of an ended session to `<parent>/.fleetdeck-trash/<name>--<12 characters of the command id>` and runs `git worktree repair`; nothing is deleted and branches are never deleted. A purge needs `confirm_name` equal to the trash folder name. A release is accepted only for a `failed` row and changes nothing on disk. **Custody hold:** while a row is open (any state but `restored`, `purged`, `released`), it holds the original folder and the trash folder (and everything below them): `session.start` (also a clone or worktree start whose path overlaps), `session.resume`, `session.import`, the auto-continue recheck, `shell.open` and `shell.handover` refuse with `invalid_state` "the worktree is in the trash or being moved; restore it first". **Claim guard:** the file work runs as an effect without a provider process; its first step reads the row and goes on only when the row is in the state that its action expects (`trashing`, `restoring`, `purging`). A stale effect ends `superseded` and changes nothing, so a release or a second command cannot let an old purge delete files. The folder is identified by its device and inode number from the trash decision; a replaced folder is never moved or deleted.
- **`session.execution_check`, `session.reconcile_execution` (DL-182; feature `shim.resume.v1`; design `docs/v2/design/shim-resume.md` §4.2):** `session.execution_check { session_id }` (scope `read`, `ExecutionCheckQuery`) answers `ExecutionCheck { shim_id, reason, same_boot, identity_alive, group_members, session_members, escaped, complete, error }` for the newest shim execution of the session whose end is not proven (`shim_id: null`, `reason: "nothing unresolved"` when there is none). A process is `{pid, command}`; `command` is the first 120 characters of `/proc/<pid>/comm`, never the arguments (`?` when unreadable). On another boot (`same_boot: false`) the answer has no members and `complete: true`: the reboot proof covers it. On a platform without a process reader it has `complete: false` and `error: "process inspection is not available on this platform"`. `escaped` lists, for information only, processes outside the group and the session that descend from the provider or its shim; it never blocks. The check kills nothing. `session.reconcile_execution { session_id, shim_id }` (scope `admin`, mutating, a receipt) is a `SessionCommand` of origin `board` or `local` (another origin is `forbidden`). The station runs the check again before it decides. It refuses (`invalid_state`) while the identity or any group or session member lives, or when the check is incomplete; an unknown `shim_id` is `invalid_command`. Accepted, it records the proof `reconciled_by_owner` on that row and appends `session.execution_reconciled { shim_id }`.
- **`artifacts.publish`, `artifacts.list`, `artifacts.get`, `artifacts.file` (DL-165; feature `artifacts.v1`):** the artifact service of R12. None is a receipt mutation. `artifacts.publish { session_id?, path, title?, artifact_id?, author? }` copies the file at `path` (absolute, or relative to the session's working directory) into `<station data>/artifacts/<artifact_id>/<version>/` and answers `{ artifact, version, created }`. The path must lie inside the session workspace or an allowed root; the station opens every component with no-follow, so a symlink is never read. An HTML file is bundled with the local files that it names through relative `src`, `href` and CSS `url()` (one level, below the file's own directory, no `..`, no absolute path, no scheme, no symlink). There is no size cap. With `artifact_id` the call makes the next version; content equal to the latest version makes no new version (`created: false`), so a retry is safe. `artifacts.list { session_id? }` answers `{ artifacts }` newest first. `artifacts.get { artifact_id, version? }` answers `{ artifact, version, versions }`. `artifacts.file { artifact_id, version, path, offset?, length? }` answers one chunk `{ path, size, offset, length, data_base64, eof }` of at most 700 KiB of raw bytes; the caller repeats it with the next `offset` until `eof`. Errors: `invalid_path`, `invalid_params`, `not_found`, `unknown_session`, `outside_roots`, `unknown_artifact`, `unsupported` (a station without an artifact directory).
- **`accounts.v1` (DL-173; design `settings-and-accounts.md` section 2):** the accounts of a station. Types are in `fleetdeck-proto::accounts`. `accounts.list {}` answers `{accounts, apps, env_tokens}`. `accounts.forge_login {kind, host}` starts a `gh` or `glab` device login and answers `{login_id, code, url, expires_in}`; `accounts.forge_login_status {login_id}` answers the state (`pending`, `done`, `failed`, `expired`). `accounts.logout {account_id}` answers `{}`. `accounts.app_set {kind, client_id, client_secret}` answers an `AppView`. `accounts.token_set {kind, site?, email?, token}` answers an `AccountView`. `accounts.oauth_start {kind, state, redirect_uri}` answers `{authorize_url}`. `accounts.oauth_finish {state, code}` answers an `AccountView`. `issues.containers {account_id}` answers `{containers: [{key, name}]}` (scope `read`). **Credential relay:** `accounts.app_set`, `accounts.token_set` and `accounts.oauth_finish` carry a secret value once, in memory, inside one request and its answer. They are not receipt mutations: the deck never stores their params, never puts them in the command store, the outbox or a receipt, and never logs them. The answers hold metadata only. A lost answer means that the owner types the value again. Errors: `invalid_params`, `unsupported`, `forbidden`, `unavailable`, `env_token_present`, `unexpected_cli_output`, `device_flow_unsupported`, `internal`.
- **`integrations.v1` (DL-164; R3):** the station reads and writes GitHub, GitLab, Jira and Linear with its own credentials (`gh api`, `glab api`, and `curl` with a stdin config for Jira Cloud and Linear; the token is never in argv or in the environment of a child). The deck relays answers and holds no token. Params and answers are the types of `fleetdeck-proto::integrations` (`IssueSearch`, `IssueGet`, `AssetQuery`, `PrsList`, `PrRef`, `IntegrationsStatus`, ...). Every third-party text in an answer (issue and pull request bodies, comments, CI log tails) is inside the envelope of `fleetdeck_proto::untrusted::fence`. A list answer has a station cache of 60 s. Errors: `invalid_params`, `unavailable` (no such tracker, no such CLI, no token), `upstream` (the system refused; a scrubbed text), `timeout` (20 s per call), `outside_roots` (`prs.create`, `prs.fetch_head`), `bad_answer`, `failed` (a `git` call of `prs.fetch_head`). `prs.fetch_head` (R3.7) fetches the head ref of a pull request or merge request, also from a fork (`refs/pull/<n>/head`, `refs/merge-requests/<n>/head`), into `refs/fleetdeck/pr/<n>` of the repository at `path` (no tags, no `FETCH_HEAD`, no prompt, 60 s) and answers `{ref_name, oid}`; it is not a receipt mutation and writes no forge object. The five write methods are not receipt mutations (no envelope row): each carries a `command_id` in its params; the station keeps the answers of the last 500 writes for one hour in memory, so a repeated id with the same request returns the stored answer `{url, actor}` and writes nothing, and the same id with another request is `invalid_params`. A write records one `tracing` line (method, target, actor, result URL; never the text). `issues.asset` answers only a PNG, JPEG, GIF or WebP of at most 5 MiB, from a host of the binding.
- **`files.list`, `files.read`, `files.search`, `files.diff` (added in M3, DL-088, under the extension rule of §3; feature `files.v1`):** the read-only file service of PLAN §6.6. A query names its root as `{"session": <session id>}` (the session's working directory) or `{"workspace": <path>}`; the station resolves the root and checks the allowed roots again on every call. Paths are relative, `/`-separated, without `.`, `..`, `\\` or NUL. The station never follows a symlink below the root, never opens a FIFO or device, and never lists, reads or searches `.git` or credential directories (`.ssh`, `.aws`, `.gnupg`, `.kube`, `.docker`, `.netrc`). Every answer is bounded (1000 entries, 256 KiB read, 200 hits, 256 KiB diff) and reports a cut with `truncated`. `files.diff` takes `{session_id, turn_id}` and returns the patch between the start and completion checkpoints of that finalized turn (PLAN §6.6); a provider's own diff is never used. They read only and need `drive` for the reason of `workspace.list`. Errors: `invalid_path`, `invalid_query`, `not_found`, `wrong_kind`, `unknown_session`, `missing_workspace`, `outside_roots`, `no_diff`, `busy`, `timeout`.
- **`shell.list`, `shell.attach`, `shell.open`, `shell.resize`, `shell.close` (added in M3, DL-404, under the extension rule of §3; feature `shell.v1`):** the human shell of PLAN §6.8. `shell.open { shell_id, cwd, cols, rows }` starts the owner's login shell in a PTY; the deck chooses the shell id; the station checks `allow_shell`, `max_shells` and the allowed roots and scrubs the environment. `shell.attach { shell_id }` returns a `stream_token`; the deck opens a data stream with it (purpose `shell:<shell_id>`), and the station sends the scrollback (at most 5,000 lines and 8 MiB), then the live output; the bytes that the deck writes on the stream are the keystrokes. Keystrokes are never on the control channel and are not audited. `shell.close` ends the shell. Shells belong to the station, not to a link: a lost link or deck does not end them; a station restart does (A10). Errors (receipt rejects): `unsupported` (`allow_shell = false`), `invalid_command` (a bad field, a cwd outside the allowed roots), `invalid_state` (`max_shells` reached, a used shell id, an unknown shell). `shell.attach` errors: `unsupported` (no data channel on the link), `invalid_state` (no such shell); a stream for an unknown shell is reset with `unknown_shell`.
- **`shell.handover`, `shell.release` (DL-140; feature `shell.handover.v1`):** `shell.handover` and `shell.release` (DL-140) run a provider TUI on a stopped session and clear its terminal hold.
- **Coordinator (G7):** `coordinator.call` carries the operations that change fleet intent (spawn, send, stop, assign). It is mutating: it needs a `command_id`, and the **deck** keeps receipts for it and deduplicates them with the same rules as station receipts (same id + method + caller + payload hash → the stored receipt; reused id with another payload → rejected). `coordinator.query` carries the read operations and is not mutating. This replaces the Revision 1 row "`coordinator.call` | coordinate | — | no".

### 9.7 Implementation items of the same gate (G8–G12)

These do not change the wire contract; `fleetdeck-link` implements them with one test each (PROGRESS P3 items): socket writes and delivery outside the cancellation `select`, close bounded to 1 s, heartbeat ping every 15 s and close after 45 s without a frame (G8); a reset fails pending writes at once (G9); generation-aware table cleanup (G10); invitation failures counted per invitation id (5 failures cancel only that invitation), at most 10 pairing attempts per source IP per minute and 4 concurrent pairing handshakes (G11); invitations never in argv, read from a non-echoing prompt, stdin or `--invite-file` (mode 0600), `Secret<T>` and redacted everywhere else (G12).

### 9.8 New reason codes

`superseded` (now also for reciprocal links), `stale_link_epoch` (with `min_epoch`), `flow_control_violation`, `too_many_requests`, `bad_stream_token`, `wrong_role`, `revoked`, `incarnation_mismatch` and `expired` (receipts), `invalid_command`.

## 10. Pairing requests (DL-178, feature `pair_request.v1`)

Design: `docs/v2/design/tailnet.md` section 5. This section is the wire contract.

### 10.1 Frames

All four are JSON text frames on `/fdlink/v1/pair`, after the normal handshake (§2.5, both keys proven, epoch 0). A node sends them only when both sides negotiated `pair_request.v1`; a peer without the feature gets `unsupported` and nothing is stored.

```text
pair_request  { request_id, name, nonce, offered_scopes }   station -> deck
pair_pending  { request_id }                                deck -> station
pair_approved { request_id, granted_scopes, signature }     deck -> station
pair_resume   { request_id, nonce }                         station -> deck
close         { reason, detail }                            either way, reasons of §2.11
```

- `request_id`: 16 random bytes made by the station, 32 lowercase hex digits. The station writes its retry file (`<station data>/pair-pending.json`, mode 0600, file and directory synced) before it sends `pair_request`.
- `name`: 1 to 64 characters, no control character and no text direction character (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069).
- `nonce`: 32 random bytes, 64 lowercase hex digits.
- `offered_scopes`: what the station grants to the deck on the station. Only `read`, `drive`, `approve`, `admin` (never `coordinate`).
- `granted_scopes`: what the deck grants to the station on the deck. It is always `["admin"]`; it is not editable.
- `signature`: Ed25519 by the deck link key over the approval message of §10.3, 128 hex digits.
- A deck refuses a request whose `request_id` it already holds with another key or another canonical request (`request_id_conflict`). A second request of a key that has one waiting is `busy`; the old connection stays. A key that is already paired is `already_paired`.
- A resume is a fresh handshake with the same station key. The deck answers with `pair_approved` over the **new** transcript from its stored completion record, only when the record exists, the proven key, `request_id` and `nonce` equal it, and the key is still paired as a station; else it closes with `unknown_request` or `revoked`. A resume never creates or changes a record, and it is accepted on any link listener without the window.
- Station-side code `not_accepted`: the deck ended the connection after TLS and before it answered (a closed window, or a deck without `pair_request.v1`). It is retryable: the retry file stays and the same command resumes it. Only an answer frame of the deck (`window_closed`, `denied`, ...) ends a request.
- Messages of a key that is not pinned are at most 16 KiB on a tailnet listener. A waiting connection gets a WebSocket ping every 15 seconds and closes after 45 seconds without a pong.

### 10.2 Encodings

Every field is prefixed with its length as 8 bytes, big endian ("lp"). A scope list is sorted by name and joined with `,` (ASCII).

```text
request digest  = SHA-256( "fleetdeck pair-request v1"
                           || lp(request_id, 16 bytes) || lp(name, UTF-8) || lp(nonce, 32 bytes)
                           || lp(offered scopes) )

pending digest  = SHA-256( "fleetdeck pair-pending v1"
                           || lp(request digest, 32 bytes) || lp(station fingerprint, 32 bytes)
                           || lp(source IP, text of the canonical form) || lp(admitted_at, Unix ms as decimal text) )
```

The pending digest is what the board shows as `digest`; the owner sends it back with the approval, so an approval names the record that the owner saw.

### 10.3 The signed approval

```text
message = "fleetdeck pair-approved v1"
          || lp(transcript hash, 32 bytes)      SHA-256 of the handshake transcript of this connection (§2.5 step 4)
          || lp(station fingerprint, 32 bytes)  SHA-256 of the raw station link key
          || lp(deck fingerprint, 32 bytes)
          || lp(request_id, 16 bytes) || lp(nonce, 32 bytes)
          || lp(request digest, 32 bytes)
          || lp(offered scopes) || lp(granted scopes)
signature = Ed25519(deck link key, message)
```

The station checks the signature with the deck key that TLS proved (and that the person confirmed by its fingerprint), against its saved request (never against the current station name). The deck commits the station, its grants and a completion record in one transaction **before** it sends `pair_approved`.

### 10.4 Test vectors

Keys come from the seeds `01` x 32 (station) and `02` x 32 (deck) (`Identity::from_seed`). The request is `request_id` = bytes `00..0f`, `name` = `laptop`, `nonce` = bytes `20..3f`, offered scopes `read,drive` (sorted: `drive,read`), granted scopes `admin`. The transcript is the text `fleet-deck link transcript v1\0 test vector`; the source IP is `100.64.0.7`; `admitted_at` is `1800000000123`. The unit test `pair_request_tests.rs` checks each value.

```text
station fingerprint  34750f98bd59fcfc946da45aaabe933be154a4b5094e1c4abf42866505f3c97e
deck fingerprint     6a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f03827
transcript hash      e09327588059314b411bb09d84d6b410f5a61f0022bbfcf50d1541f286c6e547
request digest       e79c65b4306975d100484529afbf9911cd036698d4fab06c91fcee53b50e595f
pending digest       5e177bae222802c44477051778076b22000a351c4087e6e692a5c423634ab91a
approval message     666c6565746465636b20706169722d617070726f7665642076310000000000000020e09327588059314b411bb09d84d6b410f5a61f0022bbfcf50d1541f286c6e547000000000000002034750f98bd59fcfc946da45aaabe933be154a4b5094e1c4abf42866505f3c97e00000000000000206a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f038270000000000000010000102030405060708090a0b0c0d0e0f0000000000000020202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f0000000000000020e79c65b4306975d100484529afbf9911cd036698d4fab06c91fcee53b50e595f000000000000000a64726976652c72656164000000000000000561646d696e
signature            a57391c1a69ce4e16613b87450f746786a8b05addfa33a74976dff0fdda2dac6c5f2436c6eba354b1ff700ca981af503cfc977840238b66807c01ed4e0ad7f06
```

### 10.5 Tailnet link listeners

A link listener bound to a Tailscale-range address (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`) has stricter rules. LAN and loopback listeners never accept a request without an invitation.

- The window: the owner opens it for 10 minutes (new generation, fixed deadline, memory only). An unknown client key passes TLS only while an invitation or the window is open; the gate is checked again when the request is admitted.
- 64 unauthenticated connection permits (16 for one source, an IPv6 source counted by its /64 prefix) from accept to close; at most 32 connections in TLS or in the upgrade. Stage deadlines: TLS 5 s, upgrade 5 s, the whole `Hello`/`Auth` exchange 5 s. A refused connection is closed at once after its refusal frame.
- At most 16 waiting requests, 1 for each key, 2 for each source, and 30 new requests a minute; a request expires at the earlier of 10 minutes and the deadline of the window that admitted it; a denied key waits 10 minutes.
- A request that carries an `Origin` header, or a `Host` other than `node.fleet-deck.invalid`, gets HTTP 403 on every link listener (a browser is not a link peer).
