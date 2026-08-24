> **Stamped 2026-08-24** — copied verbatim into the evidence tree from the reconstructed trial report (`/tmp` working drafts lost to a scratch wipe; see the reconstruction note below).

# P11.8 + P11.9 — sync spawns and detached launchers

*Reconstructed from the trial worker's final report (2026-08-24; full draft + raw probe JSON lost to a /tmp scratch wipe — findings preserved verbatim). Bun 1.3.14, this host.*

## P11.8 — do not treat `Bun.spawnSync` as a daemon drop-in

| Site | Verdict |
|---|---|
| `repo-identity.ts:121` `git()` | **KEEP `execFileSync`**. Daemon event loop; naming/SQL one-tick (`derive.ts:607-609`). `spawnSync` still blocks. |
| `spawn.ts:704` cached `tmux -V` | **DEFER**. Do not promote `spawnSync`. Not startup-only. Async `Bun.spawn`/Effect fiber characterized, not built. |
| CLI (`bin/fleetdeck.ts` doctor) | **KEEP async `execFileP`**. Zero `execFileSync` in `bin/`. `spawnSync` is MIGRATE-eligible only for a future sync CLI path, with a throw/timeout adapter. |

Key evidence: happy path MATCH (`tmux -V` → `tmux 3.7b\n`; git identity; self-path `execPath`/`argv0`; `sh -c` `$0`). **Hazard:** non-zero exit — Node THROWS (status 1/128); Bun RETURNS `{exitCode, success:false}`. Timeout — Node `ETIMEDOUT` throw; Bun `{exitedDueToTimeout:true, signal:SIGTERM}` no throw. ENOENT throws on both. Live `env: process.env` MATCH; OMITTED Bun env is the startup snapshot (marker + fake PATH invisible) — production must keep the explicit pass. Latency n=80: 0.996 vs 0.866 ms (tmux), 1.301 vs 1.226 ms (git) — no reason to touch daemon sites.

**Tmux probe characterization (the DEFER's blueprint):** `probe.at` starts 0 → first call always forks; 60 s TTL then re-forks on the live event loop (`/health`, `/state`, watchdog `hasTmux()` at `spawns.ts:4430`); hang ceiling 1.5 s on `/health`. Recommended async shape (not built): boot-warm + stale-while-revalidate fiber; coalesce in-flight; never return the cold `available:false` / `tmux 3.4+ required` sentinel as "tmux missing" (hides spawn UI). Deferred as a product-behavior change (post-migration / P13 cleanup candidate).

## P11.9 — KEEP all three detached launchers on `node:child_process.spawn`; never `ProcessRunner`

| Launcher | Verdict |
|---|---|
| L1 `bin/fleetdeck.ts:1074` `sh SUPERVISE_SH` | **KEEP**. `Bun.spawn` host-parity proven; conversion later, not this trial. |
| L2 `scripts/fleet-sessionstart.ts:313` | **KEEP** (hook floor). |
| L3 `spawn.ts:1383` `launchOverride` | **KEEP** (must outlive daemon shutdown). |

No board-opener/`xdg-open`. `termbridge.ts:532` and `process-driver.ts:364` `detached` are kill-tree ownership, not surviving launchers. None of L1–L3 go through `ProcessRunnerLive` (bundle pin: no `BunChildProcessSpawner`).

Key evidence: `detached:true` → session/pgroup leader (`pid==pgid==sid`), survives parent, reparented (WSL ppid 636). `unref` is load-bearing without `process.exit` (parent drains 52–55 ms with unref; still alive at 1.5 s without — both APIs). Inherited log fd survives parent `closeSync`. Launcher SIGTERM does not kill the detached child; direct SIGTERM does; the mini-supervisor trap forwards; `argv = ['sh', SUPERVISE_PATH]` satisfies `argvIsOurSupervisor`. `ProcessRunner` (30 s default timeout, join, `forceClose` SIGKILL of `killTree` group) would break L1–L3. argv/no-shell throughout; no Bun Shell.
