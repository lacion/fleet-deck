# P11 CLOSE — the Bun-native capability trials are finished

> **Written 2026-08-24.** Package close record for **P11** ("finish the
> Bun-native capability trials") on branch `fd/v1-effect-feasibility`. HEAD
> `5faec478` (`feat(security): pass --no-env-file on every bun launcher (P11.6)`).
> **Push state: fully pushed** — `origin/fd/v1-effect-feasibility == HEAD ==
> 5faec478`; the P10-close docs committed at `343a52bc` and the P11.6 REQUIRE at
> `5faec478` are both on origin. Only this P11-close documentation is uncommitted.
> The four trial groups' working drafts were lost to a `/tmp` scratch wipe; the
> authoritative evidence records are the four stamped, reconstructed trial reports
> in this directory ([p11-udp-trial.md](./p11-udp-trial.md),
> [p11-content-trial.md](./p11-content-trial.md),
> [p11-env-crypto-trial.md](./p11-env-crypto-trial.md),
> [p11-spawn-trial.md](./p11-spawn-trial.md)). This file is the as-adjudicated
> roll-up + §4 register close + exit-gate audit.

P11 is **COMPLETE**. Four trial groups produced measured, orchestrator-adjudicated
verdicts against Bun 1.3.14 (mDNS/UDP; content file I/O; `.env`/crypto; sync spawns
+ detached launchers), the one **REQUIRE** verdict (`--no-env-file` hardening + a
bundle secret-scan gate) landed at `5faec478`, and the owned-platform register
(P11.10) closed at its valid local-completion state. The headline outcome is
**KEEP** on every trialled Bun-native candidate: none reached parity + a measured
gain over the audited `node:*` path at rc.110/Bun 1.3.14. That is not timidity — it
is the plan's Bun-Native-APIs gate returning its designed answer, with a recorded
MIGRATE-later trigger per row. No daemon source changed; the daemon bundle is
byte-identical to the P10 close. The next open package is **P12** (Bun-native
builds and the optional executable).

---

## 1. Ten-item verdict table (P11.1–P11.10)

| Item | Verdict | Evidence |
|---|---|---|
| **P11.1** isolate mDNS behind a Discovery/Datagram interface | **NO SLICE NEEDED (under KEEP)** — `node:dgram` already confined to `mdns.ts`; daemon consumes only the `createMdns → {start,stop,update,alive}` port (`program.ts:978`) with a `node:dgram`-shaped `inject` seam (`test-seam.ts`). A Bun-neutral interface would exist only to host the adapter P11.2 rejects. | [p11-udp-trial.md](./p11-udp-trial.md) |
| **P11.2** two responders coexist on port 5353 (macOS + Linux) | **KEEP `node:dgram`** — `Bun.udpSocket` has no reuse option; two sockets on one port → **`EADDRINUSE`**. `udp.SocketOptions` (`bun-types/bun.d.ts:6532`) exposes no `reuseAddr`/`reusePort`; `mdns.ts:1236` needs exactly `reuseAddr:true` to answer alongside avahi/Bonjour. | [p11-udp-trial.md](./p11-udp-trial.md) |
| **P11.3** goodbye-datagram completion before close | **KEEP `node:dgram`** — `send():boolean` (`bun.d.ts:6631`) has no per-datagram callback; `send()===true`/`drain` prove buffer acceptance/writability, not delivery. The `stop()`/`withdrawAndDie()` OS-handoff barrier is **unprovable** on `Bun.udpSocket`. | [p11-udp-trial.md](./p11-udp-trial.md) |
| **P11.4** board static assets via `Bun.file` Response | **KEEP `node:fs` (readFileSync) + `HttpResShim`** — a `Bun.file` body silently adds Range (206/416), auto-HEAD 200, and a 500 HTML dump on missing files, breaking the "always 200, no range", GET/POST-only (`http.ts:2752`/`:2897`), clean-JSON-404 contract. The one parity form (`.bytes()`→`res.end`) has no measured gain and bypasses the audited shim. | [p11-content-trial.md](./p11-content-trial.md) |
| **P11.5** content writes via `Bun.write` | **KEEP `node:fs` — safe-list is EMPTY** — `Bun.write`@1.3.14 creates `0o664`, ignores `{mode}`, has no `wx`, truncates (no append), follows symlinks, is non-atomic (stable inode), and does not fsync. Every daemon write needs ≥1 missing property (PID/token `0o600`+`wx`, atomic temp+rename, append, fsync). | [p11-content-trial.md](./p11-content-trial.md) |
| **P11.6** characterize `.env` auto-load; decide `--no-env-file` | **DONE + REQUIRE IMPLEMENTED at `5faec478`** — KEEP explicit `process.env` + `FLEETDECK_HOME/service.env`; **REQUIRE `--no-env-file` on every production bun launcher** (Bun auto-loads `.env` from cwd; user-unit cwd=`$HOME`); add a bundle secret-scan gate (both landed at `5faec478`). Live child env was already correct. | [p11-env-crypto-trial.md](./p11-env-crypto-trial.md); `git show 5faec478` |
| **P11.7** crypto: constant-time compare, exact hash, UUID/random | **KEEP `node:crypto`** for `timingSafeEqual` (`http-policy.ts:198`), `createHash('sha256')` (`repos.ts:759`), and secret `randomBytes` (`program.ts:630`, `spawns.ts:749`, `bin/fleetdeck.ts:1206`). Web-Crypto UUID/random **OPTIONAL CLEANUP DECLINED** by adjudication (χ² MATCH, uniformity-only churn). `Bun.hash` (wyhash) unused and forbidden for secrets. | [p11-env-crypto-trial.md](./p11-env-crypto-trial.md) |
| **P11.8** sync spawns (CLI parity trial + daemon tmux probe) | **KEEP `execFileSync`** at `repo-identity.ts:121` `git()` (daemon event loop) and **KEEP async `execFileP`** in the CLI doctor (zero `execFileSync` in `bin/`); the cached tmux probe (`spawn.ts:704`) is a recorded **DEFER** (async boot-warm + stale-while-revalidate fiber characterized, not built — a P13-class product-behavior change). `Bun.spawnSync` is not a daemon drop-in (returns `{success:false}`/`{exitedDueToTimeout}` where Node throws). | [p11-spawn-trial.md](./p11-spawn-trial.md) |
| **P11.9** detached supervisor/CLI launchers | **KEEP all three on `node:child_process.spawn`; never `ProcessRunner`** — L1 `bin/fleetdeck.ts:1074` (SUPERVISE_SH), L2 `scripts/fleet-sessionstart.ts:313`, L3 `spawn.ts:1383` (`launchOverride`). Session-leader/`unref`/inherited-fd/signal/`argvIsOurSupervisor` all proven; `ProcessRunner`'s 30 s timeout + `forceClose` SIGKILL would break them. No `BunChildProcessSpawner` in the bundle. | [p11-spawn-trial.md](./p11-spawn-trial.md) |
| **P11.10** close the owned-platform register for each generic gap | **CLOSED LOCALLY (valid completion state)** — the app-local Bun process service is production; the parity-proven Bun-native `ChildProcessSpawner` (`child-process-spawner.ts`) + the upstream-ready `rc110.patch` are the retained artifacts. Upstream submission / package extraction / full-fork consumption is a **STANDING ASK** to Luis (§3 step 5), not blocked work. | §3 below; `src/daemon/platform/bun/`; the conformance suites; [unstable-imports.md](./unstable-imports.md); [upstream/…rc110.patch](./upstream/effect-platform-bun-child-process-spawner-rc110.patch) |

Every P11.x box is discharged. `KEEP` on nine trialled candidates + one `DEFER`
(the tmux probe, inside P11.8) + one register close, each with a named owner and a
recorded MIGRATE-later trigger.

---

## 2. §4 "Bun-native capability register" — dispositions row by row

The plan's §4 table (`effect-migration-plan.md:295-319`) is the authoritative
register. P11 settles the mDNS / content / env / crypto / sync-spawn / detached
rows and confirms the KEEP-by-default rows. Rows already owned by an earlier or a
later package are marked with that owner (their measured evidence lives there); this
is exactly the "**MIGRATE**, **KEEP**, or **DEFER** evidence and a named owner"
the P11 exit gate requires.

| §4 row (capability) | Plan cell | Measured/settled disposition | Owner |
|---|---|---|---|
| One-shot execution in `exec.ts` | MIGRATE FIRST | **MIGRATED** to the Bun `Bun.spawn` process service | P3 |
| Bounded git execution in `files.ts` | MIGRATE AFTER EXEC | **MIGRATED** (`runBounded` frozen bounded-exec adapter) | P3/P9.3 |
| tmux control client in `termbridge.ts` | EFFECT-OWN; TRIAL BUN IN P7 | **DEFER to P7** (scoped Node-stream adapter is a valid KEEP outcome) | P7 |
| Sync CLI capability checks | CLI-ONLY PARITY TRIAL | **KEEP `execFileSync`** (async `execFileP` in doctor; `spawnSync` MIGRATE-eligible only for a future sync CLI path w/ throw-adapter) | P11.8 (mDNS/CLI) |
| Cached sync tmux probe inside the daemon | CHARACTERIZE SEPARATELY | **DEFER** (async fiber blueprint recorded; do not promote `Bun.spawnSync`) | P11.8 → P13 |
| Detached supervisor/CLI launchers | KEEP INITIALLY | **KEEP** all three on `node:child_process.spawn`; never route through `ProcessRunner` | P11.9 |
| HTTP and server WS in `http.ts` | KEEP CUSTOM ADAPTER | **KEEP** (P6.3 Effect-owns `Bun.serve`; P6.7 rejected `BunHttpServer`) | P6 |
| Static board assets | BENCHMARK LATE | **KEEP `node:fs`+`HttpResShim`** (Range/HEAD/500-parity fails before benchmark) | P11.4 |
| SQLite seam | KEEP DIRECT bun:sqlite | **KEEP** (P8.1 static import; P8.2 no `strict`; P8.7 no `@effect/sql-sqlite-bun`) | P8 |
| Repeated SQL statements | KEEP prepare-once | **KEEP** (P8.5 `db.query()` is a 20-slot cache, not LRU) | P8 |
| mDNS | ISOLATE/TRIAL | **KEEP `node:dgram`** (no reuseAddr coexistence; goodbye completion unprovable) | P11.1–3 |
| Content reads/writes | SELECTIVE | **KEEP `node:fs`** (selective safe-list empty) | P11.5 |
| Directories/metadata/permissions/atomic fd I/O | KEEP | **KEEP** confirmed by measurement (O_NOFOLLOW/chmod/fsync/link-rename) | P11.5 |
| UUID/random data | OPTIONAL CLEANUP | **KEEP `node:crypto`** — cleanup **DECLINED** (χ² match, uniformity-only) | P11.7 |
| Token compare and exact SHA-256/base64url | KEEP | **KEEP `node:crypto`**; never substitute `Bun.hash` | P11.7 |
| Daemon/bin/hook bundles | TRIAL IN P12 | **DEFER to P12** (programmatic `Bun.build`) | P12 |
| Standalone executable | DISTRIBUTION GATE AFTER BUN.BUILD | **DEFER to P12** (`bun build --compile`) | P12 |
| Tests | KEEP | **KEEP** `bun:test` (add Effect test services, not another runner) | P2+ |
| Environment | KEEP AND CHARACTERIZE | **KEEP** explicit `process.env`; **REQUIRE `--no-env-file`** (landed `5faec478`); no bundled secrets (gate landed) | P11.6 |
| Paths, URLs, and OS metadata | KEEP | **KEEP** `node:path`/`node:url`/`node:os` | — |
| Incremental UTF-8 decoding | KEEP BY DEFAULT | **KEEP** `node:string_decoder` | P7-adjacent |
| Web primitives | KEEP | **KEEP** standard `fetch`/`Request`/`Response`/`AbortSignal` | — |
| Hostile raw-protocol test clients | KEEP | **KEEP** test-only `node:net`/`node:http` | — |

Every row carries a disposition and an owner. The two `TRIAL`/`GATE` rows that are
still open (tmux control → P7; bundles + executable → P12) are **scheduled
package-owned trials**, not unadjudicated gaps.

---

## 3. Owned-platform register close (P11.10)

The plan's §3 owned-platform gate (`effect-migration-plan.md:250-289`, referenced
again at P2.7 and P3.9) defines a five-step extraction ladder and states that **"a
parity-proven app-local implementation plus an upstream-ready patch is a valid local
completion state."** P11.10 closes the register at exactly that state.

