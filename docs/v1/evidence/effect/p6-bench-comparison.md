# P6.8 exit comparison — quiet-host baseline vs post-conversion

Single-run comparison of the P6.8 HTTP/WS load harness
(`scripts/effect-migration/p6-http-bench.ts`, knobs in
[`p6-http-bench.md`](../../../scripts/effect-migration/p6-http-bench.md))
against the frozen P6.1 wire shapes. Numbers below are parsed from the two
JSON reports, not retyped from a table dump.

**Headline:** every `/health` and `/state` p95 cell is **within the +10%
budget** (12/12 PASS). Correctness is clean (0 errors on both sides, 54/54
rows). Converted-route p95 does not show an Effect-shaped regression once
WSL2 single-run jitter is taken into account. No budget cell is a one-sided
outlier that would force a rerun before declaring the P6.8 p95 line met.

---

## 0. Identity

| Field | Pre-conversion baseline | Post-conversion |
| --- | --- | --- |
| JSON | [`p6-baseline.json`](./p6-baseline.json) | [`p6-postconv.json`](./p6-postconv.json) (copied from `/tmp/fd-effect/p6-postconv.json`) |
| Tree | `ac438c21` (`docs(effect): capture the pre-P6.4 performance baseline`; taken at `51d39ddd`) | `b2d11d84` (`feat(effect): convert the hook route group under the fail-open boundary`) |
| `recordedAt` | `2026-08-22T22:32:34.230Z` | `2026-08-23T07:31:57.517Z` |
| Harness `label` | `baseline` | `baseline` (quiet-host evidence label + `--require-floor`; **not** a second pre-conversion floor) |
| `ok` | `true` | `true` |
| `comparison.key` | `d9db9228b668b8e379377930d87f4575ba4b186bd84c3504f43cbc93938d60da` | **identical** |
| Bun | 1.3.14 / `0d9b296af33f2b851fcbf4df3e9ec89751734ba4` (`runtimeFloor.exactMatch: true`) | same |
| Host | linux WSL2 `6.6.87.2-microsoft-standard-WSL2`, x64, AMD Ryzen 9 9950X3D, 32 logical CPUs | same |
| Knobs | targets `source,bundle`; workloads all 9; concurrency `1,8,32`; duration 15 s; warmup 2 s; paste 2 097 152 B; probeConcurrency 8; requestTimeout 15 s; `smoke: false` | **identical** |

`comparison.key` covers runtime + machine + knobs. The two reports are
comparable under the harness rule. They are **not** a paired run-1/run-2 on
the same tree: the harness doc asked for two quiet-host captures of the
pre-conversion floor (`p6-http-bench-run-1.json` / `run-2.json`); only one
pre-conversion capture was checked in, and the post-conversion side is also
one run. Treat every delta as a single-run WSL2 observation.

Between the two trees: health/state, paste, settings/command/mail/cleanup,
control, the join-on-interrupt fix, WS-snapshot pure leaves, the hook
fail-open contract, and the hook route group all landed. Process p95 stays
with the P0 exec bench and is **not** this report.

Sign convention in every table: **latency Δ% > 0 is slower** (worse);
**rps Δ% > 0 is more throughput** (better). Percentages are
`(post − base) / base × 100`. Raw rps / p50 / p95 / p99 / error counts are
the JSON values as stored.

---

## 1. Budget definition (what this report accepts)

From [`p6-http-bench.md`](../../../scripts/effect-migration/p6-http-bench.md)
and plan §8 (do not silently loosen):

- `/health` and `/state` p95: no more than **10%** regression vs the
  quiet-host baseline, per target × concurrency, source and bundle.
- Other workloads are judged on **rps together with p95/p99 and
  correctness (0 errors)**, not raw throughput alone.
- Soft-budget overruns need a checked-in explanation and maintainer
  acceptance. Correctness / fail-open / leak failures are hard. This
  harness records numbers; it does not accept a delta.

The +10% line for a cell is `p95_base × 1.10`. PASS iff `p95_post ≤ line`.

