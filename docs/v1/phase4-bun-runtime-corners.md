# Phase 4 — bun runtime corners: live daemon validation

> Status: **validated, all three corners pass.** The full daemon was booted under
> **bun 1.3.14** (bun:sqlite channel) in a scoped test env and the three F2 "danger corners"
> the plan flagged — mDNS multicast, ws upgrade, tmux control-mode — were each exercised
> against the live daemon. The Node `node --test` lane stays the authoritative gate; nothing
> here changes source behaviour on the Node path.

Method: a self-contained boot with a scoped HOME (`mktemp -d`), a scoped port (never 4711), a
**scoped tmux socket** `fleetdeck-test-<port>` (never the shared default socket where the board's
tmux lives), and `FLEETDECK_BIND=0.0.0.0` to turn on LAN mode (`LAN_MODE = !isLoopbackAddress(BIND)`)
so the mDNS responder actually starts. Teardown (trap EXIT) SIGTERMs only the test daemon and
`kill-server`s only the scoped socket; the board (`fleetdeck.service`) is never touched.

## Headline

| Corner | Wiring | Result under bun |
|---|---|---|
| **bun:sqlite in the real daemon** | `db.mjs` → `sqlite.mjs` `openDatabase()` | `fleetd up … db …/fleetd.db`, `/health` 200 — **works** |
| **runtime identity** | `/proc/<pid>/exe` = `bun`, cmdline `bun scripts/fleetd/fleetd.mjs` | the exact production shape the Phase-4 identity fix now accepts — **validated live** |
| **mDNS** (`mdns.mjs`) | `createSocket({udp4,reuseAddr})` + object-`bind` + `setMulticastTTL(255)` + `addMembership`/`setMulticastInterface` + `send` | `mdns responding for <name>.local:<port>` — responder came up **ALIVE** |
| **ws** (`http.mjs:1290-1318`) | `WebSocketServer({noServer:true})` + `server.on('upgrade')` → `handleUpgrade` | `handleUpgrade accepted` + a `type=snapshot` frame (966 B) received — **works** |
| **tmux control-mode** (`termbridge.mjs:288`) | `tmux -L <scoped> -C attach-session` piped through the real `ControlModeParser` | `%begin/%end` → `response` event + `session-changed` parsed — **works** |
| daemon health after ws+tmux | — | `/health` 200, process ALIVE |

## mDNS — the plan's highest risk — is RETIRED

The risk ledger called mDNS *"the highest risk; may cap standalone bun or stay Node there"* and F2
warned it *"must not hostage on bun's dgram support."* Empirically, **bun's dgram does all of it**:
the responder logged `mdns responding for fdbunsmoke.local:47141 (192.168.8.223, 100.89.30.8)`, which
is only reached after `createSocket`, object-form `bind({port})`, `setMulticastTTL(255)` (the one the
code treats as decisive — `catch → die()`), per-interface `addMembership` + `setMulticastInterface`,
and the announcement `send` **all** succeed. Note `die()` only stands the responder down (closes the
socket, fires `onDown`); it never exits the process, so even a hypothetical dgram gap would degrade
discovery gracefully rather than crash the daemon. **mDNS does not cap the bun standalone path.**

## ws — noServer + handleUpgrade works

A `WebSocket` client (bun's global) connected to `ws://127.0.0.1:<port>/ws?t=<token>`: the upgrade
passed the `authorized` + `hostHeaderOk` + `crossSiteReason` gate, `wss.handleUpgrade` accepted it,
and the coalesced `snapshot` frame arrived intact. The `bufferutil`/`utf-8-validate` externals (left
external in the bundle) behave under bun — a 966-byte JSON frame round-tripped with no native-addon
error.

## tmux control-mode — child pipe + byte protocol + parser works

`tmux -L <scoped> -C attach-session -t =<sess>` was spawned under bun with the **exact** argv shape
from `termbridge.mjs:288-292` (`stdio:['pipe','pipe','pipe']`), and its stdout was fed to the
**production** `ControlModeParser`. The parser emitted a `response` event (from the `%begin/%end`
handshake block) followed by `session-changed` — i.e. bun's `child_process` pipe, the LATIN-1 byte
decode, and the incremental control-mode line parser all work together under bun. This is the same
capability the `tmux-adapter` suite exercises (15/7 under bun in Phase 3); the residual adapter
failures there were `kill-server`/`display-message` command timing on a shared scoped socket
(category G / environmental), **not** a control-mode I/O gap — this live check confirms the core
runtime capability is sound.

## Verdict

- **All three Phase 4 danger corners work under a live bun-hosted daemon**, and the daemon stays
  healthy (200 / ALIVE) after exercising them.
- The **identity fix** (`takeover.mjs` regex → `/^(?:node|nodejs|bun|fleetd)$/i`, bundle regenerated)
  is validated in its real production shape: a live daemon whose `/proc/<pid>/exe` is `bun` is now
  correctly recognised as a fleetd, and election + accept-reset flip fully green under `bun test`
  (takeover 13/2, residuals = fetch/socket, not identity) with **no Node regression** (source lane
  15/8/4, bundle lane 15/8/4).
- **mDNS is no longer a blocker** for the standalone bun distribution (Phase 5), which remains out of
  scope for this foundation pass but is now known to be unobstructed on its highest-risk corner.

## Raw method (local, uncommitted)
- `/tmp/fd-ws-smoke.mjs` — bun `WebSocket` client against `/ws`.
- `/tmp/fd-tmux-smoke.mjs` — `tmux -C` spawn under bun through the real `ControlModeParser`.
- Boot: scoped HOME/port/tmux-socket, `FLEETDECK_BIND=0.0.0.0`, trap-EXIT teardown (board preserved).
