# Tailnet: detect, list and pair (DL-178)

- **Status:** Proposed (revision 4, 2026-10-14). Revision 3 (commit a5d53fd6) got RE-REVIEW FAIL (2 blockers; `/tmp/adversary-report-a5d53fd6.md`); the round limit was reached and, following the owner's choice for over-limit designs, revision 4 applies Codex's replacement sentences for one extra focused review (§12). Earlier: Revision 2 (commit 2a061973) got RE-REVIEW FAIL (3 blockers, 1 major; `/tmp/adversary-report-2a061973.md`); §11 maps them; this is the last review round allowed for this design. Earlier: Revision 1 (commit 06a3feb3: a station discovery endpoint authorized by `tailscale whois`) got DESIGN REVIEW FAIL (7 blockers, 5 major; `/tmp/adversary-report-06a3feb3.md`). The advisor (Codex task `task-mv79r56g-cfkyf4`) recommended a station-initiated pair request on the existing authenticated pair channel; revision 2 adopts it with the advisor's changes. §10 maps the findings. Needs a PASS before code.
- **Owner request (2026-10-14):** "lets add full support for tailnet autodetect, and auto configure auto discovery"; owner choice "Detect, list, 1-click pair": nothing pairs by itself.
- **Change to the owner's workflow (stated plainly):** the deck cannot know which unpaired tailnet devices run a station without an unauthenticated probe, which revision 1 showed to be unsafe. So the board lists the devices that the deck's Tailscale client sees, and pairing starts on the station with one command that the board generates; the owner then approves on the board with one click (plus elevation). The station command is the station owner's consent.

## 1. Facts

- Pairing today: invitation (address, inviter link-key fingerprint, id, 16-byte secret, expiry) made by one side, pasted into the other; the joiner dials `/fdlink/v1/pair` pinned to the fingerprint; both run the handshake (both keys proven, transcript bound to TLS); `PairProof { invitation, proof }` → the inviter redeems, stores grants, pins → `Paired { proof }` (HMAC with the invitation secret) → the joiner pins (`crates/fleetdeck-link/src/join.rs`, `pairing.rs`, `hello.rs:132`). `PairLimiter`: 10 attempts per IP per minute, 4 handshakes at a time. TLS admission of unknown keys depends on `invites.has_open()` (`tls.rs:123`); pair-channel dispatch on `node.invitations.is_some()` (`node.rs:380`).
- The link fingerprint is SHA-256 of the raw Ed25519 link public key (`docs/v2/adr/ADR-004-link-protocol-v1.md:52`). It is not the board's HTTPS certificate hash. Every UI and CLI text in this design says "link key fingerprint" and shows all 64 hex characters.
- `netscope.rs` classifies addresses only; Tailscale identity is never authentication in this design.

## 2. Tailscale as a display source

- The `tailscale` CLI only: `tailscale status --json` (and `tailscale whois --json <ip>` for display). Executable: `tailscale` from `PATH`; on macOS also the app-bundled CLI (`/Applications/Tailscale.app/Contents/MacOS/Tailscale`, run with `TAILSCALE_BE_CLI=1`); or the setting `tailscale_bin` (absolute path).
- `tailscale_bin` is a **danger** setting in both holders (deck and station): a change needs elevation or the owner-only local socket, is listed in the holder danger comparisons, and clears every cached Tailscale answer.
- Each call: the child gets an environment built from `ChildEnv::empty()` plus only `PATH`, `HOME`, `LANG`, and on macOS `TAILSCALE_BE_CLI=1`; stdin null; 3 s timeout; stdout ≤ 2 MiB and stderr ≤ 64 KiB (more → error, child killed); `kill_on_drop` and reaped; stderr never logged beyond 512 bytes.
- Parsing: two separate typed schemas (`Status`, `WhoIs`) with only the fields used: `BackendState`, `Self { ID, HostName, DNSName, TailscaleIPs, UserID, Tags, OS }`, `Peer { … same …, Online }`, `User[id].LoginName`, `CurrentTailnet.MagicDNSSuffix` (fallback `MagicDNSSuffix`); `WhoIs { Node { ID, Name, Addresses, User, Tags, Sharer? }, UserProfile { ID, LoginName } }`. Unknown fields ignored; a non-zero exit, `BackendState != "Running"`, missing required fields, duplicate keys or invalid addresses → "Tailscale not available: <reason>". DNS names lose the trailing dot. Both IPv4 and IPv6 are supported.
- **Userspace networking:** when the tailnet addresses from `status` are not assigned to a local interface (checked against the OS interface list), the deck reports "Tailscale runs in userspace networking: automatic listen addresses are not available; configure them by hand" and offers no "Use tailnet".
- Display data (whois, sharing, tags) is labelled informational; a missing sharing field shows "unknown", never "not shared". Nothing in this design authorizes on it.

