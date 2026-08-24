// tests/helpers/wait.ts
//
// Centralised test-wait plumbing. The suite used to carry ~10 near-identical
// `waitUntil` helpers (three different signatures) plus a scatter of FIXED
// timeouts that ignored FLEETDECK_TEST_WAIT_SCALE — so the macOS advisory CI
// lane, which runs with WAIT_SCALE=3, gave those waits ZERO headroom and
// flaked (issue #2). Everything that waits now routes through this module, so
// every timeout scales together.
//
// The knob is read ONCE, here. Test files import WAIT_SCALE / scaleMs when they
// need to scale a bespoke timeout, and waitUntil / waitForResponse /
// waitForSpecRecords for the common polling shapes.

import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { networkInterfaces, tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';

// Read once, clamped to a sane minimum of 1: a stray sub-1 value can only ever
// ADD headroom, never shrink an authored timeout below its written value (which
// would defeat the point and could turn a "prove nothing happens" wait into a
// false pass). Unset / 0 / NaN all collapse to 1 — identical to the historical
// `Number(env) || 1`.
export const WAIT_SCALE = Math.max(1, Number(process.env['FLEETDECK_TEST_WAIT_SCALE']) || 1);

/** Scale a fixed timeout / settle-sleep value by WAIT_SCALE. */
export const scaleMs = (ms: number): number => ms * WAIT_SCALE;

export interface WaitUntilOptions {
  timeoutMs?: number;
  intervalMs?: number;
  label?: string;
}

/**
 * Poll `predicate` until it returns a truthy value (which is returned) or the
 * scaled deadline elapses (which throws). Unified superset of the three
 * historical variants:
 *   - options-object signature (timeoutMs / intervalMs / label);
 *   - async OR sync predicate (awaited either way);
 *   - returns the truthy result (callers that ignored it are unaffected).
 * `timeoutMs` is the AUTHORED budget; the effective deadline is
 * timeoutMs * WAIT_SCALE.
 */
export async function waitUntil<T>(
  predicate: () => T | Promise<T>,
  { timeoutMs = 5000, intervalMs = 100, label = 'condition' }: WaitUntilOptions = {},
): Promise<NonNullable<Awaited<T>>> {
  const effectiveTimeoutMs = scaleMs(timeoutMs);
  const deadline = Date.now() + effectiveTimeoutMs;
  for (;;) {
    const result = await predicate();
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- load-bearing: predicates legitimately return null/false to mean "keep polling"; the rule mis-reads the unconstrained generic as always-truthy
    if (result) return result;
    if (Date.now() >= deadline)
      throw new Error(`waitUntil: ${label} not met within ${effectiveTimeoutMs}ms`);
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

/**
 * Retry `fetch(url, options)` until it yields ANY response (returned) or the
 * scaled timeout elapses (throws). Superset of the lan-auth (options-carrying)
 * and ws-hardening (options-less) variants — both only ever pass the url. Each
 * attempt is bounded at 500ms; failures back off 100ms and retry.
 */
export async function waitForResponse(
  url: string,
  options: RequestInit = {},
  timeoutMs = 10_000,
): Promise<Response> {
  const deadline = Date.now() + scaleMs(timeoutMs);
  let lastError: unknown;
  while (Date.now() < deadline) {
    try {
      return await fetch(url, { ...options, signal: AbortSignal.timeout(500) });
    } catch (err) {
      lastError = err;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  throw new Error(
    `daemon never answered ${url}: ${lastError instanceof Error ? lastError.message : 'timeout'}`,
  );
}

/** Non-internal IPv4 addresses of this host (empty in restricted sandboxes). */
export function nonInternalIpv4s(): string[] {
  const found: string[] = [];
  try {
    for (const entries of Object.values(networkInterfaces())) {
      for (const entry of entries ?? []) {
        // @types/node types `family` as a string ('IPv4'); the historical
        // `|| entry.family === 4` numeric branch is unreachable under those
        // types and was dropped in the TS migration (see ts-migration-bugs).
        if (entry.family === 'IPv4' && !entry.internal) found.push(entry.address);
      }
    }
  } catch {
    /* restricted sandboxes may deny interface enumeration */
  }
  return found;
}

// Read a JSONL spec-capture file into an array of parsed records (private:
// the sole consumer is waitForSpecRecords). Shared verbatim by spawn /
// spawn-unsupervised.
function readSpecRecords(file: string): unknown[] {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      try {
        return JSON.parse(line) as unknown;
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

/**
 * Wait until `file` holds at least `minCount` spec records, then return them.
 * Authored budget 8000ms (the spawn/spawn-unsupervised local-waitUntil
 * default); override via `opts`.
 */
export async function waitForSpecRecords(
  file: string,
  minCount: number,
  opts?: WaitUntilOptions,
): Promise<unknown[]> {
  return waitUntil(
    () => {
      const recs = readSpecRecords(file);
      return recs.length >= minCount ? recs : null;
    },
    { timeoutMs: 8000, label: `>= ${minCount} recorded spec(s) in ${file}`, ...opts },
  );
}

/**
 * Parse an append-only JSONL capture file into its raw records, tolerating a
 * torn trailing line but never masking real corruption.
 *
 * The FLEETDECK_SPAWN_CMD fixture appends each launch with a single
 * `appendFileSync(JSON.stringify(rec) + '\n')`. A launch spec is multi-KB, and
 * Node's appendFileSync loops `writeSync` until every byte lands — so a reader
 * polling concurrently can observe the final record's bytes BEFORE its
 * terminating '\n'. That un-terminated tail is a normal, transient state of an
 * append-only log, not corruption. Two full-suite `test:bundle` runs caught it
 * as a `JSON.parse` "Unterminated string" in a per-file records() helper
 * (2026-08-24: first sighting during P9.2 slice-3 integration, second during
 * P10 slice-2) — see the adopt-jsonl-partial-read-flake note.
 *
 * Every '\n'-terminated line must be valid JSON: a parse error on one is real
 * corruption (a truncated file, interleaved concurrent writers) and is
 * re-thrown, never swallowed. Only the single un-terminated trailing line is
 * dropped; the caller's poll (waitForRecords) picks the record up on a later
 * read, once the newline has landed. Unlike readSpecRecords above — which
 * try/catch-drops ANY bad line for its multi-writer spawn callers — this reader
 * is deliberately strict about mid-file lines.
 */
export function readJsonlRecords(file: string): unknown[] {
  if (!existsSync(file)) return [];
  const lines = readFileSync(file, 'utf8').split('\n');
  // With `json + '\n'` appends, the final split element is '' when the file ends
  // on a newline (all records complete) and the torn partial line otherwise;
  // drop that one element either way. Every remaining line is newline-terminated,
  // so a JSON.parse throw below is genuine corruption, not an in-flight write.
  lines.pop();
  return lines.filter(Boolean).map((line) => JSON.parse(line) as unknown);
}

/**
 * Create a scratch spec-record file (mkdtemp'd dir + <name>, default
 * specs.jsonl) whose OWNING directory is removed at test teardown. The old
 * pattern — `path.join(scratchDir(), 'specs.jsonl')` kept only as a string —
 * lost the directory, so teardown could never remove it and every run leaked
 * its record dir into the OS temp tree. Pass the test context `t` so cleanup
 * is registered on `t.after`; the rm is registered BEFORE anything else the
 * caller adds (t.after callbacks run in reverse order of registration), so it
 * fires LAST — after the daemon has stopped and finished writing to the file.
 */
export function makeSpecRecordFile(
  t: TestContext,
  { prefix = 'fleetdeck-spawn-record-', name = 'specs.jsonl' } = {},
): string {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return path.join(dir, name);
}
