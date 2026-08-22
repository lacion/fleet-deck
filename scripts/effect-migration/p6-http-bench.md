# P6.8 HTTP/WS load harness

Duration-based, pure-Bun harness that starts a real daemon and drives the P6.8
workload set against the frozen P6.1 wire shapes. New files only — invoke it
directly; there is no `package.json` script.

```sh
bun scripts/effect-migration/p6-http-bench.ts --help
```

It prints a human table on stdout. Pass `--out=path.json` for the machine
report. Warmup is discarded. Per-op latency uses `process.hrtime.bigint()`.

`--label=smoke` (the default, also forced by `--smoke`) is harness validation.
`--label=baseline` is the quiet-host evidence label and implies `--require-floor`.
Never check a `label=smoke` JSON in as P6.8 evidence. Never treat a busy-host
run as a baseline.

## Smoke (busy host, not a baseline)

Short runs, concurrency ≤ 8, duration ≤ 5 s. Proves every workload produces
sane numbers on **both** daemon targets.

```sh
bun scripts/effect-migration/p6-http-bench.ts \
  --smoke --target=both \
  --out=/tmp/p6-http-bench-smoke.json
```

Equivalent explicit knobs: `--label=smoke --duration=2 --warmup=1 --concurrency=1,8`.

## Quiet-host baseline

Idle machine, pinned Bun **1.3.14** revision
`0d9b296af33f2b851fcbf4df3e9ec89751734ba4`, twice, same host. Do not run this
on a contended box.

```sh
bun scripts/effect-migration/p6-http-bench.ts \
  --target=both \
  --label=baseline \
  --require-floor \
  --workload=all \
  --concurrency=1,8,32 \
  --duration=15 \
  --warmup=2 \
  --paste-bytes=2097152 \
  --probe-concurrency=8 \
  --out=docs/v1/evidence/effect/p6-http-bench-run-1.json
```

Repeat as `p6-http-bench-run-2.json`. Compare only reports whose
`comparison.key` matches (runtime + machine + knobs). Source and bundle both
land in one report when `--target=both`.

## Why 1 / 8 / 32

Plan §8 characterized HTTP at 1/10/100 with an **iteration** budget (P0
`p0-workloads.ts`). P6.8 is a **duration** regression harness at representative
product concurrency:

| N | Meaning |
|---|---|
| 1 | Serial latency floor |
| 8 | Typical board + a few hook shims + a viewer |
| 32 | Burst (many sessions notifying, or a paste storm) |

100-way in-flight on a busy host is synthetic noise, not a product shape. The
exit budget still reads p95/p99 + correctness; throughput is reported alongside,
never alone.

## How results feed the P6.8 exit budget

Plan §8 provisional budgets (do not silently loosen):

- `/health`, `/state`, and process p95: no more than **10%** regression vs the
  quiet-host baseline on the same machine and `comparison.key`.
- Throughput is recorded only alongside p95/p99 and correctness.
- Soft-budget overruns need a checked-in explanation and maintainer acceptance.
  Correctness, fail-open, and leak failures are hard.

This harness covers the HTTP/WS half of that budget (`/health`, `/state`, hook
POST, large paste, withheld-other-connection probes, WS snapshot fanout, static
shell + hashed asset). Process p95 stays with the P0 exec bench. Re-run the
quiet-host commands after each P6 sub-slice and diff `rows[].latencyMs.p95`
for `health` and `state` at each concurrency, source and bundle.

The harness **records** numbers. It does not accept a delta.

## Matrix-forced design choices

These are load-shape decisions forced by
[`docs/v1/evidence/effect/p6-http-matrix.md`](../../docs/v1/evidence/effect/p6-http-matrix.md),
not knobs:

1. **Paste** is a few MB decoded (default 2 MiB, same as P0), under the 10 MiB
   image cap and 14e6 transport cap — not near 14e6. Byte-identity is checked
   once; the timed loop only asserts `201` / `ok` / `.png` because
   `MAX_KEPT_PASTES=50` prunes concurrent files.
2. **Valid hook** is `POST /hook/Notification` with `session_id`. The known
   handler still returns canonical `200 {}`.
3. **Fail-open** is authenticated `POST /hook/Stop` **missing** `session_id`
   (`validateHookEvent` → `200 {}`, no dispatch). Tokenless `silentHookRefusal`
   is a different `200 {}` site and is not this workload.
4. **Withheld** is a raw `POST /hook/Stop` with `Content-Length` and a partial
   JSON body (bearer present so the route actually waits on `'end'`). After
   `BODY_DRAIN_GRACE_MS` (1000) the measured signal is **GET `/health` on other
   connections**, not the withheld socket itself (P0 already pinned
   `responseBytes===0` on that socket). Stall FIN stays at 120 s; the harness
   aborts the sockets itself.
5. **WS** clients subscribe to `/ws` snapshots (`core.snapshot()`, no LAN
   tokens — matrix §9.2). Mutations are **serial** because
   `BROADCAST_COALESCE_MS=60` would merge overlapped POSTs. Latency is POST
   start → last client's next snapshot.
6. **Hashed `/assets/*`** is discovered from the served `GET /` HTML so the
   harness does not hard-code Vite fingerprints. Shell is `no-store` HTML;
   the JS module is `immutable`.
7. **GET `/state`** is `snapshotWithLan()` (token-bearing LAN URLs) — heavier
   than the WS snapshot. That is the route the board polls.

## Knobs

| Flag | Default | Notes |
|---|---|---|
| `--target` | `both` | `source` (`src/daemon/fleetd.ts`) or `bundle` (`src/daemon/fleetd.bundle.mjs`) |
| `--workload` | `all` | `health,state,hook,hook-fail-open,paste,withheld,ws,static-shell,static-asset` (`static` = both assets) |
| `--concurrency` | `1,8,32` (`1,8` under `--smoke`) | HTTP in-flight / withheld socket count / WS client count |
| `--duration` | `15` s (`2` under `--smoke`) | measured window; warmup excluded |
| `--warmup` | `2` s (`1` under `--smoke`) | discarded; `0` allowed |
| `--label` | `smoke` | `baseline` implies `--require-floor` |
| `--paste-bytes` | `2097152` | decoded image size |
| `--probe-concurrency` | `8` | `/health` probes while N bodies are withheld |
| `--require-floor` | off unless baseline | Bun 1.3.14 / `0d9b296af` |