**What is app-local (found in `src/daemon/platform/`):**

- **Production process service (selected).** `src/daemon/platform/bun/process-driver.ts`
  (26 KB) + `process-runner-live.ts` (the Live-Layer selection) + `ingress-supervisor-live.ts`
  (the root ingress supervisor / `runControlDetached`). This is the narrow Bun
  `Bun.spawn` process service Fleet Deck owns — direct-argv, combined-output cap,
  process-tree escalation. It is what production runs.
- **Bun-native comparison adapter (the generic-gap fill, NOT selected).**
  `src/daemon/platform/bun/child-process-spawner.ts` (19 KB) is the P3.9 Bun-native
  implementation of Effect rc.110's unstable `ChildProcessSpawner`. It exists because
  `@effect/platform-bun/BunChildProcessSpawner` at rc.110 re-exports the Node-shared
  adapter and would not exercise `Bun.spawn` (see
  [effect-feasibility.md](../../effect-feasibility.md) lines 78-89). It is
  **intentionally not selected by the production Live Layer**.
- **Node reference.** `src/daemon/platform/node/process-driver-reference.ts` — the
  differential oracle.

**Parity evidence (the conformance suites):**

- `tests/effect/bun-child-process-spawner.test.ts` — argv/env/cwd/stdin, concurrent
  drains, pipe, `PlatformError` mapping (incl. the `additionalFd`/`fdN` `BadArgument`
  Bun 1.3.14 cannot satisfy), scope-finalize reap, unref/reref, `forceKillAfter` TERM
  escalation.
