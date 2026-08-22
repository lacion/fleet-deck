# P6 preflight — Bun 1.3.14 `ServerWebSocket.send` / `server.publish`

**Purpose.** Characterize Bun 1.3.14 WebSocket `send()` / `ping()` / `publish()`
return values, the 16 MiB silent-drop cliff, drain/recovery, and close-code
delivery so P6.5 preserves the *audited* backpressure contract (per-socket
`getBufferedAmount()` + eviction) rather than the documented tri-state.
Companion source audit: [p6-http-matrix.md](./p6-http-matrix.md) §3 (`send()`
returns are consulted nowhere in `http.ts`).

This is a runtime probe of Bun, not a Fleet Deck behavior change. The §7
callsite match table is the only product-source audit; line anchors are valid
at HEAD `67758ba9` (`http.ts` is byte-identical to the P5 implementation
checkpoint `ca62b94f`).

**Section inventory:** §0 runtime identity and tri-state collisions · §1
`ws.send` return values · §2 16 MiB backpressure cliff · §3
`getBufferedAmount()` · §4 drain / recovery · §5 close-code matrix · §6
`server.publish` / `ws.publish` · §7 `http.ts` send-site match table · §8
P6.5 implications · Reproduction (scratch).

---

## 0. Runtime, method, and documented tri-state

| Field | Value |
| --- | --- |
| Date | 2026-08-22 |
| Host | Linux x64 WSL2 |
| Bun | `1.3.14` |
| Bun revision | `0d9b296af33f2b851fcbf4df3e9ec89751734ba4` (`1.3.14+0d9b296af`) |
| Binary | `/home/luismorales/.bun/bin/bun` |
| `bun-types` (documented tri-state) | `1.3.14` |
| `tcp_rmem` | `4096 131072 6291456` |
| `tcp_wmem` | `4096 16384 4194304` |
| Official runs | 5 scripts × 3 runs under `timeout 60 … < /dev/null`; `./run.sh` → fail=0, **15/15 exit 0** |
| Client | never-read TCP (`never-read-client.py`: HTTP upgrade, then no `recv` until `READ`) |
| Probe tree | `/tmp/fd-ws-probe/` — **scratch, not repo-tracked** |
| Product tree | `fd/v1-effect-feasibility` HEAD `67758ba9`; repo untouched by the probe |

`send()` never threw. The documented tri-state (`>0` bytes sent, `-1`
backpressure, `0` dropped) is empirically true **with collisions**. A positive
return is **payload byte length**, not frame size.

Tri-state collisions that matter:

| return | actual meaning |
| --- | --- |
| `>0` | accepted (usually into kernel; `getBufferedAmount()` stays 0) |
| `-1` | **queued**, not rejected — `getBufferedAmount()` grows. Sticky while buffered > 0 |
| `0` | (a) empty payload / empty `ping()` success on OPEN socket; (b) send after close (`readyState===3`); (c) drop past ~`backpressureLimit` with socket still OPEN; (d) `publish` with 0 subscribers |

`0` is a drop **only** when payload length > 0 or `readyState !== 1`.
Distinguisher on a live drained socket: `ping()` → `0`, `ping("hi")` → `2`
(3/3).

---

## 1. `ws.send` return values (3 official runs)

Healthy client, **wait-until-`getBufferedAmount()===0` between sizes**:

| payload | run1 | run2 | run3 | after send | drain |
| --- | --- | --- | --- | --- | --- |
| text 0 B | 0 | 0 | 0 | 0 | — empty success, not drop |
| text 1…65535 B | =N | =N | =N | 0 | none |
| text 65536 B | **-1** | **65536** | **-1** | 765 / 0 / 765 | fires when -1; drained in 5 ms |
| text 256 KiB | **262144** | **-1** | **-1** | 0 / 260023 / 94951 | ND around this size |
| text 1 / 4 / 8 MiB | -1 | -1 | -1 | grows (0.46–7.5 MiB) | yes; drained 5–82 ms |
| binary 0 / 5 / 1K / 64K | =N | =N | =N | 0 | — |
| binary 1 MiB | **-1** | **1048576** | **-1** | 42809 / 0 / 462167 | ND |
| `sendText("hello-text")` | 10 | 10 | 10 | | |
| `sendBinary(4 B)` | 4 | 4 | 4 | | |
| `ping()` | 0 | 0 | 0 | 0 | live socket |
| `ping("hi")` | 2 | 2 | 2 | 0 | |
| corked 3× `"cork-N"` | 6,6,6 | 6,6,6 | 6,6,6 | 0 | cork callback **arg is `undefined`** (types lie) |
| from `open()` / from timer | 18 / 10 | 18 / 10 | 18 / 10 | 0 | same tri-state |

