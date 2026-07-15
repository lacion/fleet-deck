// tests/helpers/wait.mjs
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

import { networkInterfaces } from 'node:os';
import { existsSync, readFileSync } from 'node:fs';

// Read once, clamped to a sane minimum of 1: a stray sub-1 value can only ever
// ADD headroom, never shrink an authored timeout below its written value (which
// would defeat the point and could turn a "prove nothing happens" wait into a
// false pass). Unset / 0 / NaN all collapse to 1 — identical to the historical
// `Number(env) || 1`.
export const WAIT_SCALE = Math.max(1, Number(process.env.FLEETDECK_TEST_WAIT_SCALE) || 1);

/** Scale a fixed timeout / settle-sleep value by WAIT_SCALE. */
export const scaleMs = (ms) => ms * WAIT_SCALE;

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
export async function waitUntil(predicate, { timeoutMs = 5000, intervalMs = 100, label = 'condition' } = {}) {
  const effectiveTimeoutMs = scaleMs(timeoutMs);
  const deadline = Date.now() + effectiveTimeoutMs;
  for (;;) {
    const result = await predicate();
    if (result) return result;
    if (Date.now() >= deadline) throw new Error(`waitUntil: ${label} not met within ${effectiveTimeoutMs}ms`);
    await new Promise(r => setTimeout(r, intervalMs));
  }
}

/**
 * Retry `fetch(url, options)` until it yields ANY response (returned) or the
 * scaled timeout elapses (throws). Superset of the lan-auth (options-carrying)
 * and ws-hardening (options-less) variants — both only ever pass the url. Each
 * attempt is bounded at 500ms; failures back off 100ms and retry.
 */
export async function waitForResponse(url, options = {}, timeoutMs = 10_000) {
  const deadline = Date.now() + scaleMs(timeoutMs);
  let lastError;
  while (Date.now() < deadline) {
    try {
      return await fetch(url, { ...options, signal: AbortSignal.timeout(500) });
    } catch (err) {
      lastError = err;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
  }
  throw new Error(`daemon never answered ${url}: ${lastError?.message || 'timeout'}`);
}

/** Non-internal IPv4 addresses of this host (empty in restricted sandboxes). */
export function nonInternalIpv4s() {
  const found = [];
  try {
    for (const entries of Object.values(networkInterfaces())) {
      for (const entry of entries || []) {
        if ((entry.family === 'IPv4' || entry.family === 4) && !entry.internal) found.push(entry.address);
      }
    }
  } catch { /* restricted sandboxes may deny interface enumeration */ }
  return found;
}

// Read a JSONL spec-capture file into an array of parsed records (private:
// the sole consumer is waitForSpecRecords). Shared verbatim by spawn /
// spawn-unsupervised.
function readSpecRecords(file) {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8').split('\n').filter(Boolean).map(line => {
    try { return JSON.parse(line); } catch { return null; }
  }).filter(Boolean);
}

/**
 * Wait until `file` holds at least `minCount` spec records, then return them.
 * Authored budget 8000ms (the spawn/spawn-unsupervised local-waitUntil
 * default); override via `opts`.
 */
export async function waitForSpecRecords(file, minCount, opts) {
  return waitUntil(() => {
    const recs = readSpecRecords(file);
    return recs.length >= minCount ? recs : null;
  }, { timeoutMs: 8000, label: `>= ${minCount} recorded spec(s) in ${file}`, ...opts });
}