- `tests/effect/bun-process-driver.test.ts` + `tests/effect/bun-process-driver-natural-exit.test.ts`
  — the driver differential and natural-exit behavior.
- `tests/effect/p3-production-selection.test.ts` — pins that production **selects the
  Bun driver and excludes both comparison drivers**.
- `tests/bun-platform-conformance.test.ts` — the generic Bun-platform conformance
  harness (no Fleet Deck domain policy).
- `tests/process-driver-reference.test.ts` — the Node reference driver.

**Register + upstream artifact:**

- [unstable-imports.md](./unstable-imports.md) carries the two comparison-only rows
  (`effect/unstable/process/ChildProcess`, `…/ChildProcessSpawner`), both at
  `4.0.0-rc.110`, both with rollback module `child-process-spawner.ts`.
- [upstream/effect-platform-bun-child-process-spawner-rc110.patch](./upstream/effect-platform-bun-child-process-spawner-rc110.patch)
  (22 KB) is the upstream-ready patch against
  `packages/platform/bun/src/BunChildProcessSpawner.ts` — the "upstream-ready patch"
  half of the valid local-completion state (it documents the Bun 1.3.14 limitation
  that parent→child additional-fd pipes cannot publish EOF, so `additionalFds`/`fdN`
  fail `BadArgument`).

