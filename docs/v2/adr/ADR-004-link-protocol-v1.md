# ADR-004 — Link protocol v1

- **Status:** Proposed
- **Date:** 2026-10-11
- **Deciders:** owner (approval pending), architect (goal loop, M1)
- **Implements:** PLAN §8 (link protocol), D13 (network scope), L15 (scoped methods), L16 (feature flags), §14 Phase 0 row S4
- **Evidence:** `spikes/s4-link/` (crate `s4-link`), tests in `spikes/s4-link/tests/` and module tests in `spikes/s4-link/src/`
- **Verified against:** rustls 0.23 (ring provider), tokio-rustls 0.26, tokio-tungstenite 0.30, rcgen 0.14, ed25519-dalek 3, Rust 1.96, loopback only (no Docker, no Tailscale)

## 1. Context

A Fleet Deck **link** connects one deck and one station. Either side can dial: the station dials the deck when the station is behind NAT, and the deck dials the station when the station has a listener. After the link is up, the direction does not matter: both sides send requests (§8.1).

PLAN §8 fixes the shape: two WebSockets (control and data) over mutual TLS with pinned Ed25519 keys, a `Hello` exchange with versions and feature flags, a transcript signature bound to the TLS session, `link_epoch` for duplicate links, the control frames of §8.3, scoped methods, single-use `fdinv1:` invitations, and jittered backoff. PLAN leaves several details open. The spike `s4-link` makes them concrete and tests them. DL-016 to DL-023 log every choice that PLAN does not state.

This ADR describes link protocol **v1** as the spike implements it. Phase 3 takes it into `crates/link`. The PLAN requires an adversary DESIGN REVIEW of §8 before Phase 3 freezes the protocol. The goal loop cannot run that review (§8 of this ADR).

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

Every step after TLS has the bound `NodeConfig::handshake_timeout` (default 10 s, DL-023). This includes the WebSocket upgrade on both sides. A timeout gives the reason `timeout`.

**Feature flags of v1** (`hello::feature`):

| Flag | Meaning when both sides offer it |
| --- | --- |
| `control.v1` | Control frames of §2.6 (required) |
| `data_streams.v1` | The data channel and the methods that need it (`artifact.open`) |
| `command_receipts.v1` | `command.receipt` (§8.4 receipts, Phase 3) |
| `snapshots.v1` | `Snapshot` frames and `session.snapshot` |

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
  2. The listener accepts the unpinned joiner key at the TLS level only while an invitation is open (`PinnedOrInvited`). An unpinned key on the control or data path gets `not_paired`.
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
| `unknown_method`, `feature_not_negotiated`, `forbidden`, `missing_command_id` | request errors in a `response` |
| `timeout` | any bounded step |

## 3. Failure behaviour

| Failure | Behaviour | Evidence |
| --- | --- | --- |
| Peer stalls after TLS (no upgrade, no hello) | The step fails with `timeout` after `handshake_timeout`. The listener keeps accepting other dialers, and the dial supervisor retries with backoff. | `tests/timeouts.rs` |
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
| Stalled peers do not block setup | 2.5 | `timeouts::a_dial_to_a_server_that_never_upgrades_times_out`, `timeouts::a_client_that_never_sends_the_upgrade_does_not_block_the_listener` |

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
- **Key rotation (§8.5)** is not implemented in the spike: "a paired node signs a new key with its old key". Phase 3 adds a `link.rotate_key` method (scope `admin`) that carries the new key signed by the old key.
- **In-memory invitations:** a restart of the inviter drops the open invitations. This is safe (fail closed), and the owner makes a new invitation.
- **No real tailnet test:** the Tailscale ranges are unit-tested only.

## 7. Phase 3 work that this ADR does not cover

The `command_id` receipt store and the `ambiguous` state (§8.4), the event replay with `station_seq`/`ack` and snapshots across reconnects (§7), station names, key rotation, LAN discovery suggestions, and persistent grants with owner-chosen scopes.

## 8. Owner open points

1. PLAN Phase 0 asks for an adversary DESIGN REVIEW of ADR-004. The goal loop forbids subagents, so the review has not run. Run it before Phase 3 freezes the protocol.
2. A real tailnet check (two nodes on Tailscale addresses) is an owner step.
3. Approve DL-016 to DL-023, especially the spike scope names (DL-019), the pair channel and IP-literal invitations (DL-021), and the wall-clock epoch (DL-022).