Paused never-read (identical 3/3):

| payload | ret | after |
| --- | --- | --- |
| `"hello"` 5 B | 5 | 0 |
| 64 KiB | 65536 | 0 |
| 1 MiB | -1 | 973676 |
| 8 MiB | -1 | 9362294 (`readyState` still 1) |

After / during close:

| scenario | ret | readyState | notes |
| --- | --- | --- | --- |
| after `close(1000,"probe-bye")` | 0, 0, 0 | 1→**3 synchronously** | client code 1000, reason `probe-bye`, `wasClean=false` |
| during **server** close(1009, …) | 0 immediately | already 3 | client gets 1009 + exact reason |
| during **client** close (20 ms flood) | positive for ~5k frames, then -1 | server stays **1** | after 40 ms wait: rs=3, ret=0. Server close handler: 1000, reason `''` |

ND called out in **bold**. The size boundary is “did this frame fit in the
kernel send buffer right now”, not a fixed payload size.

---

## 2. Backpressure threshold (16 MiB cliff)

Default `backpressureLimit` = 16 MiB (`16<<20 = 16777216`).
`closeOnBackpressureLimit` default **false** — the socket stays OPEN while
dropping.

**Never-read, 64 KiB frames, default cap**

| run | first -1 | buffered after that | first 0 | buffered at first 0 | then |
| --- | --- | --- | --- | --- | --- |
| 1 | i=2 | 54734 | i=259 | 16,834,510 | sticky 0 (`before==after`, rs=1) |
| 2 | i=2 | 54734 | i=262 | 16,779,196 | sticky 0 |
| 3 | i=2 | 54734 | i=259 | 16,834,510 | sticky 0 |

Kernel absorbs ~2×64 KiB with `send()>0` and `buffered=0`. Then `-1` and uWS
userspace grows. Drop (`0`) at **~16.78–16.83 MiB** (one in-flight frame
overshoots 16 MiB). Drain does **not** fire while the client is not reading.

**Never-read, 1 KiB frames:** first -1 at i=136/137; first 0 at i=16529/16697;
peak 16,777,552–16,777,944. Same 16 MiB cliff, more frames.

**SO_RCVBUF=4096, 64 KiB frames:** first -1 at **i=1**; first 0 at i=258; peak
16,829,780. **3/3 identical.**

**`backpressureLimit: 256 KiB`, 16 KiB frames:** first -1 at i=8; first 0 at
i=25; peak **267,796**. **3/3 identical.** Sticky 0.

**Healthy Bun client, 200×64 KiB tight loop (client is reading, just slower):**
hist `{65536: 2, -1: 198}`, peak ~12.6–12.8 MiB, **never 0**. So `-1` is easy
on a live peer; `0` is the 16 MiB cliff.

**Single huge send into never-read (3/3):**

| size | ret | queued? |
| --- | --- | --- |
| 256K / 1M / 4M / 8M | -1 | yes |
| 16 MiB | -1 | **yes — overshoots** to ~30.3–30.5 MiB |
| 32 MiB | **0** | no (`before==after` ~30.3 MiB) |

A single frame **larger than remaining room still queues if it is ≤
remaining+something**; 32 MiB is refused. The 16 MiB “limit” is not a hard
per-send cap.

---

## 3. `getBufferedAmount()` / `bufferedAmount`

- ServerWebSocket: **method** `getBufferedAmount()` on the prototype. **No**
  `bufferedAmount` property (`hasBufferedAmountProp=false`). `ownNames=[]`.
- Never negative. `FLEETDECK_WS_BUFFER_MAX=-1` eviction trick is valid.
- Counts **uWS userspace** unsent bytes, **not** kernel TCP. The first
  ~128–140 KiB of stall is invisible (`send()>0`, buffered=0).
- Under `-1`: grows. Under cap-`0`: stays put. After close: 0.

---

## 4. Drain / recovery

Fill to the 16 MiB cliff, then `READ` on the never-read client.

| run | drain events after READ | ms to first drain | send() right after drain | recovered to `>0` | then 64 KiB send |
| --- | --- | --- | --- | --- | --- |
| 1 | 2 | 3.39 | -1 (buffered still ~14.6 MiB) | i=13, t=81 ms, buffered=0 | 65536 |
| 2 | 1 | 3.77 | -1 (~14.9 MiB) | i=16, t=90 ms, buffered=0 | 65536 |
| 3 | 1 | 3.92 | -1 (~14.8 MiB) | i=12, t=86 ms, buffered=0 | 65536 |

