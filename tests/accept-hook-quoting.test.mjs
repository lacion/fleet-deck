// tests/accept-hook-quoting.test.mjs
//
// BUG-092: the demo acceptance scripts generate .claude/settings.json with a
// command hook whose script path was interpolated unquoted
// (`"command": "node $SESSIONSTART_SCRIPT"`). In a checkout under a path with
// spaces ("/Users/me/Fleet Deck") the hook shell splits the path into
// multiple arguments and SessionStart/Stop can never execute — every billed
// acceptance gate fails. Fix: quote the path inside the JSON string
// (`node \"$SESSIONSTART_SCRIPT\"`), the shape run-smoke.sh already uses.
//
// The test re-runs each script's actual `cat > settings.json <<EOF` heredoc
// (extracted by line block) with a space-containing FLEETDECK_ROOT, parses
// the generated JSON, and asserts every command hook resolves its script to
// a real file when executed under `sh -c` with cwd elsewhere.

import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

// [script, line of `cat > ... <<EOF`, line of the closing `EOF`]
const HEREDOCS = [
  ['demo/run-accept-phase3.sh'],
  ['demo/run-accept-plan.sh'],
  ['demo/run-accept-spawn.sh'],
];

function heredocBlock(file) {
  const lines = readFileSync(path.join(REPO_ROOT, file), 'utf8').split('\n');
  const start = lines.findIndex(l => l.includes('cat > "$PROJECT_DIR/.claude/settings.json" <<EOF'));
  assert.notEqual(start, -1, `${file}: settings.json heredoc not found`);
  const end = lines.indexOf('EOF', start);
  assert.notEqual(end, -1, `${file}: heredoc terminator not found`);
  return lines.slice(start, end + 1).join('\n');
}

function hookCommands(settings) {
  const cmds = [];
  for (const entries of Object.values(settings.hooks)) {
    for (const entry of entries) {
      for (const h of (entry.hooks || [])) if (h.command) cmds.push(h.command);
    }
  }
  return cmds;
}

for (const [script] of HEREDOCS) {
  test(`${script}: hook commands survive a checkout path with spaces (BUG-092)`, (t) => {
    const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-hook-quoting-'));
    t.after(() => rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

    // A checkout root containing a space, with stand-in scripts to execute.
    const spacedRoot = path.join(scratch, 'Fleet Deck');
    mkdirSync(path.join(spacedRoot, 'scripts'), { recursive: true });
    for (const name of ['fleet-sessionstart.mjs', 'fleet-watch.mjs']) {
      writeFileSync(path.join(spacedRoot, 'scripts', name), 'process.exit(0);\n');
    }

    // Render the script's own heredoc verbatim, redirected at a temp file.
    const outFile = path.join(scratch, 'settings.json');
    const render = [
      'set -u',
      `FLEETDECK_ROOT=${JSON.stringify(spacedRoot)}`,
      'SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"',
      'WATCH_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-watch.mjs"',
      'BASE="http://127.0.0.1:4711"',
      heredocBlock(script).replace('"$PROJECT_DIR/.claude/settings.json"', JSON.stringify(outFile)),
    ].join('\n');
    execFileSync('bash', ['-c', render]);

    const settings = JSON.parse(readFileSync(outFile, 'utf8'));
    const cmds = hookCommands(settings);
    assert.ok(cmds.length > 0, `${script}: expected at least one command hook`);

    for (const cmd of cmds) {
      // Run the generated command exactly as the hook shell would, from an
      // unrelated cwd: an unquoted path makes node try to load a nonexistent
      // prefix of the real script path.
      assert.doesNotThrow(
        () => execFileSync('sh', ['-c', cmd], { cwd: scratch, stdio: 'pipe' }),
        `${script}: generated hook command must execute with a spaced path — got: ${cmd}`,
      );
    }
  });
}
