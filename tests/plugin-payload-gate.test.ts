// tests/plugin-payload-gate.test.ts
//
// BUG-001 — the release-bound plugin gate (the hook-integrity CI job) used to
// watch only seven hook-side paths, omitting the daemon bundle, most daemon
// sources, and the board — while the marketplace plugin cache keys on the
// plugin.json version. The reproduction from the audit: change
// scripts/fleetd/http.mjs, rebuild fleetd.bundle.mjs, keep every version
// manifest at the same string, and every gate passed — so existing installs
// kept the old cached payload while new installs got different behavior under
// the same semantic version.
//
// The fix moved the watched closure into one checked-in verifier,
// scripts/check-plugin-payload.mjs, which covers the complete
// behavior-bearing payload (hooks, bin/, ALL of scripts/fleetd/ including the
// bundle and board-dist, board/ sources, .claude-plugin/) and requires a
// version-manifest change in the same range. These tests run that verifier
// against scratch git repos, exactly as CI does.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const CHECK = path.join(REPO_ROOT, 'scripts', 'check-plugin-payload.mjs');

const PAYLOAD_FILES = [
  '.claude-plugin/plugin.json',
  'bin/fleetdeck.mjs',
  'board/src/App.jsx',
  'board/bun.lock',
  'hooks/hooks.json',
  'scripts/fleet-hook.mjs',
  'scripts/fleet-sessionstart.mjs',
  'scripts/fleet-watch.mjs',
  'scripts/fleetd/fleetd.mjs',
  'scripts/fleetd/http.mjs',
  'scripts/fleetd/fleetd.bundle.mjs',
  'scripts/fleetd/board-dist/index.html',
];

const VERSION_MANIFEST = 'package.json';

interface PayloadRepo {
  dir: string;
  base: string;
  commit: (rel: string, content: string) => void;
  cleanup: () => void;
}

interface GateResult {
  status: number | null;
  output: string;
}

function git(args: string[], cwd: string): string {
  return execFileSync('git', args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

function write(dir: string, rel: string, content: string): void {
  const abs = path.join(dir, rel);
  mkdirSync(path.dirname(abs), { recursive: true });
  writeFileSync(abs, content);
}

/**
 * A scratch repo containing the payload closure plus a version manifest, with
 * the initial state committed. Returns paths and a commit(rel, content)
 * helper; base is the commit the verifier diffs against.
 */
function makePayloadRepo(): PayloadRepo {
  const dir = mkdtempSync(path.join(tmpdir(), 'fleetdeck-payload-gate-'));
  git(['init', '-q'], dir);
  git(['config', 'user.email', 'test@fleetdeck.local'], dir);
  git(['config', 'user.name', 'Fleet Deck Tests'], dir);
  for (const rel of PAYLOAD_FILES) write(dir, rel, `// ${rel} v1\n`);
  write(dir, VERSION_MANIFEST, JSON.stringify({ name: 'fleetdeck', version: '0.0.1' }, null, 2));
  write(dir, 'docs/README.md', '# docs are not payload\n');
  git(['add', '.'], dir);
  git(['commit', '-q', '-m', 'seed'], dir);
  const base = git(['rev-parse', 'HEAD'], dir);
  return {
    dir,
    base,
    commit(rel: string, content: string) {
      write(dir, rel, content);
      git(['add', rel], dir);
      git(['commit', '-q', '-m', `change ${rel}`], dir);
    },
    cleanup() {
      rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    },
  };
}

function runGate(base: string, cwd: string): GateResult {
  try {
    const stdout = execFileSync(process.execPath, [CHECK, base], { cwd, encoding: 'utf8' });
    return { status: 0, output: stdout };
  } catch (err) {
    const e = err as { status?: number | null; stdout?: string; stderr?: string };
    return { status: e.status ?? null, output: `${e.stdout ?? ''}${e.stderr ?? ''}` };
  }
}

// Every payload path the audit found unwatched must make the verifier demand
// a version bump. The hook-side paths were already watched pre-fix; the
// daemon/board rows are the BUG-001 regression assertions.
for (const rel of [
  'scripts/fleetd/http.mjs',
  'scripts/fleetd/fleetd.mjs',
  'scripts/fleetd/fleetd.bundle.mjs',
  'scripts/fleetd/board-dist/index.html',
  'board/src/App.jsx',
  'board/bun.lock',
  'bin/fleetdeck.mjs',
  'hooks/hooks.json',
  'scripts/fleet-hook.mjs',
  'scripts/fleet-sessionstart.mjs',
  'scripts/fleet-watch.mjs',
]) {
  test(`payload change without a version bump fails the gate: ${rel}`, (t: TestContext) => {
    const repo = makePayloadRepo();
    t.after(repo.cleanup);
    repo.commit(rel, `// changed ${rel}\n`);
    const res = runGate(repo.base, repo.dir);
    assert.equal(res.status, 1, `expected exit 1, got ${String(res.status)}: ${res.output}`);
    assert.match(res.output, /no version manifest/);
  });
}

test('version bump alone is a release, not a payload change — gate stays green', (t: TestContext) => {
  const repo = makePayloadRepo();
  t.after(repo.cleanup);
  repo.commit(VERSION_MANIFEST, JSON.stringify({ name: 'fleetdeck', version: '0.0.2' }, null, 2));
  repo.commit(
    '.claude-plugin/plugin.json',
    JSON.stringify({ name: 'fleetdeck', version: '0.0.2' }, null, 2),
  );
  const res = runGate(repo.base, repo.dir);
  assert.equal(res.status, 0, `expected exit 0, got ${String(res.status)}: ${res.output}`);
  // plugin.json lives inside the payload closure, so bumping it shows up as a
  // payload change — but it rides its own version bump, so the gate is green.
  assert.match(res.output, /release-bound/);
});

test('payload change WITH a version bump passes the gate', (t: TestContext) => {
  const repo = makePayloadRepo();
  t.after(repo.cleanup);
  repo.commit('scripts/fleetd/http.mjs', '// changed http.mjs\n');
  repo.commit(VERSION_MANIFEST, JSON.stringify({ name: 'fleetdeck', version: '0.0.2' }, null, 2));
  const res = runGate(repo.base, repo.dir);
  assert.equal(res.status, 0, `expected exit 0, got ${String(res.status)}: ${res.output}`);
  assert.match(res.output, /release-bound/);
});

test('changes outside the payload closure pass without a version bump', (t: TestContext) => {
  const repo = makePayloadRepo();
  t.after(repo.cleanup);
  repo.commit('docs/README.md', '# docs changed\n');
  const res = runGate(repo.base, repo.dir);
  assert.equal(res.status, 0, `expected exit 0, got ${String(res.status)}: ${res.output}`);
  assert.match(res.output, /untouched/);
});