`POST /command` (P7) is converted on this tree
(`settings-command-mail-cleanup.ts`) but **is not a harness workload**.
Nothing in the nine workloads POSTs `/command`. The converted mutating HTTP
path the harness *does* drive is paste (P8). Hook / hook-fail-open drive
the converted `/hook/:name` fail-open settler. WS mutations are serial
`POST /hook/Notification` (converted hook) plus ownership-only snapshot
fanout.

---

## 2. Budget verdict — `/health` and `/state` p95

**12 / 12 PASS. Overall P6.8 p95 budget: within budget.**

| workload | target | N | p95 base (ms) | p95 post (ms) | p95 Δ | +10% line (ms) | post − line (ms) | verdict | rps base | rps post | rps Δ | errors (base/post) |
|---|---|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---|
| health | source | 1 | 11.089 | 10.687 | -3.625% | 12.198 | -1.511 | **PASS** | 98.107 | 98.772 | +0.68% | 0/0 |
| health | source | 8 | 151.291 | 151.285 | -0.004% | 166.420 | -15.135 | **PASS** | 97.571 | 98.843 | +1.30% | 0/0 |
| health | source | 32 | 334.041 | 324.377 | -2.893% | 367.445 | -43.068 | **PASS** | 97.962 | 98.399 | +0.45% | 0/0 |
| health | bundle | 1 | 11.285 | 10.663 | -5.512% | 12.414 | -1.751 | **PASS** | 98.273 | 98.4 | +0.13% | 0/0 |
| health | bundle | 8 | 151.12 | 151.27 | +0.099% | 166.232 | -14.962 | **PASS** | 98.098 | 98.256 | +0.16% | 0/0 |
| health | bundle | 32 | 325.49 | 343.085 | +5.406% | 358.039 | -14.954 | **PASS** | 98.21 | 98.096 | -0.12% | 0/0 |
| state | source | 1 | 11.323 | 10.714 | -5.378% | 12.455 | -1.741 | **PASS** | 98.079 | 98.182 | +0.11% | 0/0 |
| state | source | 8 | 141.864 | 141.652 | -0.149% | 156.050 | -14.398 | **PASS** | 97.883 | 98.615 | +0.75% | 0/0 |
| state | source | 32 | 424.524 | 387.85 | -8.639% | 466.976 | -79.126 | **PASS** | 97.424 | 97.902 | +0.49% | 0/0 |
| state | bundle | 1 | 11.35 | 10.68 | -5.903% | 12.485 | -1.805 | **PASS** | 97.849 | 98.615 | +0.78% | 0/0 |
| state | bundle | 8 | 141.994 | 141.712 | -0.199% | 156.193 | -14.481 | **PASS** | 97.406 | 98.102 | +0.71% | 0/0 |
| state | bundle | 32 | 425.7 | 372.217 | -12.564% | 468.270 | -96.053 | **PASS** | 97.167 | 98.04 | +0.90% | 0/0 |

Closest cell to the line: **health / bundle / N=32**, p95 +5.406% (343.085 vs
line 358.039, 14.954 ms of headroom). That is the only budget cell whose
p95 moved up by more than a tenth of a percent; rps there is flat
(-0.12%). Serial (N=1) p95 improved 3.6–5.9% on both routes and both
targets. N=8 p95 is a 151 ms / 142 ms plateau that did not move (see §5).
N=32 `/state` p95 improved 8.6–12.6% — read as quieter-host scatter on a
wide quantile, not as an Effect speedup.

No budget cell fails. A rerun is **not** warranted before declaring the
+10% line met: the one cell that moved the wrong way still has ~4.6
percentage points of headroom, and its p99 jump (see §5) is a tail sample,
not a p95 outlier.

---

## 3. Per-workload × target × concurrency

54 rows. Errors are 0 on every row of both reports (`errorBuckets` empty;
combined `ok` 939 172 → 996 465; the extra post ops are almost all
static-shell / static-asset).

