// tests/demo-phase3-transcript.test.ts
//
// Regression for BUG-095: demo/run-accept-phase3.sh located the resumed
// session's transcript under "$HOME/.claude/projects" unconditionally.
// Claude Code stores sessions under ${CLAUDE_CONFIG_DIR:-$HOME/.claude}, so a
// contributor running with CLAUDE_CONFIG_DIR set had the gate searching the
// wrong tree and reporting a WORKING answer relay as missing.
//
// We don't run the acceptance script (it spends real Claude usage). Instead we
// extract the exact TRANSCRIPT_ROOT/TRANSCRIPT_DIR assignment block from the
// script and execute it under a controlled env — the test pins the script's
// real logic, not a copy of it.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
  'demo',
  'run-accept-phase3.sh',
);

// Pull the assignment block out of the script (TRANSCRIPT_ROOT through
// TRANSCRIPT_DIR) so the test executes the script's own lines verbatim.
function resolveTranscriptDir(env: Record<string, string>): string {
  const src = readFileSync(SCRIPT, 'utf8');
  const assignBlock = /^TRANSCRIPT_ROOT=.*\nTRANSCRIPT_DIR=.*$/m.exec(src)?.[0];
  assert.ok(assignBlock, 'transcript-root resolution block not found in run-accept-phase3.sh');
  const out = execFileSync('bash', ['-c', `${assignBlock}\nprintf '%s' "$TRANSCRIPT_DIR"`], {
    env: { PATH: process.env['PATH'], ...env },
    encoding: 'utf8',
  });
  return out;
}

test('BUG-095: transcript lookup honors CLAUDE_CONFIG_DIR when set', () => {
  const dir = resolveTranscriptDir({
    HOME: '/home/dev',
    CLAUDE_CONFIG_DIR: '/custom/claude-config',
    PROJECT_DIR: '/work/fleet-deck/demo/project',
  });
  assert.equal(dir, '/custom/claude-config/projects/-work-fleet-deck-demo-project');
});

test('BUG-095: transcript lookup falls back to $HOME/.claude when unset', () => {
  const dir = resolveTranscriptDir({
    HOME: '/home/dev',
    PROJECT_DIR: '/work/fleet-deck/demo/project',
  });
  assert.equal(dir, '/home/dev/.claude/projects/-work-fleet-deck-demo-project');
});
