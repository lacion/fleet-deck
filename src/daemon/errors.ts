// errors.ts — the catch-clause error readers shared across the fleetd core.
//
// Under `useUnknownInCatchVariables` a `catch` binding is `unknown`, and Node's
// errno errors carry a string `code`/`errno`/`status` the base Error type does
// not declare. Every module used to keep its own private copy of these readers
// (errno-code narrowing, the `code || message || fallback` phrasing, the status
// tag, the message coercion) because they predate any shared home. They are
// pure and import nothing, so they live here and can be imported from anywhere —
// including run-nonce.ts / takeover.ts, which the hook bundle inlines.
//
// One deliberate unification: `errCode` uses a STRUCTURAL read (`typeof err ===
// 'object' && typeof err.code === 'string'`) rather than `instanceof Error`.
// It is identical for every real Error, and it also reads `.code` off a thrown
// plain object — which none of the daemon's throw sources (fs / process / tmux /
// child_process / crypto) can produce, so that extra reach is never exercised.
// The three per-site `errnoCode` copies (instanceof + string-narrow) are a
// strict subset and fold in exactly. The old fleetd/spawn `errCode` copies had
// NO string-narrow, so versus those the structural read differs only for an
// Error carrying a non-string `.code` — immaterial at every call site, which
// only ever compares the result against a string errno literal.

// The errno code, for the ENOENT/EEXIST/ESRCH control-flow branches that read
// `err?.code === '…'`. `undefined` for a non-object or a code-less error, so the
// comparisons behave exactly as the untyped optional-chain reads did.
export function errCode(err: unknown): string | undefined {
  if (typeof err === 'object' && err !== null) {
    const code = (err as Record<string, unknown>)['code'];
    if (typeof code === 'string') return code;
  }
  return undefined;
}

// The old `err?.code || err?.message || 'unknown error'` idiom the .mjs used
// throughout, preserved as one helper so the phrasing can never drift. The tail
// is `fallback ?? String(err)`: pass a fixed string (the startup/confidentiality
// refusals want 'unknown error') or omit it to fall through to `String(err)`
// (the tmux-generation wrap sites in spawn.ts want the dynamic coercion).
export function errText(err: unknown, fallback?: string): string {
  const code = errCode(err);
  if (code) return code;
  if (typeof err === 'object' && err !== null) {
    const message = (err as Record<string, unknown>)['message'];
    if (typeof message === 'string' && message) return message;
  }
  return fallback ?? String(err);
}

// A caught value read back as a message string — the `err.message || String(err)`
// intent, unchanged.
export function errMessage(err: unknown): string {
  return err instanceof Error && err.message ? err.message : String(err);
}

// The numeric `.status` tag off a namedError (RepoError / SettingError), read
// without assuming a shape — a thrown non-Error is still possible from deep in a
// driver. `undefined` when there is no numeric status.
export function errStatus(err: unknown): number | undefined {
  if (typeof err === 'object' && err !== null && 'status' in err) {
    const status = (err as { status?: unknown }).status;
    if (typeof status === 'number') return status;
  }
  return undefined;
}