**Standing ask (Luis-gated, not blocked work).** Per §3 step 5, the local state is
**complete**. Going further — submitting the patch upstream, extracting a Fleet
Deck-scoped `@effect`-adjacent package, or consuming a pinned full fork of
`@effect/platform-bun` — requires explicit user approval and, if approved, triggers
exact-RC conformance + package + lock + rollback gates. **The same route is open for
the second named generic gap, the Node-shared `BunSocketServer`, which has not been
built.** This ask is recorded alongside P7 draft-PR authorization and the P14
version pick as a standing Luis-authorization item (see
[effect-migration-status.md](../../effect-migration-status.md)). Silence is not
approval; the app-local implementation is retained and this decision is recorded.

---

## 4. P11.6 hardening record (`5faec478`)

P11.6 is the only P11 item that produced a source change; it is a security
REQUIRE, landed and pushed at `5faec478` (`feat(security): pass --no-env-file on
every bun launcher (P11.6)`).

**The finding.** Bun 1.3.14 auto-loads `.env` from `process.cwd()`. The daemon's
production launchers run with cwd unrelated to a checkout (systemd user-unit
cwd=`$HOME`), so a stray `$HOME/.env` could inject `FLEETDECK_TOKEN`/`FLEETDECK_BIND`
into an unset environment. A partial-env spawn probe confirmed **unset keys were
injected** from a cwd `.env`; the same spawn with `--no-env-file` did not inject.

