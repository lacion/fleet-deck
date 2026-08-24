> **Stamped 2026-08-24** — copied verbatim into the evidence tree from the reconstructed trial report (`/tmp` working drafts lost to a scratch wipe; see the reconstruction note below). The P11.6 REQUIRE verdict has since landed as commit `5faec478`.

# P11.6 + P11.7 — `.env` auto-load and crypto

*Reconstructed from the trial worker's final report (2026-08-24; full draft + raw probe JSON lost to a /tmp scratch wipe — findings preserved verbatim). The P11.6 REQUIRE verdict has since been IMPLEMENTED at commit `5faec478` (--no-env-file on every launcher + the bundle secret-scan gate).*

**Verdicts**

| Item | Verdict |
|---|---|
| **P11.6** | KEEP explicit `process.env` + `FLEETDECK_HOME/service.env`; **REQUIRE `--no-env-file` on production bun launchers** (implemented at `5faec478`). Live child env already correct. Bundle secret-scan gate did not exist (added at `5faec478`). |
| **P11.7** | **KEEP `node:crypto`** for `timingSafeEqual`, `createHash('sha256')`, and secret `randomBytes`. UUID/random OPTIONAL CLEANUP **DECLINED by adjudication** (uniformity-only churn). `Bun.hash` unused and forbidden for secrets. |

**Key evidence — P11.6 bun 1.3.14 auto-load semantics**
- Loads from `process.cwd()` only — not script dir, not parent walk-up, not `$HOME` unless cwd is `$HOME`, not package.json root unless that dir is cwd.
- Mode stack (highest wins): `.env.$(MODE).local` > `.env.local` (skipped when `MODE=test`) > `.env.$(MODE)` > `.env`. `MODE` = `BUN_ENV` else `NODE_ENV` else `development`. `NODE_ENV=""` still selects development files. Process env (including empty string) always beats files. `BUN_ENV=production` + `NODE_ENV=development` loads production files.
- `--no-env-file` disables auto-load; `--env-file` still applies in either argv order. Shebang `#!/usr/bin/env -S bun --no-env-file` works.
- Partial-env spawn with cwd `.env`: set keys held; UNSET keys (e.g. `FLEETDECK_TOKEN`/`FLEETDECK_BIND`) were INJECTED. Same spawn + `--no-env-file`: no injection.
- Launch surfaces (none passed the flag pre-`5faec478`): systemd `ExecStart` (`bin/fleetdeck.ts:598`, user-unit cwd=`$HOME`); `supervise.sh:634`; in-process `fleetdeck serve` (later ruled a non-surface — import, no re-exec); `hook-launcher.sh:49` + SessionStart `spawn(process.execPath, [FLEETD], { env: bootEnv() })`; `tests/helpers/daemon.ts:191`.
- Live child env: already the production contract — `process-driver.ts:365-367` merges `{ ...process.env, ...request.env }`; same live object on `spawn.ts:711`/`1395`, `termbridge.ts:536`, `repo-identity.ts:128`. Pin: `tests/exec-timeout.test.ts:241`. Do NOT replace `service.env` with bun `--env-file` (dual-reader quoting contract).
- Bundle secret-scan: no gate existed; manual grep of committed bundles found no PEM/`AKIA`/`ghp_`/`glpat-` literals; `FLEETDECK_TOKEN` appears as a key name; `sk-ant-` as redaction regexes. (Gate added at `5faec478` with structural discriminators for exactly those twins.)

**Key evidence — P11.7 inventory (`src/daemon`, file:line)**
- KEEP: `http-policy.ts:198` `timingSafeEqual` (auth); `repos.ts:759` `createHash('sha256').digest('base64url')` (sync cache key); `app/program.ts:630` `randomBytes(32)` token mint; `spawns.ts:749` `randomBytes(24)` arm token; `bin/fleetdeck.ts:1206` token `--rotate`.
- OPTIONAL-CLEANUP sites (declined): `run-nonce.ts:122`, `paste.ts:257`, `spawn.ts:264/570/939/1292`, `spawns.ts:1715/1887-1888/2245-2246/3026`, `program.ts:739`, `scripts/fleet-watch.ts:244`.
- Web Crypto trial: 4000 UUIDs unique/shape/v4/RFC-variant all pass; 2000×32 bytes χ² web 253.48 vs node 253.46 — MATCH, but conversion declined. `Bun.hash` = wyhash bigint; zero src/bin/scripts callsites.