| workload | target | N | rps base | rps post | rps Δ | p50 base | p50 post | p50 Δ | p95 base | p95 post | p95 Δ | p99 base | p99 post | p99 Δ | err base | err post |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| health | source | 1 | 98.107 | 98.772 | +0.68% | 10.102 | 10.093 | -0.09% | 11.089 | 10.687 | -3.63% | 12.974 | 11.892 | -8.34% | 0 | 0 |
| health | source | 8 | 97.571 | 98.843 | +1.30% | 80.474 | 80.541 | +0.08% | 151.291 | 151.285 | -0.00% | 152.031 | 151.629 | -0.26% | 0 | 0 |
| health | source | 32 | 97.962 | 98.399 | +0.45% | 322.972 | 313.223 | -3.02% | 334.041 | 324.377 | -2.89% | 635.755 | 634.802 | -0.15% | 0 | 0 |
| health | bundle | 1 | 98.273 | 98.4 | +0.13% | 10.097 | 10.09 | -0.07% | 11.285 | 10.663 | -5.51% | 12.626 | 12.334 | -2.31% | 0 | 0 |
| health | bundle | 8 | 98.098 | 98.256 | +0.16% | 80.18 | 80.486 | +0.38% | 151.12 | 151.27 | +0.10% | 152.268 | 151.838 | -0.28% | 0 | 0 |
| health | bundle | 32 | 98.21 | 98.096 | -0.12% | 323.066 | 322.836 | -0.07% | 325.49 | 343.085 | +5.41% | 415.854 | 635.011 | +52.70% | 0 | 0 |
| state | source | 1 | 98.079 | 98.182 | +0.11% | 10.105 | 10.097 | -0.08% | 11.323 | 10.714 | -5.38% | 13.921 | 14.855 | +6.71% | 0 | 0 |
| state | source | 8 | 97.883 | 98.615 | +0.75% | 71.002 | 70.943 | -0.08% | 141.864 | 141.652 | -0.15% | 153.37 | 151.711 | -1.08% | 0 | 0 |
| state | source | 32 | 97.424 | 97.902 | +0.49% | 313.495 | 313.352 | -0.05% | 424.524 | 387.85 | -8.64% | 628.464 | 627.588 | -0.14% | 0 | 0 |
| state | bundle | 1 | 97.849 | 98.615 | +0.78% | 10.094 | 10.095 | +0.01% | 11.35 | 10.68 | -5.90% | 19.538 | 11.905 | -39.07% | 0 | 0 |
| state | bundle | 8 | 97.406 | 98.102 | +0.71% | 71.054 | 70.917 | -0.19% | 141.994 | 141.712 | -0.20% | 153.328 | 151.982 | -0.88% | 0 | 0 |
| state | bundle | 32 | 97.167 | 98.04 | +0.90% | 313.882 | 313.506 | -0.12% | 425.7 | 372.217 | -12.56% | 631.026 | 627.732 | -0.52% | 0 | 0 |
| hook | source | 1 | 92.251 | 95.276 | +3.28% | 10.767 | 10.369 | -3.70% | 13.373 | 12.444 | -6.95% | 19.964 | 19.132 | -4.17% | 0 | 0 |
| hook | source | 8 | 95.354 | 96.677 | +1.39% | 80.56 | 80.504 | -0.07% | 151.651 | 150.199 | -0.96% | 159.921 | 158.851 | -0.67% | 0 | 0 |
| hook | source | 32 | 96.792 | 96.728 | -0.07% | 326.784 | 324.561 | -0.68% | 346.226 | 343.602 | -0.76% | 483.487 | 633.759 | +31.08% | 0 | 0 |
| hook | bundle | 1 | 91.335 | 95.906 | +5.00% | 10.919 | 10.292 | -5.74% | 13.324 | 12.216 | -8.32% | 16.935 | 16.786 | -0.88% | 0 | 0 |
| hook | bundle | 8 | 94.547 | 96.397 | +1.96% | 80.546 | 80.416 | -0.16% | 152.166 | 151.044 | -0.74% | 162.862 | 159.47 | -2.08% | 0 | 0 |
| hook | bundle | 32 | 95.484 | 96.536 | +1.10% | 328.665 | 326.302 | -0.72% | 348.475 | 348.246 | -0.07% | 484.383 | 429.642 | -11.30% | 0 | 0 |
| hook-fail-open | source | 1 | 98.052 | 98.729 | +0.69% | 10.098 | 10.093 | -0.05% | 11.672 | 10.612 | -9.08% | 14.985 | 13.127 | -12.40% | 0 | 0 |
| hook-fail-open | source | 8 | 97.979 | 98.38 | +0.41% | 80.518 | 80.579 | +0.08% | 151.236 | 150.992 | -0.16% | 152.121 | 151.833 | -0.19% | 0 | 0 |
| hook-fail-open | source | 32 | 96.847 | 98.847 | +2.07% | 323.154 | 322.919 | -0.07% | 394.561 | 333.038 | -15.59% | 454.797 | 635.457 | +39.72% | 0 | 0 |
| hook-fail-open | bundle | 1 | 97.061 | 97.948 | +0.91% | 10.094 | 10.095 | +0.01% | 11.376 | 11.088 | -2.53% | 20.165 | 15.888 | -21.21% | 0 | 0 |
| hook-fail-open | bundle | 8 | 97.482 | 98.791 | +1.34% | 80.472 | 80.568 | +0.12% | 151.214 | 151.055 | -0.11% | 152.081 | 151.661 | -0.28% | 0 | 0 |
| hook-fail-open | bundle | 32 | 97.991 | 98.277 | +0.29% | 323.246 | 322.992 | -0.08% | 353.659 | 333.324 | -5.75% | 404.544 | 414.168 | +2.38% | 0 | 0 |
| paste | source | 1 | 35.047 | 35.643 | +1.70% | 25.404 | 24.559 | -3.33% | 43.976 | 43.226 | -1.71% | 46.396 | 45.061 | -2.88% | 0 | 0 |
| paste | source | 8 | 73.966 | 71.613 | -3.18% | 87.762 | 93.659 | +6.72% | 193.61 | 184.943 | -4.48% | 270.672 | 231.862 | -14.34% | 0 | 0 |
| paste | source | 32 | 59.236 | 63.015 | +6.38% | 518.789 | 484.598 | -6.59% | 653.883 | 650.368 | -0.54% | 788.869 | 816.804 | +3.54% | 0 | 0 |
| paste | bundle | 1 | 34.059 | 34.113 | +0.16% | 25.682 | 25.207 | -1.85% | 44.637 | 43.07 | -3.51% | 46.762 | 44.311 | -5.24% | 0 | 0 |
| paste | bundle | 8 | 71.125 | 77.086 | +8.38% | 91.565 | 86.775 | -5.23% | 238.519 | 181.332 | -23.98% | 299.781 | 264.491 | -11.77% | 0 | 0 |
| paste | bundle | 32 | 65.809 | 67.461 | +2.51% | 474.284 | 450.113 | -5.10% | 559.299 | 660.093 | +18.02% | 698.907 | 848.319 | +21.38% | 0 | 0 |
| withheld | source | 1 | 97.411 | 98.821 | +1.45% | 70.832 | 70.726 | -0.15% | 141.661 | 141.431 | -0.16% | 152.771 | 151.446 | -0.87% | 0 | 0 |
| withheld | source | 8 | 97.359 | 98.838 | +1.52% | 80.45 | 80.487 | +0.05% | 151.352 | 151.237 | -0.08% | 152.325 | 151.84 | -0.32% | 0 | 0 |
| withheld | source | 32 | 98.255 | 98.323 | +0.07% | 80.45 | 80.471 | +0.03% | 151.253 | 151.25 | -0.00% | 151.898 | 151.772 | -0.08% | 0 | 0 |
| withheld | bundle | 1 | 97.978 | 98.419 | +0.45% | 70.813 | 70.733 | -0.11% | 141.555 | 141.452 | -0.07% | 151.47 | 151.479 | +0.01% | 0 | 0 |
| withheld | bundle | 8 | 98.196 | 98.183 | -0.01% | 80.24 | 80.533 | +0.37% | 151.288 | 151.305 | +0.01% | 151.739 | 151.755 | +0.01% | 0 | 0 |
| withheld | bundle | 32 | 98.207 | 98.125 | -0.08% | 80.35 | 80.467 | +0.15% | 151.248 | 151.214 | -0.02% | 152.196 | 151.798 | -0.26% | 0 | 0 |
| ws | source | 1 | 11.815 | 12.085 | +2.29% | 83.84 | 82.487 | -1.61% | 91.247 | 85.655 | -6.13% | 97.361 | 91.513 | -6.01% | 0 | 0 |
| ws | source | 8 | 10.058 | 10.217 | +1.58% | 97.024 | 96.04 | -1.01% | 114.055 | 110.348 | -3.25% | 117.696 | 114.013 | -3.13% | 0 | 0 |
| ws | source | 32 | 6.216 | 6.24 | +0.39% | 148.256 | 143.379 | -3.29% | 214.701 | 217.849 | +1.47% | 227.461 | 403.824 | +77.54% | 0 | 0 |
| ws | bundle | 1 | 11.846 | 12.125 | +2.36% | 83.653 | 82.282 | -1.64% | 89.736 | 85.235 | -5.02% | 101.754 | 88.836 | -12.70% | 0 | 0 |
| ws | bundle | 8 | 10.054 | 10.082 | +0.28% | 96.45 | 98.2 | +1.81% | 112.021 | 108.001 | -3.59% | 115.463 | 112.173 | -2.85% | 0 | 0 |
| ws | bundle | 32 | 5.96 | 6.183 | +3.74% | 147.857 | 146.485 | -0.93% | 216.391 | 215.083 | -0.60% | 250.684 | 265.94 | +6.09% | 0 | 0 |
| static-shell | source | 1 | 929.707 | 1001.332 | +7.70% | 0.858 | 0.802 | -6.53% | 2.399 | 1.975 | -17.67% | 3.63 | 3.586 | -1.21% | 0 | 0 |
| static-shell | source | 8 | 7953.927 | 8430.075 | +5.99% | 0.773 | 0.734 | -5.05% | 2.701 | 2.473 | -8.44% | 4.012 | 4.194 | +4.54% | 0 | 0 |
| static-shell | source | 32 | 18476.09 | 19645.299 | +6.33% | 1.292 | 1.23 | -4.80% | 4.571 | 4.388 | -4.00% | 6.631 | 6.213 | -6.30% | 0 | 0 |
| static-shell | bundle | 1 | 948.731 | 1014.241 | +6.91% | 0.845 | 0.797 | -5.68% | 2.394 | 1.909 | -20.26% | 3.635 | 3.321 | -8.64% | 0 | 0 |
| static-shell | bundle | 8 | 7954.343 | 7994.016 | +0.50% | 0.793 | 0.77 | -2.90% | 2.612 | 2.659 | +1.80% | 3.928 | 4.23 | +7.69% | 0 | 0 |
| static-shell | bundle | 32 | 18433.402 | 19893.078 | +7.92% | 1.313 | 1.218 | -7.24% | 4.57 | 4.362 | -4.55% | 6.489 | 6.161 | -5.05% | 0 | 0 |
| static-asset | source | 1 | 231.343 | 261.919 | +13.22% | 3.732 | 3.303 | -11.50% | 6.785 | 5.861 | -13.62% | 8.128 | 7.302 | -10.16% | 0 | 0 |
| static-asset | source | 8 | 787.587 | 1004.262 | +27.51% | 7.902 | 6.533 | -17.32% | 24.135 | 19.222 | -20.36% | 30.802 | 29.2 | -5.20% | 0 | 0 |
| static-asset | source | 32 | 1128.182 | 1218.619 | +8.02% | 19.817 | 19.67 | -0.74% | 84.293 | 76.092 | -9.73% | 126.597 | 122.128 | -3.53% | 0 | 0 |
| static-asset | bundle | 1 | 241.426 | 262.274 | +8.64% | 3.585 | 3.094 | -13.70% | 6.602 | 6 | -9.12% | 8.173 | 7.524 | -7.94% | 0 | 0 |
| static-asset | bundle | 8 | 863.863 | 826.694 | -4.30% | 7.575 | 6.861 | -9.43% | 22.2 | 24.268 | +9.32% | 29.529 | 33.091 | +12.06% | 0 | 0 |
| static-asset | bundle | 32 | 1319.23 | 1495.315 | +13.35% | 18.876 | 18.077 | -4.23% | 68.472 | 44.212 | -35.43% | 106.285 | 98.177 | -7.63% | 0 | 0 |

