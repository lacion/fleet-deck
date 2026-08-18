// Test-only seed for hooks exercised without the SessionStart that precedes
// them in Claude Code. Production creates the same lease in
// scripts/fleet-sessionstart; keeping this helper explicit prevents unit tests
// from weakening the runtime's missing-verdict-is-inactive contract.

import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import compatibility from '../../compatibility.json' with { type: 'json' };
import packageJson from '../../package.json' with { type: 'json' };

const VERSION = '2.1.234';
const CLAUDE_PID = String(process.pid);
const LIFETIME_MS = 30 * 24 * 3600_000;

export function seedHookCompatibility(home: string): Record<string, string> {
  const now = Date.now();
  mkdirSync(home, { recursive: true });
  writeFileSync(
    path.join(home, `claude-compat-${CLAUDE_PID}.json`),
    `${JSON.stringify({
      schema: 1,
      identity: { key: Number(CLAUDE_PID), source: 'CLAUDE_PID' },
      generation: { kind: 'test', value: `${CLAUDE_PID}:${VERSION}` },
      fleetdeckVersion: packageJson.version,
      policy: `${compatibility.schema}:${compatibility.claudeCode.min}:${compatibility.claudeCode.max}`,
      claudeVersion: VERSION,
      active: true,
      createdAt: now,
      expiresAt: now + LIFETIME_MS,
    })}\n`,
    { mode: 0o600 },
  );
  return { CLAUDE_PID, FLEETDECK_TEST_CLAUDE_VERSION: VERSION };
}
