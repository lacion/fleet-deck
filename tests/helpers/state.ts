// state.ts — helpers shared by the /state-driven integration tests. Two concerns
// live here, both about a session's presence in the daemon snapshot: scratchCwd()
// mints the throwaway working directory a session registers under, and
// questionsFor / findSession / getSession read a fetched /state snapshot.
//
// The snapshot type diverges per test file (some import contracts/state.ts, some
// keep a minimal local interface, plans makes sessions?/questions? optional), so
// every reader is GENERIC over the row type and constrains only the fields it
// touches. That is why there is no shared StateResponse here — the callers keep
// their own, and inference binds the row type from the `state` argument.

import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

// A fresh empty temp dir to register a session's cwd under. The prefix is leak
// forensics: a stray fleetdeck-*cwd- dir names the suite that failed to clean up
// (this repo has had a >13k-orphan tmpdir incident), so a suite that wants its
// own tag passes it — plans uses 'fleetdeck-plans-cwd-'.
export function scratchCwd(prefix = 'fleetdeck-cwd-'): string {
  return mkdtempSync(path.join(tmpdir(), prefix));
}

// The questions in a /state snapshot for one session, optionally narrowed to a
// kind. The `?? []` tolerates a snapshot that omits `questions` entirely; the
// three plain-variant callers had a non-optional field and so TypeError'd on
// absence instead. That is a dead branch today — the contract declares
// `questions` required — and both shapes fail the same assertions, so this
// changes only the failure MODE (a cleaner empty-filter assertion vs a throw),
// never a passing outcome.
export function questionsFor<Q extends { session_id: string; kind: string }>(
  state: { questions?: Q[] },
  sid: string,
  kind?: string,
): Q[] {
  return (state.questions ?? []).filter((q) => q.session_id === sid && (!kind || q.kind === kind));
}

// The session row for sid, or undefined (plans' non-asserting variant). Callers
// that need the row present use getSession.
export function findSession<S extends { session_id: string }>(
  state: { sessions?: S[] },
  sid: string,
): S | undefined {
  return (state.sessions ?? []).find((s) => s.session_id === sid);
}

// The session row for sid, asserting it is present so the caller reaches its
// fields directly (needs-you's variant — the assert lives here once instead of
// at every call site).
export function getSession<S extends { session_id: string }>(
  state: { sessions?: S[] },
  sid: string,
): S {
  const s = findSession<S>(state, sid);
  assert.ok(s, `session ${sid} should be present in /state`);
  return s;
}