---

## 4. Converted-route workloads

These now run through the Effect bridge (`installEffectRoutes` /
`mapEffectRouteExit` or the hook-specific `mapHookExit`). Fail-open is
still canonical `200 {}` with 0 errors.

### hook — `POST /hook/Notification` (known handler, still `200 {}`)

Serial p95 improved (source -6.95%, bundle -8.32%) and serial rps rose
(+3.28% / +5.00%). N=8 p95 is the same ~151 ms plateau as health. N=32 p95
is flat (-0.76% / -0.07%). The only ugly number is **source N=32 p99
+31.08%** (483.487 → 633.759) while p95 did not move and bundle N=32 p99
went the other way (-11.30%). That is a tail sample (~1 480 ops), not a
bridge regression.

### hook-fail-open — authenticated `POST /hook/Stop` missing `session_id`

p95 improved in every cell (serial -2.53% to -9.08%; source N=32 -15.59%).
rps is flat to slightly up. Source N=32 p99 +39.72% (454.797 → 635.457)
again with p95 improved — same tail-quantile pattern as health/hook at
N=32. Fail-open did not grow a latency tax, and it did not start failing
closed (0 errors, still `200 {}`).

### paste — `POST /api/paste-image` (2 MiB decoded, sync mutating settler)

Serial is unchanged in any budget-relevant sense (p95 -1.71% / -3.51%, rps
+1.70% / +0.16%, persistedExact true both sides, same sha256). Concurrent
paste is the noisiest converted workload:

