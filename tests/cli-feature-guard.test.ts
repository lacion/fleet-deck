// tests/cli-feature-guard.test.ts
//
// Version-bump guard. Fleet Deck leans on
// two UNDOCUMENTED Claude Code features, both validated live on 2.1.206:
//
//   - `asyncRewake` (+ @internal rewakeMessage/rewakeSummary) on the Stop
//     command hook — powers scripts/fleet-watch.mjs (F3d-2). The binary
//     schema self-describes these fields as @internal with no compat
//     promise, so any CLI upgrade may silently drop them.
//   - `AskUserQuestion` as a hookable tool (PreToolUse/PermissionRequest) —
//     powers the F3c choice relay.
//
// This test greps the LOCAL claude binary (readlink -f $(which claude)) for
// both strings. It is a canary, not a behavior test: a hit does not prove
// the feature still works, but a MISS on a version bump means the relay is
// running blind and the live validation must be redone. Skips cleanly when
// no claude binary is installed (CI without the CLI).

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { spawnSync, type SpawnSyncReturns } from 'node:child_process';

function resolveClaudeBinary(): string | null {
  const which = spawnSync('bash', ['-c', 'command -v claude'], { encoding: 'utf8' });
  const found = (which.stdout || '').trim();
  if (which.status !== 0 || !found) return null;
  const real = spawnSync('readlink', ['-f', found], { encoding: 'utf8' });
  return (real.status === 0 && real.stdout.trim()) || found;
}

test('version-bump guard: the local claude binary still ships asyncRewake and AskUserQuestion', (t) => {
  const bin = resolveClaudeBinary();
  if (!bin) {
    t.skip('no `claude` binary on PATH — nothing to guard here');
    return;
  }

  for (const feature of ['asyncRewake', 'AskUserQuestion']) {
    const grep: SpawnSyncReturns<Buffer> = spawnSync('grep', ['-qaF', feature, bin]);
    if (grep.status === 2 || grep.error) {
      t.skip(`could not grep ${bin}: ${grep.error?.message ?? 'grep error'}`);
      return;
    }
    assert.equal(
      grep.status,
      0,
      `'${feature}' is GONE from the claude binary at ${bin} — the CLI dropped an undocumented feature Fleet Deck relies on. ` +
        `Re-run the live validation before trusting the ${feature === 'asyncRewake' ? 'F3d-2 rewake watcher' : 'F3c question relay'} on this CLI version; ` +
        'turn-boundary mail delivery remains the safe fallback.',
    );
  }
});