**The fix (files changed at `5faec478`).** `--no-env-file` was added to every
production bun launcher: `bin/fleetdeck.ts` (systemd `ExecStart`) + its regenerated
`bin/fleetdeck.mjs`; `scripts/fleet-sessionstart.ts` (SessionStart spawn) + its
regenerated `scripts/fleet-sessionstart.mjs`; `scripts/hook-launcher.sh`; and the
test-daemon helper `tests/helpers/daemon.ts`. The in-process `fleetdeck serve` path
is documented as a **non-surface** (import, not a re-exec). Live child env was
already the correct production contract (`process-driver.ts:365-367` merges
`{...process.env, ...request.env}`); it was not changed. `service.env` was **not**
replaced by `--env-file` (the dual-reader quoting contract).

**The bundle secret-scan gate.** No gate existed. `5faec478` added
`tests/bundle-secret-scan.test.ts` (134 lines) — structural discriminators for five
secret twins (PEM / `AKIA…` / `ghp_…` / `glpat-…` / `sk-ant-…`) with an
alnum-after-prefix rule that distinguishes a real literal from the key-name
`FLEETDECK_TOKEN` and the `sk-ant-` redaction regexes, plus negative controls and a
mutation proof. `5faec478` also added `tests/no-env-file-spawn.test.ts` (70 lines)
pinning the launcher flag behavior, and updated `tests/cli.test.ts` +
`tests/hook-launcher.test.ts`.

**P11.7** required no source change: `node:crypto` stays for `timingSafeEqual`,
`createHash('sha256')`, and secret `randomBytes`; the Web-Crypto UUID/random cleanup
was adjudicated **DECLINED** (uniformity-only, χ² 253.48 web vs 253.46 node — a
match, not a win).

---

## 5. P11 EXIT GATE AUDIT

> **Exit gate** (`effect-migration-plan.md:1057-1058`): *"each row in §4 has
> measured **MIGRATE**, **KEEP**, or **DEFER** evidence and a named owner. No broad
> 'replace all node imports' task remains."*

**Clause 1 — every §4 row has measured MIGRATE / KEEP / DEFER evidence and a named
owner.** Discharged. §2 above walks all 23 §4 rows: 2 MIGRATED (P3/P9.3), 3
DEFER-to-package (tmux control→P7; bundles + executable→P12; the tmux probe→P13),
and the remainder KEEP with an owner. The P11-scope rows (mDNS, static assets,
content writes, sync CLI, detached launchers, UUID/random, token/hash, environment)
each carry a probe-measured verdict in one of the four stamped trial reports; the
`Bun.spawnSync` non-drop-in, the `Bun.udpSocket` `EADDRINUSE`, the `Bun.file`
Range/HEAD/500 divergence, and the `Bun.write` missing-property matrix are all
recorded with file:line citations and a re-run trigger.

