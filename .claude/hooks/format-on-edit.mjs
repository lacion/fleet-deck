#!/usr/bin/env node
// PostToolUse dev hook (project-local, NOT the shipped plugin hooks).
//
// After Claude edits a TypeScript file, run Prettier --write then ESLint --fix on
// just that file. Scoped to TS extensions so the legacy .mjs surface is never
// touched. Runs the repo-local binaries via the current Node — no dependency on
// bun/npx being on PATH. Always exits 0: formatting must NEVER block an edit.
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

for (const [name, args] of [
  ['prettier', ['--write', filePath]],
  ['eslint', ['--fix', filePath]],
]) {
  const bin = localBin(name);
  if (!existsSync(bin)) continue;
  try {
    // Run the resolved script under this Node so we don't rely on shebang/PATH.
    execFileSync(process.execPath, [bin, ...args], { cwd: root, stdio: 'ignore' });
  } catch {
    /* lint/format failures are advisory here — never block the edit */
  }
}

process.exit(0);