| cell | rps Δ | p95 Δ | read |
| --- | ---: | ---: | --- |
| source N=8 | -3.18% | -4.48% | within paste scatter |
| source N=32 | +6.38% | -0.54% | no p95 regression |
| bundle N=8 | +8.38% | **-23.98%** | faster, not slower |
| bundle N=32 | +2.51% | **+18.02%** | the only converted p95 that looks like a regression |

The +18% cell is **one-sided** (source N=32 is -0.54%; bundle N=8 is
-24%) and rps still rose. Paste is judged on rps + p95/p99 + correctness,
not the +10% line. Correctness holds. This is not an Effect-shaped
pattern: a real bridge tax would show up on serial paste and on both
targets. See §5 for why paste p95 at N=8 already moved tens of percent
on the *pre-conversion* tree.

### command

**Not exercised.** `POST /command` is converted on this tree; the harness
has no command workload. Do not infer command latency from paste, hook, or
WS.

---

## 5. Ownership-only (and mixed) surfaces — context, not budget

### withheld

Measured signal is **other-connection `GET /health`** while N raw
`POST /hook/Stop` bodies sit past `BODY_DRAIN_GRACE_MS` (not the withheld
socket; `prematureResponseBytes=0` both sides). p95 deltas are all
|Δ| ≤ 0.16%. This is the most repeatable row in the matrix — and it is
sitting on the same 141 / 151 ms timer plateau as health N=8.

