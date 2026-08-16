// BUG-096: run-accept-phase3.sh must resolve the transcript directory with the
// SAME slash-and-dot cwd munging the daemon uses (helpers.mjs), not the old
// sed 's|/|-|g' that left dots in place — a checkout path containing a dot
// would make the gate search a directory Claude never writes.
import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = readFileSync(path.join(HERE, '..', 'demo', 'run-accept-phase3.sh'), 'utf8');

test('phase-3 transcript lookup uses the production mungeClaudeProjectCwd helper (BUG-096)', () => {
  assert.match(
    SCRIPT,
    /mungeClaudeProjectCwd/,
    'run-accept-phase3.sh must derive the transcript dir via src/daemon/helpers.mjs mungeClaudeProjectCwd',
  );
  assert.doesNotMatch(
    SCRIPT,
    /TRANSCRIPT_DIR=.*sed 's\|\/\|-\|g'/,
    'the old slash-only sed munging must be gone — it leaves dots unconverted',
  );
});

test('the production helper converts both slashes and dots (the defect trigger)', async () => {
  const { mungeClaudeProjectCwd } = await import('../src/daemon/helpers.ts');
  assert.equal(mungeClaudeProjectCwd('/tmp/fleet.deck/proj'), '-tmp-fleet-deck-proj');
});
