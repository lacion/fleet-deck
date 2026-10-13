# Tailnet: detect, discover and pair (DL-178)

- **Status:** Proposed (revision 1, 2026-10-14). Needs an adversary DESIGN REVIEW PASS before code.
- **Owner request (2026-10-14):** "lets add full support for tailnet autodetect, and auto configure auto discovery". Owner choice: "Detect, list, 1-click pair": the deck finds its tailnet name and address, lists tailnet peers that run a station, and pairs one with one click; nothing pairs by itself.
- **Replaces:** the DL-131 deferral of station discovery (mDNS stays deferred; this is tailnet only).

## 1. Facts today

- Pairing (operations.md §6): either the deck makes an invitation (`fleetdeck deck invite`) and the station joins (`fleetdeck station join`), or the station listens (`listen` in station.toml) and makes an invitation (`fleetdeck station invite`) that the deck adds (`fleetdeck deck add-station`, or "Add station" in the board). An invitation is single-use and secret; the link pins the peer key (fleetdeck-link, ADR-004).
- The deck and the station refuse public listen addresses; loopback, private LAN and the Tailscale ranges (100.64.0.0/10, fd7a:115c:a1e0::/48) are allowed (`fleetdeck-core/src/netscope.rs`). Tailscale membership is "only an address range" there: nothing asks Tailscale who a peer is.
- `deck.toml` has `tailnet_hostname` (for the WebSocket `Origin` check). Nothing fills it in.

## 2. Tailscale as a source (read-only)

- Fleet Deck reads Tailscale only through the `tailscale` CLI of the machine: `tailscale status --json` and `tailscale whois --json <ip>`. No new dependency; no Tailscale API key; no LocalAPI socket access by path.
- The binary is `tailscale` from `PATH`, or the station/deck setting `tailscale_bin` (absolute path; `apply: Live`).
- Each call: 3 s timeout, stdout capped at 2 MiB (more is an error), stderr capped at 64 KiB and never logged above 512 bytes, the child gets a scrubbed environment (`ChildEnv`, no secrets), `kill_on_drop`.
- A missing binary, a stopped tailscaled (`BackendState != "Running"`), a parse error or a timeout gives "Tailscale not available" with the reason; nothing else changes.
- Parsed fields only: `Self.{DNSName, TailscaleIPs, UserID, Tags, HostName, OS}`, `Peer[*].{DNSName, TailscaleIPs, UserID, Tags, HostName, OS, Online}`, `User[id].LoginName`, `BackendState`, `MagicDNSSuffix`. Unknown fields are ignored. DNS names lose the trailing dot.

## 3. Detect and configure the deck

- New deck route `GET /api/tailnet` (owner only, scope `admin`): `{ available, reason?, self: { dns_name, ips, login, os }?, configured: { listen_has_tailnet, link_listen_has_tailnet, tailnet_hostname } }`. The answer is computed on each call (no cache) and never includes peers.
- Board, Settings → Deck: a "Tailnet" card. When Tailscale is available and the deck is not configured for it, it shows the name and the address and one button "Use tailnet".
- "Use tailnet" is a normal settings save (DL-172) with these changes, shown in the confirm dialog before the save:
  - `listen` += `<tailnet IPv4>:<port of the first listen address>`;
  - `link_listen` += `<tailnet IPv4>:<port of the first link_listen address, default 7421>`;
  - `tailnet_hostname` = `<Self.DNSName>`.
  A non-loopback `listen` is already a danger change: it needs elevation. `listen` and `link_listen` apply at restart; the dialog offers the restart (DL-172 restart rules, including the direct-session list).
- Nothing is configured automatically without that click. A changed tailnet address later shows the card again with "Update".
- `fleetdeck tailnet status` (CLI) prints the same facts for the deck and the station of this machine.

## 4. Discover stations

### 4.1 Station side (opt-in)

- New station settings:
  - `tailnet_discovery` (default `false`, `danger: true`, `apply: Restart`). On: the station serves the discovery endpoint (below). Turning it on needs elevation in the board of the deck that the station is paired with, or `fleetdeck settings set tailnet_discovery true --station` on the station machine (the local socket is the owner, DL-172).
  - `tailnet_discovery_port` (default 7422, `apply: Restart`).
- The endpoint listens only on the station's own tailnet addresses (from `tailscale status`, read at start), never on another address. If Tailscale is not available at start, the station logs one warning and serves nothing.
- Plain HTTP over the tailnet (WireGuard encrypts it). Limits as the coordinator ingress (ADR-006 §5.1): 32 connections, 4 unauthenticated in flight, header 5 s, body 10 s, body ≤ 16 KiB, keep-alive 15 s.
- **Caller check, before any answer:** the station runs `tailscale whois --json <remote ip>`. It accepts the caller only when the caller node is not tagged and its user id equals the station node's user id (same Tailscale user). Every other caller gets `403` with no body. The result is cached for 60 s per remote IP.
- `GET /fleetdeck/v1/hello` → `{ name, station_id, version, key_fingerprint, paired: bool, pairing_open: bool }`. `key_fingerprint` is the SHA-256 of the station link public key (the same value that pairing pins). No session, path or user data.
- `POST /fleetdeck/v1/pair` (§5).