### ws

Mutations are serial Notification POSTs (converted hook) + snapshot
fanout (P6.4 converted-by-ownership, pure leaves in `http-policy.ts`;
`/ws/term` untouched). Serial p95 improved ~5–6%. N=8 p95 improved ~3%.
N=32 p95 is +1.47% / -0.60% on n ≈ 90–94 broadcasts. **source N=32 p99
+77.54%** (227.461 → 403.824) is `p99 = max` on 94 samples — one outlier
*is* the p99. rps +0.39% to +3.74%. No Effect-shaped fanout regression.

### static-shell / static-asset

Recorded legacy-until-P13. Largest rps swings in the whole report live
here (static-asset source N=8 **+27.51%**; bundle N=32 **+13.35%** rps and
**-35.43%** p95). These are hashed ~340 KB JS transfers and tiny
no-store HTML. WSL2 page-cache / disk jitter dominates. Not the Effect
bridge.

---

## 6. Noise assessment (do not over-read)

Both captures are **one run** on a WSL2 host, ~9 hours apart (late evening
2026-08-22 vs morning 2026-08-23). The harness itself told operators to
take two quiet-host baselines on the same tree and only then diff; that
pair does not exist. Absolute milliseconds on this host are not a
cross-machine floor.

**Smoke-vs-baseline history** (busy-host harness validation at
`/tmp/p6-http-bench-smoke.json`, `recordedAt` `2026-08-22T19:45:49.657Z`,
`label=smoke`, duration 2 s, warmup 1 s, conc 1,8 only,
`comparison.key` `b577b4bb…` — **not** comparable for the budget, useful
only as a jitter sense, captured on the pre-conversion tree):

