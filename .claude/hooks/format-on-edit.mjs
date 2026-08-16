#!/usr/bin/env node
// PostToolUse dev hook (project-local, NOT the shipped plugin hooks).
//
// After Claude edits a TypeScript file, run Biome (format + safe fixes) on just
// that file. Scoped to TS extensions so the generated .mjs surface is never
// touched. Runs the repo-local binary via the current Node — the .bin/biome shim
// is a node launcher, so no dependency on bun/npx being on PATH. Always exits 0:
// formatting must NEVER block an edit.
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
const localBin = (name) => path.join(root, 'node_modules', '.bin', name);

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
