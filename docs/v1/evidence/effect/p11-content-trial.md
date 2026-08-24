> **Stamped 2026-08-24** — copied verbatim into the evidence tree from the reconstructed trial report (`/tmp` working drafts lost to a scratch wipe; see the reconstruction note below).

# P11.4 + P11.5 — content trials: **KEEP `node:fs` on both**

*Reconstructed from the trial worker's final report (2026-08-24; full draft + raw probe JSON lost to a /tmp scratch wipe — findings preserved verbatim from the report). Bun 1.3.14, @types/bun 1.3.14, real board-dist fixtures.*

## P11.4 — board static assets via `new Response(Bun.file(p))` → **KEEP** `node:fs` (readFileSync) + `HttpResShim`

Parity fails before any benchmark. Probing against real `board-dist` shows handing `Bun.serve` a `Bun.file` body silently ADDS three behaviors the daemon has never emitted:

- **Range** → returns **206** + `Content-Range` (and **416** unsatisfiable). Decisive: this leaks through even when supplying our own fixed `content-length` header — `Bun.serve` intercepts `Range` for any `Bun.file` body and rewrites status/length. The pinned contract is "always 200, no range."
- **HEAD** → auto-answered 200 headers-only. Today HEAD isn't a public shell (`isPublicShell` is GET-only) and the router is GET/POST-only (`http.ts:2752`/`:2897`), so HEAD → 401/404, never 200.
- **missing file** → `Bun.file` is lazy; serve throws at read → 500 HTML dev dump (66 KB) vs the daemon's clean JSON 404.
- MIME also byte-differs (`text/javascript;charset=utf-8`, no space) unless header-overridden.

The only parity-preserving form is `Bun.file(p).bytes()` → `Buffer` → `res.end` — an async, lazy-error rewrite of `readFileSync` with no measured gain (assets ≤348 KB, committed, page-cache-hot) that also bypasses the audited `HttpResShim` lifecycle. Pure path/header gates (`resolveBoardAssetPath`, `boardAssetHeaders`) already sit outside the file API and stay. Contract floor: `tests/static-serving.test.ts` 1 pass / 0 fail read-only.

## P11.5 — content writes via `Bun.write` → **KEEP** `node:fs` (safe-list is **EMPTY**)

`Bun.write` @1.3.14: creates `0o664` and silently ignores a `{mode}` option, has no `wx` (clobbers), truncates (no append), follows a symlink at the destination, is an in-place truncate+write (stable inode → partial visible to readers), and has no fsync. Every daemon write needs ≥1 property it lacks:

| Site | Mandatory property |
|---|---|
| `program.ts:463/641/682` | PID/TOKEN `0o600` + `wx` |
| `paste.ts:264-265` | temp+`rename` atomic + `0o600` + `wx` |
| `payload-capture.ts:536` | `append` + `0o600` |
| `run-nonce.ts:124` | `0o600` |
| `spawn.ts:268-280` | `open wx` + `handle.sync()` fsync + `link`/`rename` atomic |
| `repos.ts:1210` | atomic dir `rename` |

`Bun.write` + follow-up `fs.chmodSync` is strictly worse (re-imports `node:fs` and opens a TOCTOU `0o664` window).

## §4 register dispositions
- Static board assets (BENCHMARK LATE row) → **KEEP**
- Content reads/writes (SELECTIVE row) → **KEEP** (selective set empty)
- Directories/metadata/permissions/atomic fd I/O → **KEEP** confirmed by measurement

**MIGRATE-later trigger:** Bun.write gains mode/wx/append/fsync semantics or Bun.serve gains an opt-out of Range/HEAD interception for file bodies; re-run the parity probes.