Facts:

- **`drain` does fire on recovery.** It does **not** fire during the flood.
- **`drain` fires before the backlog is empty.** Immediate `send()` is still
  `-1`.
- Recovery to positive is when **`getBufferedAmount()===0`**, ~80–90 ms after
  READ at the 16 MiB cliff.
- Post-flood sends **before** READ are sticky `0` (already past the drop
  cliff). After drain-to-empty they become positive again — the 0-drop is not
  a permanent socket death; it is “over cap right now”.
- At 256 KiB limit, backlog is gone by the first poll (~5 ms) and the first
  send already returns 16.

---

## 5. Close codes / `readyState` (deterministic 3/3)

ServerWebSocket **never occupies CLOSING (2)**. `close()` / `terminate()` jump
**1→3 synchronously**. Client still 1 for ~80–130 ms, then the client `close`
event fires.

| server action | client code | client reason | client wasClean | server `close` handler | send() after call |
| --- | --- | --- | --- | --- | --- |
| `close()` | 1000 | `''` | **false** | 1000, `''` | 0 |
| `close(1000)` | 1000 | `''` | false | 1000, `''` | 0 |
| `close(1000, "bye")` | 1000 | `bye` | false | 1000, `bye` | 0 |
| `close(1009, "terminal viewer too far behind")` | 1009 | exact | false | 1009, exact | 0 |
| `close(1009, "input frame too large")` | 1009 | exact | false | 1009, exact | 0 |
| `terminate()` | 1006 | `Connection ended` | false | 1006, `''` | 0 |

Client-initiated (also 3/3):

| client action | immediately: server rs / send | after ~50 ms | server handler | client wasClean |
| --- | --- | --- | --- | --- |
| `close()` | 1 / **8 (still queues)** | 3 / 0 | 1000, `''` | **true** |
| `close(1000, "client-bye")` | 1 / 8 | 3 / 0 | 1000, `''` — **reason does not arrive** | true |
| `close(1001, "going away")` | 1 / 8 | 3 / 0 | 1001, `''` | true |

`wasClean` is **false for every server-initiated close**, including 1000. Do
not use it as a graceful-close detector with Bun’s client.

---

## 6. `server.publish` / `ws.publish`

| case | return (3/3 unless noted) |
| --- | --- |
| 0 subscribers / missing topic | 0 |
| 1 healthy, 5 B / 64 KiB / 1 MiB | 5 / 65536 / 1048576 |
| 2 healthy, `server.publish` 9 B | **9, not 18** (payload size, not ×N) |
| `ws.publish` to a topic with only self, `publishToSelf` default false | 0 |
| `ws.publish` with `publishToSelf: true` | 7, and the client receives it |
| 4000 × 64 KiB `server.publish` to a never-read subscriber | **all 4000 return 65536**; never -1 or 0 |
| same socket `getBufferedAmount` at end | 16,780,644 / 16,780,644 / 16,788,946 |
| `ws.send` after that cliff | 0, buffered unchanged, rs=1 |
| mixed healthy+paused, 4000 publishes | publish still all-65536; paused buf=16,779,196; healthy buf 8.3 / 7.0 / 5.2 MiB; **healthy client received 97 / 125 / 123 frames (ND)** |

**`server.publish` is not a per-subscriber backpressure signal.** It returns
payload bytes whenever `subscriberCount >= 1`, even while that subscriber is
at the 16 MiB cliff and further topic messages are silently dropped for that
socket. P6.5 “preserve native publish -1/0/bytes as backpressure” **does not
match this runtime**.

---

## 7. `http.ts` send sites vs observed semantics

Current policy: **no `drain`, no `publish`, no `backpressureLimit` override**
(Bun defaults: 16 MiB, `closeOnBackpressureLimit=false`). Cap via
`getBufferedAmount()`, then evict.

