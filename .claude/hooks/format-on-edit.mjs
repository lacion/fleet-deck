#!/usr/bin/env node
// PostToolUse dev hook of this repository (Claude Code only; Fleet Deck ships no hooks).
//
// After Claude edits a TypeScript file, run Biome (format + safe fixes) on just
// that file. Biome comes from the board's dependencies (`web/node_modules`) and
// uses the root `biome.jsonc`. It runs the .bin/biome shim with the current Node,
// so bun or npx need not be on PATH. Always exits 0: formatting must never block
// an edit.
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';

const TS_EXT = new Set(['.ts', '.tsx', '.mts', '.cts']);

const raw = await new Promise((resolve) => {
  let data = '';
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => (data += chunk));
  process.stdin.on('end', () => resolve(data));
  process.stdin.on('error', () => resolve(data));
});

let filePath;
try {
  filePath = JSON.parse(raw)?.tool_input?.file_path;
} catch {
  /* malformed payload — nothing to format */
}

if (!filePath || !TS_EXT.has(path.extname(filePath))) process.exit(0);

const root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
const localBin = (name) => path.join(root, 'web', 'node_modules', '.bin', name);

// `biome check --write` formats and applies safe lint fixes in one pass —
// the Biome equivalent of the old prettier --write + eslint --fix sequence.
const bin = localBin('biome');
if (existsSync(bin)) {
  try {
    // Run the resolved script under this Node so we don't rely on shebang/PATH.
    execFileSync(process.execPath, [bin, 'check', '--write', filePath], {
      cwd: root,
      stdio: 'ignore',
    });
  } catch {
    /* lint/format failures are advisory here — never block the edit */
  }
}

process.exit(0);