## 3. Use tailnet (deck reachability)

- `GET /api/tailnet` (owner, admin scope): `{ available, reason?, self: { dns_name, ips, login, os }?, managed: { listen, link_listen, tailnet_hostname } }`; computed per call.
- Board → Settings → Deck → "Tailnet" card: "Use tailnet" (or "Update" when the managed addresses differ from the current tailnet addresses). It is a normal elevated settings save (DL-172) that shows the exact changes first:
  - The deck stores which `listen` and `link_listen` entries it manages (new deck key `tailnet_managed`, a list of socket addresses; not editable by hand in the board).
  - "Use"/"Update" removes the previously managed entries, adds `<tailnet IP>:<port>` for each current tailnet IP (IPv4 and IPv6, bracketed for v6) with the port of the first existing entry of that list (7420/7421 by default), deduplicates, keeps every unmanaged entry, and sets `tailnet_hostname` to `Self.DNSName`. Each new address must be assigned to a local interface at save time.
  - `listen`/`link_listen` apply at restart: the card says "Saved; listeners start after a restart" until the running process reports the listener active (the settings view's restart flag, DL-172).
- `fleetdeck tailnet status` prints the same facts on any machine.

## 4. Devices list (deck)

- `GET /api/tailnet/devices` (owner, admin scope): the deck's `tailscale status` peers (online and offline), each with host name, DNS name, IPs, OS, login, tags, and a pairing state from the deck's own store: `paired` (a paired station whose last link came from one of the device's IPs — labelled "probably this device", not proof), `request waiting` (§5), or `none`. No network probe.
- Board → Stations → "Add station" → tab "On your tailnet": the list; for a `none` device the row shows the station command (§5.2) with a copy button.

## 5. Pair request

### 5.1 Deck: the request window

- Board → "Accept pairing requests for 10 minutes" (owner, admin scope, CSRF, **elevation**); "Close" (owner, CSRF, no elevation). The window applies only to the deck's **tailnet** `link_listen` addresses (the managed ones); LAN and loopback link listeners never accept invitation-free pairing. A restart closes it. Closing cancels every waiting request. Each opening creates a new **window generation** (a counter) with a fixed deadline (opened + 10 minutes).
- Unknown client keys pass TLS admission on a tailnet link listener only while an invitation is open or the window is open; the gate is checked again after the handshake and at request admission (a handshake that began before the window closed does not create a request after it).

### 5.2 Station: `fleetdeck station join --tailnet`

- The board shows, while the window is open, the exact command:
  `fleetdeck station join --tailnet <deck dns name or IP>:<link port> --deck-fingerprint <64 hex link key fingerprint> [--scopes <list>]`
  The fingerprint is public, so argv is fine. The deck's own board is the reference: the owner must reach the board over an already trusted connection (the board certificate accepted earlier, or locally on the deck machine).
- Without `--deck-fingerprint`, the command does an **observation-only** probe: a TLS connect with a separate observing verifier (the normal dialer verifier, which refuses any key, is not changed), prints the full observed link key fingerprint, and asks "Does this match the link key fingerprint on the board? [y/N]". The probe writes nothing. On "y", the pair connection pins exactly that fingerprint and fails if the key differs.
- The target host must match a peer in the station's own `tailscale status`; the command dials that peer's exact `TailscaleIPs` (no general DNS resolution, no redirects, no port guessing).
- The station owner chooses the scopes that the deck receives on the station (default: the scopes that `fleetdeck station join` gives today; never coordinator authority unless named).

### 5.3 Wire (ADR-004 extension, negotiated)

- New optional `Hello` feature `pair_request.v1`. Frames are sent only when both sides negotiated it; an older deck answers as today (no open invitation → refused), and the station prints "This deck does not accept pairing requests; use an invitation".
- On `/fdlink/v1/pair`, after the normal handshake (both keys proven, epoch 0):
  - station → `PairRequest { request_id (16 random bytes, made by the station), name (1–64 chars, no control or bidi characters), nonce (32 random bytes), offered_scopes }`. The station generates `request_id` and durably writes the retry file (§5.4) **before** it sends the frame. The deck binds that `request_id` to the proven key and the immutable canonical request; a `request_id` that the deck already holds with another key or another canonical request is refused (`request_id_conflict`). No station id: the proven key (`Hello.node_id`) is the identity; a request never selects or replaces an existing station record.
  - deck → `PairPending { request_id }` (an echo of the station's id) at once, then later `PairApproved { request_id, granted_scopes, signature }` or a close with `denied` / `expired` / `cancelled` / `window_closed`.
  - `signature`: Ed25519 by the deck link key over a domain-separated canonical encoding (`"fleetdeck pair-approved v1"`, the handshake transcript hash, both full fingerprints, request id, nonce, SHA-256 of the canonical request, granted scopes in both directions). ADR-004 gets the encoding and test vectors. `Paired { proof }` (invitation HMAC) is not reused.
- The station verifies the signature, then stores the deck pin, its chosen grants and the dial target in one local transaction, then dials the link as after a normal join.

### 5.4 Deck: pending requests and approval

- **Unauthenticated connection permits:** every TCP connection that a tailnet link listener accepts takes one of 64 unauthenticated permits at accept, before TLS; the permit is held through TLS, the upgrade, the handshake, any refusal (busy, rate limit, unknown key on the control or data channel, window closed) and its cleanup, and is released when the socket closes, or transferred atomically to a pending-request slot when a request is admitted. With no permit free, the connection is closed at accept. Every stage has a deadline (TLS 5 s, upgrade 5 s, Hello 5 s); a refused connection is closed at once after its refusal frame, never left waiting. Established links (proven, paired keys) do not use these permits.
- Admission limits: TLS/upgrade setups ≤ 32 per listener with stage deadlines; the existing 4 handshake permits are **released before** a request waits; pending requests ≤ 16 in total, ≤ 1 per proven key, ≤ 2 per source IP; ≤ 30 new requests per minute globally plus the existing per-IP limit; pair-channel messages ≤ 16 KiB; a request is bound to the window generation that admitted it, and its expiry is `min(admitted + 10 minutes, that window's deadline)` (a duplicate never extends it); when full, refuse at once (no eviction); a denied key gets a 10-minute cooldown. Established links do not count against these limits. Pending connections have read/write deadlines and a heartbeat.
- Each pending request is an immutable record: the station's `request_id`, proven key fingerprint, nonce, name, offered scopes, source IP, admitted time, a digest of all of these; plus informational whois (fetched after admission, bounded concurrency, "identity unavailable" on failure).
- Routes: `GET /api/pairing/requests`; `POST /api/pairing/requests/{id}/approve { fingerprint, digest, granted_scopes }` (owner session, admin scope, CSRF, **elevation**; members and tokens refused even with admin); `POST …/{id}/deny` (owner, CSRF). Approve checks under one serialized decision: still pending, not expired, **the window that admitted it is still open and has the same generation** (a request of a closed or reopened window is never approvable, also when an expiry timer runs late), its connection still open, the submitted full fingerprint and digest equal the record's. It never approves by name, IP, Tailscale user or list position.
- The board shows the requested name (escaped, labelled "requested name"), the full link key fingerprint (to compare with what the station printed), the offered scopes and the informational Tailscale identity.
- **Commit, then answer:** approval commits, in one deck transaction, the station's key pin, its grants and an immutable **completion record** with the whole approval context: `request_id`, the request digest, nonce, requested name, offered scopes, granted scopes in both directions, the station key fingerprint, the deck key fingerprint, the window generation and the time. It then publishes the runtime trust and sends `PairApproved`. If the answer is lost, the board shows "approved; waiting for the station to connect" until an authenticated link from that key arrives.
- **Retry after a lost answer or a crash:** the station keeps its retry state in `<station data>/pair-pending.json` (mode 0600: deck address, deck fingerprint, the **complete original canonical `PairRequest`** — `request_id`, requested name, nonce, offered scopes — and its SHA-256 hash, plus the station's chosen scopes), written and fsynced (file and directory) before it sends `PairRequest`; it verifies a resumed `PairApproved` against this saved context, never against the current configuration (a renamed station still verifies its old request) and removed after it stored the pin or after `fleetdeck station join --cancel`. A retry is a fresh handshake with the same station key and a new frame `PairResume { request_id, nonce }` (feature `pair_request.v1`). The deck answers with `PairApproved` re-signed over the **new** transcript from the stored completion only when the completion exists, the proven key, `request_id` and nonce equal it, and the pairing is still current (not revoked, not replaced); else it closes with `unknown_request` or `revoked`. A retry never creates or changes a completion. A request that was never approved cannot be resumed (a disconnect cancels it). Approval never calls the existing "pair an existing key again" mutation that replaces grants and address (`crates/fleetdeck-deck/src/store/stations.rs:159`); a key that is already paired is refused (`already_paired`).
- A disconnect before approval cancels the request (serialized against approve). Pending requests are not resumable by nonce.

## 6. Browser defenses

- The link channel refuses requests with an `Origin` header and accepts only the `Host` value that the protocol's own dialer sends (`node.fleet-deck.invalid`, `crates/fleetdeck-link/src/ws.rs:117`); this is unchanged for invitation pairing and established links. A browser also has no link client key for the mutual TLS. Board routes keep their CSRF and owner checks. No board request triggers a probe or a pairing.

## 7. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| No Tailscale, or userspace networking | The card says why; manual pairing stays. |
| Window closed | The station prints `window_closed`; nothing is stored. |
| Key differs from the confirmed fingerprint | TLS fails; nothing is stored. |
| Approval answer lost | The station retries; it gets the stored completion re-signed; the deck never pairs twice. |
| Pending list full | Immediate refusal (`busy`). |

## 8. Tests

- Core: `Status` and `WhoIs` parsers with recorded documents (IPv4+IPv6, IPv6 only, tagged, shared, other user, offline, malformed, duplicate keys, non-zero exit, oversized output, timeout); the minimal child environment (credential canaries absent); macOS bundled CLI selection.
- Deck: `/api/tailnet` and `/api/tailnet/devices` owner-only; "Use"/"Update" replaces only managed entries, keeps manual ones, IPv6 bracketed, refuses an address not assigned locally; userspace mode detected; `tailscale_bin` is a danger change in both holders and clears caches.
- Link (revision 4): a crash of the station after writing the retry file and before receiving `PairPending`, while the deck approves: the restarted station resumes with the saved `request_id` and completes; a `request_id` reused with another key or request → `request_id_conflict`; both processes restarted and the station renamed in between → the resume verifies against the saved canonical request and completes.
- Link (revision 3): unchanged invitation pairing and control/data reconnects with the Host rule; the 64-permit cap with stalled Hello after a busy or rate-limit refusal and with unknown-key control and data upgrades; permit transfer to a pending slot; request expiry capped at the window deadline; approval after close, after close-and-reopen (new generation) and with a late expiry timer refused; `PairResume` after a deck crash between commit and answer (both processes restarted) re-signs the stored context; `PairResume` with another nonce, another key or a revoked pairing refused; the station's retry file removed after success and by `--cancel`.
- Link: `pair_request.v1` negotiation and refusal on an old peer; no request outside the window or on a LAN listener; window closed between handshake and request; per-key, per-IP, global and pending caps and the cooldown; handshake permits released while waiting; key substitution (different proven key than the approved fingerprint) refused before any write; stale approval (digest mismatch); duplicate name, key or nonce; disconnect/approve race; lost `PairApproved` and the re-signed retry; crash between commit and answer; revocation then a retry (refused); `PairApproved` signature vectors.
- Station CLI: `--deck-fingerprint` mismatch fails without writes; the probe path writes nothing before "y"; a host not in `tailscale status` is refused; atomic local store after a verified approval.
- Board: the window control with elevation, the request card (full fingerprint, escaped name, scopes), approve with elevation, the devices list and the copy command.
- Smoke (owner step, recorded in operations.md §12): real Linux and macOS machines, IPv4 and IPv6, a listener start failure.

## 9. Risks

- **Social engineering:** an attacker on the tailnet can send a request with a familiar name while the window is open. Defenses: the window, elevation, the full fingerprint comparison with the station's own output, and "requested name" labelling.
- **Availability:** limits bound resources but cannot prevent an attacker with many keys and addresses from filling 16 slots during a window.
- **Same-UID attacker** on either machine already controls it (ADR-006 §3).

## 10. Review map (revision 1 → revision 2)

| Finding (rev 1) | Change |
| --- | --- |
| B parser schema mismatch | §2 separate `Status`/`WhoIs` schemas; whois display only. |
| B shared nodes not excluded | §2/§5: Tailscale identity never authorizes; sharing shown as informational. |
| B whois cache survives tagging | No whois-based authorization at all. |
| B tailnet IP is not authenticated ingress | No station listener; pairing is by proven keys on the existing pair channel. |
| B DNS rebinding to the station endpoint | No station endpoint; §6 link-channel Origin/Host checks. |
| B fingerprint checked after trust | §5.2 pinned before connect; §5.4 approve checks the proven key's full fingerprint and digest before any write. |
| B `tailscale_bin` without elevation | §2 danger setting in both holders. |
| M deck pair route missing | §5.4 routes. |
| M ChildEnv blacklist | §2 `ChildEnv::empty()` allowlist. |
| M Update keeps stale addresses | §3 managed entries replaced. |
| M IPv6-only | §2–§5 both families. |
| M tests | §8. |

## 11. Re-review map (revision 2 → revision 3)

| Finding (rev 2) | Change |
| --- | --- |
| B Host check rejects existing clients | §6 accepts only `node.fleet-deck.invalid`, as the dialer sends. |
| B post-upgrade refusal connections uncapped | §5.4 64 unauthenticated permits from accept to close, stage deadlines, immediate close after refusal. |
| B approval ignores the window deadline | §5.1 window generation and deadline; §5.4 expiry capped and approval rechecks the window. |
| M completion lacks the signed context | §5.4 full completion record, `PairResume`, the station retry file. |
| M tests | §8 revision-3 link tests. |

## 12. Extra review map (revision 3 → revision 4)

| Finding (rev 3) | Change |
| --- | --- |
| B request id must exist before the send | §5.3 the station makes `request_id`, persists it, then sends; the deck binds and refuses conflicts. |
| B retry file lacks the signed context | §5.4 the retry file holds the complete canonical request and its hash; verification uses it. |
