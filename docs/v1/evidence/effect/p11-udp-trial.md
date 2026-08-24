> **Stamped 2026-08-24** — copied verbatim into the evidence tree from the reconstructed trial report (`/tmp` working drafts lost to a scratch wipe; see the reconstruction note below).

# P11.1 + P11.2 + P11.3 — mDNS / `Bun.udpSocket` trial: **KEEP `node:dgram`**

*Reconstructed from the trial worker's final report (session 2026-08-24; the full draft + raw probe JSON were lost to a /tmp scratch wipe — every verdict, probe result, and citation below is preserved verbatim from the report).*

**Verdicts**

| Item | Verdict | Basis |
|---|---|---|
| **P11.1** isolate behind Datagram interface | **NO SLICE NEEDED (under KEEP)** | Responder is already a narrow port; a runtime-neutral interface would only host a rejected adapter |
| **P11.2** two responders coexist on 5353 | **KEEP `node:dgram`** | `Bun.udpSocket` has no reuse option → `EADDRINUSE` on a second bind |
| **P11.3** goodbye-datagram completion | **KEEP `node:dgram`** | `send()===true`/`drain` prove writability, not delivery; no per-datagram callback |

**Key evidence**

- **P11.1 seam state** — `node:dgram` is confined to `mdns.ts` alone. The daemon consumes only the `createMdns → { start, stop, update, alive }` port (`program.ts:978`), and a `node:dgram`-shaped `inject` seam already exists (`test-seam.ts`). The pure wire codec is fully exported/off-network. There is no existing Datagram/Discovery Effect interface (the `addDiscovery`/"Discovery" hits are unrelated). Isolation is partial but sufficient for a KEEP — a Bun-neutral Datagram interface would exist only to host the adapter P11.2 rejects, so no slice is warranted.
- **P11.2 decisive probe** (Linux/WSL2, bun 1.3.14): `Bun.udpSocket` — two sockets, one port → **`EADDRINUSE`** (coexistence fails). `node:dgram` `reuseAddr:true` → coexist; `reuseAddr:false` → `EADDRINUSE`. Reuse *is* the delta, and Bun cannot reach it. Type-level confirmation: `udp.SocketOptions = { hostname?, port?, binaryType?, socket? }` (`bun-types/bun.d.ts:6532`) — no `reuseAddr`/`reusePort`, while TCP-listen and `Bun.serve` both expose `reusePort`. This is exactly the `reuseAddr:true` `mdns.ts:1236` uses to answer alongside avahi. (Host had no avahi, so the high-port two-socket test is the clean semantic; macOS is strictly worse — Bonjour permanently holds 5353.)
- **P11.3 goodbye completion** — `send(data,port,addr): boolean` (`bun.d.ts:6631`), no callback param; `drain` = writability. Probe: `send()` returned `true` but the receiver saw the datagram only after an explicit 50 ms wait, i.e. after `send()` already returned. `mdns.ts` `stop()`/`withdrawAndDie()`/`update()` all serialize `close()` against `node:dgram`'s send-callback (the OS-handoff barrier); Bun offers no equivalent → a port would drop the goodbye or invent a timing guess. **Unprovable.**
- **Characterization floor** — `tests/mdns.test.ts` + `tests/lan-mdns-state.test.ts` = 45 pass / 0 fail read-only; already pins goodbye TTL-0, per-interface egress, roaming, conflict stand-down, `onDown`/`alive`, idempotency. Notably the suite's own observer binds 5353 with `reuseAddr:true` (`:820`, `:1227`) — it depends on the coexistence Bun lacks. A KEEP needs no new pin.

**§4-row disposition** — mDNS row `ISOLATE/TRIAL` → **`KEEP node:dgram`** (P11.1 already isolated behind `createMdns` + `inject`; P11.2 no reuseAddr coexistence; P11.3 completion unprovable), owner mDNS/discovery.

**MIGRATE-later trigger:** Bun ships a documented UDP reuse option (reuseAddr/reusePort on udpSocket) AND per-datagram send-completion; re-run the two-socket coexistence probe and the goodbye-capture probe before any port.