### 4.2 Deck side

- New deck route `POST /api/tailnet/scan` (owner only, scope `admin`, CSRF): runs `tailscale status --json` on the deck machine, selects the peers that are online, not tagged and of the deck node's own user, at most 64, and calls `GET /fleetdeck/v1/hello` on the first tailnet IPv4 of each at `tailnet_discovery_port` (default 7422; the deck setting `tailnet_discovery_port` overrides it for every peer), 8 at a time, 1.5 s timeout each, body ≤ 16 KiB. Answer: the stations found, with peer host name, DNS name, OS, `name`, `station_id`, `version`, `key_fingerprint`, `paired` (with this deck), `pairing_open`. Peers that do not answer are not listed. No background scan.
- Board, Stations → "Add station": a first tab "On your tailnet" with "Scan" and the list. A row has "Pair" when `pairing_open` and not yet paired with this deck; else the reason ("Pairing is closed on this station: run `fleetdeck station pair-open` there", "Already paired").

## 5. One-click pair

### 5.1 The station's confirm: a pairing window

- "Nothing pairs by itself": the station accepts a pair request only while its pairing window is open.
- The window opens with `fleetdeck station pair-open [--minutes N]` on the station machine (default 10, maximum 60; local socket, owner only). It prints `Pairing is open for 10 minutes. Key fingerprint: <first 16 hex>`. It closes after the time, after one successful pairing, or with `fleetdeck station pair-close`.
- Station setting `tailnet_pairing` (`window` default; `same_user` = always open for same-user decks; `danger: true`, needs elevation or the local socket). With `same_user`, the window rule is skipped, never the caller check.
- A station that is already paired with a deck refuses (`409 already_paired`) until `fleetdeck station forget-deck`.

### 5.2 The exchange

1. The owner presses "Pair" on a row. The board shows the station `key_fingerprint` (first 16 hex) and the station name, and asks to confirm that they match what `pair-open` printed (in `same_user` mode the dialog says that no window is used).
2. The deck makes a single-use invitation for this station the same way `fleetdeck deck invite` does (secret, deck key pin, deck link address = the deck's tailnet `link_listen` address; if the deck has no tailnet `link_listen`, the board says "Use tailnet first" and stops).
3. The deck sends `POST /fleetdeck/v1/pair { invitation, deck_name, deck_key_fingerprint }` to the station.
4. The station checks: caller check (§4.1), window open (or `same_user`), not paired, the invitation parses and its deck address is a tailnet address of the caller node (from the cached whois answer: the caller's own tailnet IPs). It then runs the same join as `fleetdeck station join` with that invitation, in the station process. The join pins the deck key from the invitation.
5. The station answers `{ joined: true, station_id, key_fingerprint }` after the link is up, or an error code (`window_closed`, `already_paired`, `invalid_invitation`, `join_failed`). The window closes on success.
6. The deck checks that the joined station's link key matches `key_fingerprint` from `hello`; else it removes the station at once and shows "The station key changed during pairing".
- Audit: the deck writes `stations.pair_tailnet` (station id, peer DNS name, outcome). The station logs one info line (deck name, caller DNS name, outcome).

## 6. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| No Tailscale on the deck | The card and the tab say "Tailscale not available: <reason>"; manual pairing stays. |
| A peer does not answer `hello` | Not listed. |
| whois fails on the station | `403` (fail closed). |
| The window closed between scan and pair | `window_closed`; the row shows the reason. |
| The deck dies after step 3 | The invitation is single-use and expires as today; the station is joined or not, and the next scan shows `paired`. |

## 7. Tests

- Core: a parser test with recorded `tailscale status --json` and `whois --json` output (real field names), with tagged peers, other users, offline peers, IPv6 only, and a malformed document.
- A fake `tailscale` script (a test fixture) for station and deck tests.
- Station: the endpoint binds only tailnet IPs; a caller of another user, a tagged caller and a whois failure get `403`; `hello` has no other data; `pair` outside the window `window_closed`; inside it joins once; a second pair `already_paired`; the window closes after success; `same_user` skips the window but not the caller check.
- Deck: `/api/tailnet` and `/api/tailnet/scan` are owner-only, CSRF-checked and audited; the scan filters tagged and other-user peers; the fingerprint mismatch removes the station.
- Board: the Tailnet card ("Use tailnet" asks for elevation and lists the changes), the "On your tailnet" tab and its row states.

## 8. Risks

- **Another device of the same Tailscale user can pair a deck while the window is open.** Accepted: same user, owner action on the station, short window, fingerprint confirm on the deck.
- **`same_user` mode** lets any untagged node of the owner's Tailscale user pair at any time. It is a danger setting and off by default.
- **The station trusts tailscaled's whois.** A same-UID attacker on the station could fake the CLI; such an attacker already controls the station (ADR-006 §3).
