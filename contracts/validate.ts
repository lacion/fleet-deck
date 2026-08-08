// Tiny, dependency-free runtime guards shared by the two HOSTILE-boundary
// validators (hook intake, spawn intake). Static types describe what our code
// EXPECTS; these guards check what the wire actually SENT. F1a's second
// non-negotiable: "static types don't validate wire input."
//
// Pure module — only `typeof`, `Array.isArray`, and plain comparisons — so it
// is safe in the browser (board), Node (daemon source), Bun, and inside both
// bundles. No `enum`/`namespace`/param-properties (native strip-types won't
// transform them).

// Discriminated result every validator returns. Callers branch on `ok`; a
// failed validation carries a human-readable `error` for the fail-open log
// line and NEVER throws — the boundary decides what to do (reject + still 200).
export type ValidationResult<T> = { ok: true; value: T } | { ok: false; error: string };

export function ok<T>(value: T): ValidationResult<T> {
  return { ok: true, value };
}

export function fail<T>(error: string): ValidationResult<T> {
  return { ok: false, error };
}

// A JSON object (not null, not an array). Under `noPropertyAccessFromIndexSignature`
// callers must read keys off the result with bracket access (`v['session_id']`).
export function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

export function isNonEmptyString(v: unknown): v is string {
  return typeof v === 'string' && v.length > 0;
}

export function isBoolean(v: unknown): v is boolean {
  return typeof v === 'boolean';
}

// Finite number only — rejects NaN/Infinity, which JSON.parse never produces
// but a hand-crafted body can smuggle in as a string coercion target.
export function isFiniteNumber(v: unknown): v is number {
  return typeof v === 'number' && Number.isFinite(v);
}

// A positive integer (spawn's `plan_id`). Rejects 0, negatives, and non-integers.
export function isPositiveInt(v: unknown): v is number {
  return isFiniteNumber(v) && Number.isInteger(v) && v > 0;
}
