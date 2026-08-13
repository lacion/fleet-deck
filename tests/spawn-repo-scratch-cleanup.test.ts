// Regression for BUG-212: the failed-clone case in tests/spawn-repo.test.ts
// creates its missing origin under a scratch parent named
// `fleetdeck-missing-origin-*`. Without teardown owning that parent, every run
// of the focused test left one directory behind in tmpdir. Running the single
// test file with a private TMPDIR makes the leak observable: before the fix
// exactly one orphan survives; after the fix none do.
import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { childRunArgv, childPassCount } from './helpers/child-runner.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));

test(
  'the failed-clone test cleans up its missing-origin scratch parent (BUG-212)',
  { timeout: 180_000 },
  (t) => {
    const tmpRoot = mkdtempSync(path.join(tmpdir(), 'fd-bug212-'));
    t.after(() => {
      rmSync(tmpRoot, { recursive: true, force: true });
    });
    // The outer `node --test` sets NODE_TEST_CONTEXT=child-v8; if the inner child
    // inherits it, it runs in child-runner mode, executes nothing, and exits 0 —
    // a vacuous pass. Strip it so the inner run is a real one. Object-spreading
    // process.env drops its index signature (bun-types closes ProcessEnv), so the
    // literal is annotated NodeJS.ProcessEnv to keep bracket delete/read legal.
    const env: NodeJS.ProcessEnv = { ...process.env, TMPDIR: tmpRoot };
    delete env['NODE_TEST_CONTEXT'];
    const argv = childRunArgv({
      file: path.join(HERE, 'spawn-repo.test.ts'),
      namePattern: 'clone failure tombstones',
    });
    const result = spawnSync(process.execPath, argv, {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
      timeout: 120_000,
    });
    // node prints `# pass N` on stdout, bun ` N pass` on stderr — parse combined.
    const out = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    assert.equal(
      childPassCount(out),
      1,
      `the inner run must actually execute (and pass) exactly the focused test:\n${out}`,
    );
    const orphans = readdirSync(tmpRoot).filter((name) =>
      name.startsWith('fleetdeck-missing-origin-'),
    );
    assert.deepEqual(orphans, [], `missing-origin scratch parents leaked: ${orphans.join(', ')}`);
  },
);
