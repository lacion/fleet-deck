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
  assert.equal(res.error, undefined, `Bun still launches; only entrypoint resolution fails: ${res.error}`);
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