| site | what it does | match? |
| --- | --- | --- |
| `http.ts:107–110` `MAX_WS_BUFFER` default 1 MiB; `FLEETDECK_WS_BUFFER_MAX=-1` | comment “bufferedAmount is never negative” | **MATCH** — method is `getBufferedAmount()`, never negative, no `bufferedAmount` property |
| `http.ts:124–125` term caps 1 MiB input / 4 MiB buffer | evict well before 16 MiB silent drop | **MATCH** — 1 MiB / 4 MiB fire inside the `-1`-and-still-queued region, before `0`-drop |
| `http.ts:2217` `if (c.readyState !== 1) continue` | skip closed | **PARTIAL** — works after **server** close (rs=3 sync). After **client** close the server stays 1 and `send()` still queues until the `close` handler |
| `http.ts:2227–2229` `getBufferedAmount() > MAX_WS_BUFFER` → `terminate()` | evict snapshot peer | **MATCH** and the right policy. Skip-and-forget would strand the board (comment is correct). `terminate()` → client 1006 `"Connection ended"` |
| `http.ts:2235` `c.send(msg)` return ignored | | **OK / do not “fix”**. Treating `-1` as failure would murder healthy clients: 256 KiB–1 MiB snapshots often return `-1` then drain in 5–50 ms. `0` cannot happen under the 1 MiB cap unless the socket is already closed (empty payload isn’t used) |
| `http.ts:2262` quiescing `open` → `terminate()` | | **MATCH** — abrupt 1006 |
| `http.ts:2272` `open()` snapshot `ws.send(...)` in try/catch, **no buffer check** | | **OK for current snapshots** (typically ≪ 1 MiB; small sends return positive). A pathological snapshot could `-1` and queue; next `broadcast()` would then evict |
| `http.ts:2294` `ws.close(1009, 'input frame too large')` | | **MATCH** — client gets 1009 + exact reason |
| `http.ts:2349` `sendTermFrame` skip if `readyState !== 1` | | **PARTIAL** — same client-close lag as 2217 |
| `http.ts:2350–2352` `getBufferedAmount() > 4 MiB` → `close(1009, 'terminal viewer too far behind')` | | **MATCH** — 1009 + exact reason delivered. Preferable to `terminate()` here: the viewer can show why it died |
| `http.ts:2358` `ws.send(JSON.stringify(frame))` return ignored | | **OK** — same as 2235. 4 MiB cap ≪ 16 MiB drop cliff |
| `http.ts:2369` quiescing term → `terminate()` | | **MATCH** |
| `http.ts:2397` / `http.ts:2431` `ws.close()` no args on term exit/error | | **MATCH** — client sees 1000, empty reason, `wasClean=false` |
| `http.ts:2453` keepalive missed pong → `terminate()` | | **MATCH** — 1006 |
| `http.ts:2458` `ws.ping()` return ignored | | **MUST ignore**. Empty `ping()` returns **0 on a live socket**. Treating 0 as failure would reap healthy peers |
| `http.ts:2257–2341` handler has `open/message/close/pong` only — **no `drain`** | | **OK for evict-or-send**. Resume-on-drain would need to wait until `getBufferedAmount()===0`, not merely the `drain` event |
| `http.ts:2721–2743` `Bun.serve({ idleTimeout: 0, websocket })` — no WS `backpressureLimit` | 16 MiB default, drop-not-close | **MATCH observed**. Current 1/4 MiB caps make the 16 MiB silent-drop unreachable on the send path |
| `http.ts:2831` `finalizeNativeClients` `terminate()` | | **MATCH** 1006. Orthogonal: `http.ts:2983–3004` correctly avoids close/terminate **before** `stop(true)` on 1.3.14 |

---

## 8. P6.5 implications

Do **not** wire a pub/sub backpressure policy to `server.publish`’s return. It
will not go `-1`/`0` per slow subscriber. Per-socket `getBufferedAmount()`
(current `http.ts` shape) is the signal that actually moves. If P6 uses
topics, keep a subscriber set and `send()` (or check `getBufferedAmount()`
then `publish`, knowing publish can still silently drop at 16 MiB).

**Do not** treat `send()===-1` as “drop it”. **Do not** treat `send()===0` as
“closed” without also checking payload length and `readyState`. **Do not**
treat `ping()===0` as failure.

The contract to preserve is therefore: per-socket `getBufferedAmount()`
thresholds with eviction (snapshot `terminate()` past `MAX_WS_BUFFER`;
terminal `close(1009)` past `MAX_TERM_WS_BUFFER`); `send()`/`ping()` returns
deliberately ignored; no drain-based resume; no reliance on `server.publish`
return values for per-subscriber backpressure.

---

## Reproduction

All under `/tmp/fd-ws-probe/` (**scratch, not repo-tracked**):

```
01-inspect-and-send-returns.mjs
02-backpressure-threshold.mjs
03-drain-recovery.mjs
04-close-codes.mjs
05-publish.mjs
lib.mjs
never-read-client.py
run.sh
RESULTS.md          # same tables, archival
out/01-run{1,2,3}.json … out/05-run{1,2,3}.json
out/bun-version.txt
```

`out/*-run0.json` are earlier smokes; counts above use runs 1–3 only.

```sh
export PATH="$HOME/.bun/bin:$PATH"
cd /tmp/fd-ws-probe
./run.sh
```
