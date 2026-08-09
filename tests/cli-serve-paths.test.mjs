// tests/cli-serve-paths.test.mjs
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

import test, { after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'fleetdeck-serve-paths-'));

after(() => { try { fs.rmSync(TMP, { recursive: true, force: true }); } catch { /* best effort */ } });

// Lay out the shape `serve` resolves against: the built, self-contained
// bin/fleetdeck.mjs artifact (esbuild has inlined tmux-version into it, so no
// sibling source file is needed) and scripts/fleetd/fleetd.bundle.mjs, which
// takes precedence over the source fallback exactly as in a packed install.
function packFakeRuntime(prefix) {
  const bin = path.join(prefix, 'bin');
  const fleetdDir = path.join(prefix, 'scripts', 'fleetd');
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(fleetdDir, { recursive: true });
  fs.copyFileSync(path.join(REPO, 'bin', 'fleetdeck.mjs'), path.join(bin, 'fleetdeck.mjs'));
  fs.writeFileSync(path.join(fleetdDir, 'fleetd.bundle.mjs'), `process.stdout.write('FLEETD_LOADED\\\\n');\n`);
  return path.join(bin, 'fleetdeck.mjs');
}

function serveLoads(prefix) {
  const cli = packFakeRuntime(prefix);
  const res = spawnSync(process.execPath, [cli, 'serve'], { encoding: 'utf8', timeout: 15000 });
  assert.equal(res.error, undefined, `child must not fail to spawn: ${res.error}`);
  assert.equal(res.status, 0, `serve must exit cleanly under ${JSON.stringify(prefix)}\nstderr: ${res.stderr}`);
  assert.ok(res.stdout.includes('FLEETD_LOADED'), 'the daemon bundle was actually imported');
}

// Each legal-but-hostile prefix is exercised on its own so a failure names the
// character class that broke. `%zz` is a raw percent followed by a non-hex
// sequence — decodeURIComponent throws URIError on it.
for (const [label, dirname] of [
  ['a fragment delimiter (#)', 'install#fragment'],
  ['a query delimiter (?)', 'install?query'],
  ['a raw percent sequence (%)', 'install%zz'],
  ['spaces', 'install dir with spaces'],
  ['all of them at once', 'fleet deck #1?x=%zz'],
]) {
  test(`serve: loads the daemon from an install path containing ${label}`, () => {
    serveLoads(path.join(TMP, dirname));
  });
}