| observation | what it says about jitter |
| --- | --- |
| Serial health p95 on smoke: source **19.958** ms vs bundle **10.537** ms | a 2 s busy-host window already spans ~9 ms (~90%) on the *same* tree |
| Quiet serial health p95: 11.089 / 11.285 → 10.687 / 10.663 | well inside that smoke spread; the post “improvement” is not a real speedup |
| Health N=8 p95: smoke 150.970 / 151.463, baseline 151.291 / 151.12, post 151.285 / 151.27 | **locked to ~151 ms across busy smoke, quiet baseline, and post-conversion** |
| State N=8 p95: smoke 151.300 / 151.201 vs quiet 141.864 / 141.994 | ~10 ms (7%) busy-vs-quiet move on the same tree; post stayed on the quiet plateau |
| Paste bundle N=8 p95: smoke **160.109**, quiet baseline **238.519**, post **181.332** | 49% “regression” smoke→baseline *before any Effect route conversion*; post sits between them |
| WS N=8 p95: smoke 88.6 / 88.8 vs quiet 114.1 / 112.0 | ~28% busy-vs-quiet on n ≈ 24; post vs baseline is -3% |
| Static-asset source N=1 rps: smoke 196, baseline 231, post 262 | 30%+ rps swings with no code change in that path |

**Timer plateaus, not Effect:** at N=8, health / hook / hook-fail-open /
withheld p50 clusters at ~80 ms and p95 at ~151 ms on *every* report
including pre-conversion smoke. At N=32 the p50 cluster is ~313–328 ms.
Duration-based in-flight on a single-threaded handler plus Bun timer
quantization produces those steps. Diffing 151.12 vs 151.27 is measuring
the plateau, not the workflow.

**p99 is not a budget metric and is not stable here.** The largest p99
moves are N=32 tails (ws source +77.54% on 94 samples with p99=max;
health bundle +52.70% while max *fell* 737.573 → 656.056; hook source
+31.08%; hook-fail-open source +39.72%). A few samples crossing a
quantile boundary on a 15 s window will do that. State bundle N=1 p99
**-39.07%** (19.538 → 11.905) is the same phenomenon in the other
direction.

**What is inside run-to-run jitter (do not call a regression or a
speedup):** serial p95 moves of a few percent on health/state/hook;
N=8 p95 on the 151 ms plateau; static rps ±10–27%; paste concurrent p95
of tens of percent; any p99 at N=32.

**What would have been a real regression:** serial health/state p95 over
the +10% line on both targets; serial paste p95 up with rps down on both
targets; hook-fail-open growing errors or leaving `200 {}`; a p95 tax
that showed up on source *and* bundle at the same concurrency. None of
those happened.

---

## 7. Overall P6.8 verdict

**Within budget.**

| Question | Answer |
| --- | --- |
| `/health` p95 ≤ baseline+10%, 6 cells | **6/6 PASS** |
| `/state` p95 ≤ baseline+10%, 6 cells | **6/6 PASS** |
| Closest cell | health / bundle / N=32, **+5.406%**, 14.954 ms under the line |
| Correctness | 0 errors, both reports, 54/54 rows |
| Converted hook / fail-open | p95 flat or improved; 0 errors; still fail-open |
| Converted paste | serial unchanged; one one-sided concurrent p95 (+18% bundle N=32) inside pre-existing paste scatter; rps not down |
| `POST /command` | not in the harness |
| WS / static / withheld | ownership-only (WS mixed with converted Notification POSTs); no budget line; deltas are context / cache / timer plateau |
| Rerun before declaring a real regression? | **No** for the +10% budget — no cell fails, the closest is not a one-sided p95 blow-up. A second quiet-host pair would only tighten jitter on paste bundle N=32 and ws source N=32 p99, neither of which is a budget cell. |

P6.8’s process-p95 half remains the P0 exec bench. This document is the
HTTP/WS half.

This file and `p6-postconv.json` are uncommitted evidence.