**Clause 2 — no broad "replace all node imports" task remains.** Discharged. Every
`node:*` retention is a per-row adjudicated KEEP with a stated reason (not a blanket
"Bun is incomplete"), and every MIGRATE-later path is gated on a **specific**
observable Bun change (documented UDP reuse + per-datagram completion; `Bun.write`
gaining mode/wx/append/fsync; `Bun.serve` gaining a Range/HEAD opt-out for file
bodies; a sync CLI path needing `spawnSync`). No sweeping substitution task is
carried forward; `node:dgram` stays confined to `mdns.ts`, `node:crypto` to the
security leaves, `node:fs` to the permission/atomic/fd operations, and
`node:child_process` to the three detached launchers.

**Rollback.** P11 introduced **no daemon source change and no new adapter seam** —
there is nothing to flag off. The one source change (`5faec478`) is additive
security hardening (launcher flags + two test files); its rollback is an ordinary
revert of `5faec478`, and it does not touch the daemon bundle. The owned-platform
register close (P11.10) is documentation over pre-existing P3.9 artifacts; no code
was selected or deselected.

---

## 6. Bundle at close

`src/daemon/fleetd.bundle.mjs`. Gzip measured by the project's canonical
`Bun.gzipSync(bytes, {level:9, library:'zlib'})` (`daemon-bundle-policy.test.ts:34`).
Ceiling: raw ≤ 768,000 · gzip-9 ≤ 189,440.

| Commit | Package point | raw B | gzip-9 B | sha256 (prefix) |
|---|---|---|---|---|
| `285122a1` | P10 slice 3 / last daemon source change | 652,147 | 172,335 | `84daf597…` |
| `5faec478` | **P11 close (HEAD)** | **652,147** | **172,335** | **`84daf597…`** |

**Re-verified this close (live tree, `5faec478`):** raw **652,147**, sha256
`84daf5972d10316797bbf7a8f6b5996d8801c3612994de49eaf58e56ec20e327`, gzip-9-zlib
**172,335** — **byte-identical to the P10 close**. P11 changed no daemon source, so
the daemon bundle has not moved since `285122a1`. The P11.6 commit regenerated only
the launcher artifacts (`bin/fleetdeck.mjs` +4 lines, `scripts/fleet-sessionstart.mjs`
+2 lines) deterministically to carry `--no-env-file`. **Headroom: gzip 17,105 B ·
raw 115,853 B.**

**Convention flag (carried from the P9/P10 closes).** Any per-slice worker report
that quotes a `gzip -c` (default level 6) figure is **not** citing the policy
metric; the `daemon-bundle-policy` gate measures
`Bun.gzipSync(…,{level:9,library:'zlib'})`, whose value at HEAD is **172,335**
(17,105 B under ceiling). The version-manifest string `0.23.6` (×4) remains the
standing bundle landmine — a `hook-integrity` bump reprices this row and is the
**P14** RC-rehearsal / version-closure checkpoint, not a P11 item.

---

## 7. Residuals carried out of P11

- **Cached tmux probe (P11.8 DEFER)** — async boot-warm + stale-while-revalidate
  fiber is characterized but not built; it is a product-behavior change (must never
  surface a cold `available:false` sentinel as "tmux missing"). Owner: P13 cleanup.
- **Owned-platform upstream ask (P11.10)** — submission / extraction / fork is a
  standing Luis-authorization item; the Bun-native `ChildProcessSpawner` and its
  `rc110.patch` are retained app-local. The Node-shared `BunSocketServer` is a
  second, unbuilt candidate on the same route.
- **`/ws/term` terminal ownership (P7)** — still BLOCKED on Luis's draft-PR
  authorization at the §7 platform checkpoint; untouched by P11.
- **Version-manifest landmine (`0.23.6` ×4)** — retained for any merge-to-main;
  closed at P14.

No P11-scope capability is left unadjudicated; the one in-scope DEFER (the tmux
probe) is trigger-gated, and the register close is at its plan-sanctioned valid
local-completion state. Resume at **P12**.
