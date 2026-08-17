// tests/cli-serve-paths.test.ts
//
// Regression for BUG-074: `fleetdeck serve` used to build the daemon's module
// specifier by string-concatenating `file://${FLEETD}`. Under a legal install
// path containing `#` or `?` the fragment/query delimiter truncated the module
// path (ERR_MODULE_NOT_FOUND), and a raw percent sequence raised URIError — so
// standalone/supervised Fleet Deck never booted from such a prefix. The fix
// imports `pathToFileURL(FLEETD).href`, which escapes the path correctly.
//
// These tests pack a minimal fake runtime (the real CLI + a stub bundle that
// prints a marker instead of booting a daemon) into a directory whose name
// carries the offending characters, then run `serve` as a child process. No
// daemon, no tmux, no port: the stub bundle lets the event loop drain and the
// child exits 0 on its own.

import test, { after } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'fleetdeck-serve-paths-'));

after(() => {
  try {
    fs.rmSync(TMP, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
});

// Lay out the shape `serve` resolves against: the built, self-contained
// bin/fleetdeck.mjs artifact (esbuild has inlined tmux-version into it, so no
// sibling source file is needed) and scripts/fleetd/fleetd.bundle.mjs, which
// takes precedence over the source fallback exactly as in a packed install.
function packFakeRuntime(prefix: string) {
  const bin = path.join(prefix, 'bin');
  const fleetdDir = path.join(prefix, 'src', 'daemon');
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(fleetdDir, { recursive: true });
  const cli = path.join(bin, 'fleetdeck.mjs');
  fs.copyFileSync(path.join(REPO, 'bin', 'fleetdeck.mjs'), cli);
  fs.chmodSync(cli, 0o755); // the shebang can only launch it if it stays executable
  fs.writeFileSync(
    path.join(fleetdDir, 'fleetd.bundle.mjs'),
    `process.stdout.write('FLEETD_LOADED\\\\n');\n`,
  );
  return cli;
}

// Boot the packed CLI through its own shebang (`#!/usr/bin/env bun`), exactly as
// the OS runs the installed `fleetdeck` command — NOT via `process.execPath`, so
// the runner (`bun test` or `node --test`) is irrelevant: the shebang always
// pins the CLI to Bun. It has to be Bun — `serve` loads the daemon IN-PROCESS via
// `await import(pathToFileURL(FLEETD).href)` (bin/fleetdeck.ts), and the daemon
// bundle resolves `bun:sqlite`, which Node cannot. That in-process import IS
// BUG-074's fix: `pathToFileURL` escapes a `#` or `%` in the daemon path so the
// module specifier is not truncated (raw string-concat used to break here). A `?`
// is NOT safe even here under Bun — Bun decodes the href and re-splits the path at
// the `?`; see serveCannotBootUnderQueryPrefix.
function serveBoots(prefix: string) {
  const cli = packFakeRuntime(prefix);
  const res = spawnSync(cli, ['serve'], { encoding: 'utf8', timeout: 15000 });
  assert.equal(res.error, undefined, `child must not fail to spawn: ${res.error}`);
  assert.equal(
    res.status,
    0,
    `serve must exit cleanly under ${JSON.stringify(prefix)}\nstderr: ${res.stderr}`,
  );
  assert.ok(res.stdout.includes('FLEETD_LOADED'), 'the daemon bundle was actually imported');
}

// A `?` anywhere in the install PREFIX is unsupported under Bun, at TWO layers:
// (1) Bun's main-entrypoint resolver reads the `?` in the script path the kernel
// hands it as a module query and truncates there, so `bun <prefix>?…/fleetdeck.mjs`
// cannot even load the CLI; and (2) even past that, `import(pathToFileURL(FLEETD))`
// fails too — Bun decodes the `%3F` back to `?` and re-splits the path, so BUG-074's
// escaping does not save it (Node loads the identical file URL per WHATWG spec).
// Node did neither, but under the single-runtime model the CLI only ever runs under
// Bun, and there is no in-repo fix: the kernel execs the shebang with the literal
// `?`-bearing path as Bun's argv, and the query-split is intrinsic to Bun's loader.
// (Bun query-STRIPS rather than errors, so a `?` prefix could even run a sibling at
// the truncated path if one existed — the FLEETD_LOADED assertion guards that too.)
// So a `?` in the prefix is an accepted, documented limitation of the Bun-only
// install (npm/pnpm bin dirs never contain `?`; it is illegal in a Windows path).
// Pinned as an assertion — not skipped — so a Bun that stops query-splitting decoded
// paths at BOTH layers flips this test and prompts us to restore full support.
function serveCannotBootUnderQueryPrefix(prefix: string) {
  const cli = packFakeRuntime(prefix);
  const res = spawnSync(cli, ['serve'], { encoding: 'utf8', timeout: 15000 });
  assert.equal(
    res.error,
    undefined,
    `Bun still launches; only entrypoint resolution fails: ${res.error}`,
  );
  assert.notEqual(
    res.status,
    0,
    `a '?' in the prefix truncates Bun's entrypoint path, so serve cannot boot under ${JSON.stringify(prefix)}`,
  );
  assert.ok(
    !res.stdout.includes('FLEETD_LOADED'),
    'the daemon must never load when Bun cannot resolve the CLI entrypoint',
  );
}

// Realistic hostile prefixes Bun's entrypoint resolver AND its file-URL importer
// both handle — each guards BUG-074's `pathToFileURL` escaping for its character
// class. `%zz` is a raw percent followed by a non-hex sequence (decodeURIComponent
// would throw URIError on it). The final case combines the classes so an escaping
// interaction between them can't slip. (`?` is deliberately absent — it is
// unsupported under Bun; see queryPrefixed below.)
const bootable: [string, string][] = [
  ['a fragment delimiter (#)', 'install#fragment'],
  ['a raw percent sequence (%)', 'install%zz'],
  ['spaces', 'install dir with spaces'],
  ['a fragment, spaces, and a percent together', 'fleet deck #1 x=%zz'],
];
for (const [label, dirname] of bootable) {
  test(`serve: loads the daemon from an install path containing ${label}`, () => {
    serveBoots(path.join(TMP, dirname));
  });
}

// A `?` in the prefix (alone, or combined with the other characters) is
// unsupported under the Bun single-runtime model — see the function above.
const queryPrefixed: [string, string][] = [
  ['a query delimiter (?)', 'install?query'],
  ['all of them at once', 'fleet deck #1?x=%zz'],
];
for (const [label, dirname] of queryPrefixed) {
  test(`serve: a '?' install prefix (${label}) is unsupported under Bun-only`, () => {
    serveCannotBootUnderQueryPrefix(path.join(TMP, dirname));
  });
}

// Wrong-RUNTIME guard (the v0.23.0 upgrade-path landmine). From 0.23.0 the daemon
// bundle calls `Bun.serve` unconditionally, so `serve` must run under Bun. An
// in-place upgrade (`npm i -g fleetdeck@latest`) refreshes the package but does
// NOT rewrite an already-installed unit, so a pre-0.23.0 `ExecStart=node …serve`
// keeps pointing Node at a now-Bun-only daemon → a raw `ReferenceError: Bun is
// not defined` in a `Restart=always` hot loop. serve() preflights the runtime and
// fails fast: exit 78 (EX_CONFIG), a one-line fix on stderr, and — proven by the
// absence of FLEETD_LOADED — WITHOUT importing the daemon bundle or binding a port.
//
// Resolve a Node binary to exercise the guard: under `node --test` this process IS
// Node; under `bun test` we spawn `node` off PATH (CI is Bun-only, but the GitHub
// runner images ship a node, so the guard is still exercised there). If no Node is
// available (a genuinely Bun-only box) the guard cannot be observed here, so the
// test skips rather than failing on the environment.
function resolveNode(): string | null {
  const candidate = process.versions.bun ? 'node' : process.execPath;
  const probe = spawnSync(candidate, ['--version'], { encoding: 'utf8', timeout: 15000 });
  if (probe.error || probe.status !== 0 || !/^v\d+\./.test((probe.stdout ?? '').trim()))
    return null;
  return candidate;
}

// Install-integrity guard (the Coder post-Bun-cutover landmine). A partial or
// interrupted `npm i -g fleetdeck` — or an in-place upgrade over a daemon still
// running out of the global dir — can land bin/ WITHOUT the daemon bundle
// (src/daemon/fleetd.bundle.mjs). Before the guard, serve() fell through to
// `import('.../fleetd.ts')` and emitted a cryptic `Cannot find module` naming an
// unshippable SOURCE path (`.../src/daemon/takeover.ts`), attributed to
// bin/fleetdeck.mjs — baffling to an operator. serve() now checks the entrypoint
// exists and, when it does not, names the real problem and exits 66 (EX_NOINPUT)
// WITHOUT importing anything — proven by the absence of FLEETD_LOADED. Like the
// wrong-runtime guard, this runs under Bun (spawn via the shebang), so the bundle
// check is reached only after the runtime preflight passes.
function packCliWithoutBundle(prefix: string) {
  const bin = path.join(prefix, 'bin');
  fs.mkdirSync(bin, { recursive: true });
  const cli = path.join(bin, 'fleetdeck.mjs');
  fs.copyFileSync(path.join(REPO, 'bin', 'fleetdeck.mjs'), cli);
  fs.chmodSync(cli, 0o755); // the shebang can only launch it if it stays executable
  // Deliberately DO NOT write src/daemon/fleetd.bundle.mjs (nor the source
  // fallback): this is exactly the bundle-less tree a broken install leaves.
  return cli;
}

test('serve: an install missing the daemon bundle exits 66 with a reinstall hint, importing nothing', () => {
  const cli = packCliWithoutBundle(path.join(TMP, 'incomplete-install'));
  const res = spawnSync(cli, ['serve'], { encoding: 'utf8', timeout: 15000 });
  assert.equal(res.error, undefined, `child must not fail to spawn: ${res.error}`);
  assert.equal(
    res.status,
    66,
    `serve with no daemon bundle must exit 66 (EX_NOINPUT), got ${res.status}\nstderr: ${res.stderr}`,
  );
  assert.match(res.stderr, /install is incomplete/, 'the guard names the real problem');
  assert.match(res.stderr, /npm install -g fleetdeck/, 'the guard points at the reinstall fix');
  assert.ok(
    !res.stdout.includes('FLEETD_LOADED'),
    'nothing is imported when the daemon entrypoint is missing',
  );
});

test('serve: refuses to boot under a non-Bun runtime and never imports the daemon', (t) => {
  const node = resolveNode();
  if (!node) return t.skip('no node binary available to exercise the wrong-runtime guard');
  const cli = packFakeRuntime(path.join(TMP, 'wrong-runtime-node'));
  const res = spawnSync(node, [cli, 'serve'], { encoding: 'utf8', timeout: 15000 });
  assert.equal(res.error, undefined, `node must launch the CLI: ${res.error}`);
  assert.equal(
    res.status,
    78,
    `serve under Node must exit 78 (EX_CONFIG), got ${res.status}\nstderr: ${res.stderr}`,
  );
  assert.match(res.stderr, /requires Bun/, 'the guard must name the Bun requirement');
  assert.match(res.stderr, /service install/, 'the guard must point at the one-line fix');
  assert.ok(
    !res.stdout.includes('FLEETD_LOADED'),
    'the daemon bundle must NOT be imported when the runtime is wrong',
  );
});

// Self-contained-bundle guard (the Coder incomplete-install landmine, ROOT CAUSE).
// bin/fleetdeck.ts loads the daemon's pid-identity helpers (pidRecord /
// livePidLooksLikeFleetd / verifyDaemonPid) from src/daemon/takeover.ts. That
// import MUST be STATIC so `bun run bundle:bin` esbuild-INLINES the helpers into
// the shipped bin/fleetdeck.mjs. A published (bundle-only) install ships no src/
// tree (see the `files` allowlist), so ANY runtime load of that source — the
// former computed-path require()/import(), or a static import esbuild failed to
// inline — crashed `fleetdeck service start`/`stop` (which reach the identity
// gates) with `Cannot find module .../src/daemon/takeover.ts`. That is the cryptic
// error the exit-66 bundle guard above did NOT cover: it fires on a bundle-COMPLETE
// install too. This tripwire reads the committed artifact and asserts the helpers
// are present as INLINED function bodies and that no executable line still refers
// to the takeover source. Paired with the bin-drift CI gate (which pins the
// committed .mjs to bin/*.ts), it guarantees the shipped CLI is self-contained.
test('bin bundle inlines the takeover pid helpers and never loads the source at runtime', () => {
  const src = fs.readFileSync(path.join(REPO, 'bin', 'fleetdeck.mjs'), 'utf8');
  // esbuild marks each inlined module with a bare `// <path>` provenance comment;
  // drop pure-comment lines so only executable code is inspected for the source ref.
  const code = src
    .split('\n')
    .filter((line) => !/^\s*\/\//.test(line))
    .join('\n');
  assert.ok(
    !/takeover/i.test(code),
    'the shipped bin bundle must not reference the takeover source at runtime — the ' +
      'pid helpers must be esbuild-inlined via a STATIC import, not loaded by computed ' +
      'path (a bundle-only install ships no src/ tree)',
  );
  // Positively prove the helpers were INLINED (not merely dropped): their function
  // bodies must appear verbatim in the artifact.
  for (const fn of [
    'function pidRecord',
    'function livePidLooksLikeFleetd',
    'function verifyDaemonPid',
  ]) {
    assert.ok(src.includes(fn), `expected ${fn} to be inlined into bin/fleetdeck.mjs`);
  }
});
