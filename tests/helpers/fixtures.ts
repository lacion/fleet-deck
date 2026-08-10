// tests/helpers/fixtures.ts — load synthesized hook payload fixtures.
//
// Fixtures are plain JSON with "__SESSION__" / "__CWD__" placeholder tokens
// (see tests/fixtures/*.json). loadFixture() replaces those tokens and then
// shallow-merges any extra overrides on top, so tests can do e.g.
//   loadFixture('post-tool-use-bash', { session_id: sid, cwd }, {
//     tool_input: { command: 'npm test' },
//   })

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const FIXTURES_DIR = path.resolve(HERE, '../fixtures');

function substituteTokens(value: unknown, tokens: Record<string, string>): unknown {
  if (typeof value === 'string') {
    let out = value;
    for (const [token, replacement] of Object.entries(tokens)) {
      out = out.split(token).join(replacement);
    }
    return out;
  }
  if (Array.isArray(value)) return value.map((v: unknown) => substituteTokens(v, tokens));
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) out[k] = substituteTokens(v, tokens);
    return out;
  }
  return value;
}

export interface FixtureTokens {
  session_id?: string;
  session?: string;
  cwd?: string;
}

/**
 * Load fixture `name` (basename without .json), substitute the __SESSION__ /
 * __CWD__ tokens from `tokens`, then shallow-merge `overrides` on top (nested
 * objects like tool_input are replaced wholesale, not deep-merged).
 */
export function loadFixture(
  name: string,
  tokens: FixtureTokens = {},
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  const raw = readFileSync(path.join(FIXTURES_DIR, `${name}.json`), 'utf8');
  const base: unknown = JSON.parse(raw);
  const substituted = substituteTokens(base, {
    __SESSION__: tokens.session_id ?? tokens.session ?? '',
    __CWD__: tokens.cwd ?? '',
  });
  return { ...(substituted as Record<string, unknown>), ...overrides };
}
