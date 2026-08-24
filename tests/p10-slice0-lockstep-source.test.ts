// tests/p10-slice0-lockstep-source.test.ts
//
// P10 SLICE 0 — source-invariant characterization pins (deliverable 3 + the
// watch-ceiling half of 1E.2). These are SOURCE pins: they read constants
// straight out of their files and fail LOUDLY, with file:line, the moment a
// future edit moves any of them. They add ZERO wall time and touch no daemon.
//
// D1 LOCKSTEP (design §4-D1): the three-tier hold deadline chain must stay
// strictly ordered so a board answer never lands on a socket that a lower tier
// already failed open:
//
//     daemon hold ceiling      650 s   src/daemon/questions.ts  (resolveHoldMs)
//   < shim board-hold watchdog 660 s   scripts/fleet-hook.mjs   (rearmWatchdog 66e4)
//   < hooks.json hold timeout  720 s   hooks/hooks.json         (the 3 hold hooks)
//
// The P10 2C conversion (Deferred<HookResponse,never> hold settlement) is the
// one most able to silently perturb the daemon ceiling; this pin makes any drift
// of ANY tier — even one that preserves the ordering — a hard test failure that
// forces a re-review of the whole chain.
//
// WATCH 25 s CEILING (design 1E.2): GET /api/watch clamps its idle hold window to
// 25 s inline (http.ts watchHook), a literal that is neither exported nor covered
// by watch-rewake.test.ts (which only ever passes a SHORT synthetic hold_ms). The
// idle BODY bytes are already pinned there; only the ceiling constant is loose, so
// the P10 2B conversion (Deferred idle fold) could move it unnoticed. Pinned here.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';

import test from './helpers/harness-test.ts';
import { REPO_ROOT } from './helpers/daemon.ts';

// 1-based line number of a byte offset within a file's text.
function lineAt(text: string, index: number): number {
  return text.slice(0, index).split('\n').length;
}

interface HookCommand {
  args?: unknown;
  timeout?: unknown;
}
interface HookGroup {
  hooks?: HookCommand[];
}
interface HooksFile {
  hooks: Record<string, HookGroup[]>;
}

test('P10 D1 lockstep: daemon hold ceiling (650s) < shim board-hold watchdog (660s) < hooks.json hold timeout (720s)', () => {
  // ---- tier 1: daemon hold ceiling (resolveHoldMs clamp), questions.ts -------
  const daemonRel = 'src/daemon/questions.ts';
  const daemonSrc = readFileSync(path.join(REPO_ROOT, daemonRel), 'utf8');
  const daemonMatch = /Math\.min\(raw,\s*([0-9_]+)\)/.exec(daemonSrc);
  assert.ok(
    daemonMatch,
    `${daemonRel}: the resolveHoldMs ceiling "Math.min(raw, <ms>)" is gone — the D1 daemon tier moved or was renamed`,
  );
  const daemonLine = lineAt(daemonSrc, daemonMatch.index);
  const daemonRaw = daemonMatch[1];
  assert.ok(
    daemonRaw,
    `${daemonRel}:${daemonLine}: resolveHoldMs ceiling matched but captured no number`,
  );
  const daemonCeilingMs = Number(daemonRaw.replace(/_/g, ''));

  // ---- tier 2: shim board-hold watchdog rearm, fleet-hook.mjs ----------------
  const shimRel = 'scripts/fleet-hook.mjs';
  const shimSrc = readFileSync(path.join(REPO_ROOT, shimRel), 'utf8');
  // Anchor to the board-hold branch specifically (there is a second, unrelated
  // rearmWatchdog(500) for the short pre-hold re-arm).
  const shimMatch = /longBoardHold = true;\s*rearmWatchdog\(\s*([^)]+?)\s*\)/.exec(shimSrc);
  assert.ok(
    shimMatch,
    `${shimRel}: the board-hold "rearmWatchdog(<ms>)" after "longBoardHold = true" is gone — the D1 shim tier moved or was renamed`,
  );
  const shimLine = lineAt(shimSrc, shimMatch.index);
  const shimRaw = shimMatch[1];
  assert.ok(shimRaw, `${shimRel}:${shimLine}: board-hold watchdog matched but captured no value`);
  const shimMs = Number(shimRaw); // Number('66e4') === 660000
  assert.ok(
    Number.isFinite(shimMs),
    `${shimRel}:${shimLine}: shim watchdog value ${JSON.stringify(shimRaw)} is not a finite number`,
  );

  // ---- tier 3: hooks.json timeout for the three HOLD hooks --------------------
  const hooksRel = 'hooks/hooks.json';
  const hooksRaw = readFileSync(path.join(REPO_ROOT, hooksRel), 'utf8');
  const hooks = JSON.parse(hooksRaw) as HooksFile;
  const holdEvents = new Set(['PermissionRequest', 'Elicitation', 'AskUserQuestion']);
  const holdTimeouts: { event: string; timeout: number }[] = [];
  for (const [event, groups] of Object.entries(hooks.hooks ?? {})) {
    for (const group of groups ?? []) {
      for (const h of group.hooks ?? []) {
        const args = Array.isArray(h.args) ? h.args : [];
        const isFleetHook = args.some((a) => typeof a === 'string' && a.includes('fleet-hook.mjs'));
        const namesHold = args.some((a) => typeof a === 'string' && holdEvents.has(a));
        if (isFleetHook && namesHold && typeof h.timeout === 'number') {
          holdTimeouts.push({ event, timeout: h.timeout });
        }
      }
    }
  }
  assert.equal(
    holdTimeouts.length,
    3,
    `${hooksRel}: expected 3 fleet-hook.mjs hold hooks (PermissionRequest/Elicitation/AskUserQuestion), found ${JSON.stringify(holdTimeouts)}`,
  );
  const distinct = [...new Set(holdTimeouts.map((h) => h.timeout))];
  assert.equal(
    distinct.length,
    1,
    `${hooksRel}: the 3 hold hooks must share one timeout, found ${JSON.stringify(holdTimeouts)}`,
  );
  const hooksTimeoutMs = (distinct[0] ?? 0) * 1000; // hooks.json timeouts are SECONDS

  // ---- exact-value pins: any edit to a tier trips this, ordering or not ------
  assert.equal(
    daemonCeilingMs,
    650_000,
    `D1 tier drift: daemon hold ceiling is ${daemonCeilingMs}ms (${daemonRel}:${daemonLine}), expected 650000 — re-review the whole lockstep chain`,
  );
  assert.equal(
    shimMs,
    660_000,
    `D1 tier drift: shim board-hold watchdog is ${shimMs}ms (${shimRel}:${shimLine}), expected 660000 — re-review the whole lockstep chain`,
  );
  assert.equal(
    hooksTimeoutMs,
    720_000,
    `D1 tier drift: hooks.json hold timeout is ${hooksTimeoutMs}ms (${hooksRel}), expected 720000 — re-review the whole lockstep chain`,
  );

  // ---- the invariant itself: strictly increasing across the chain ------------
  assert.ok(
    daemonCeilingMs < shimMs,
    `D1 LOCKSTEP VIOLATION: daemon hold ceiling ${daemonCeilingMs}ms (${daemonRel}:${daemonLine}) must stay strictly BELOW the shim watchdog ${shimMs}ms (${shimRel}:${shimLine}); otherwise a board answer lands on a socket the shim already failed open`,
  );
  assert.ok(
    shimMs < hooksTimeoutMs,
    `D1 LOCKSTEP VIOLATION: shim watchdog ${shimMs}ms (${shimRel}:${shimLine}) must stay strictly BELOW the hooks.json hold timeout ${hooksTimeoutMs}ms (${hooksRel}); otherwise the CLI kills the hook before the shim's own fail-open runs`,
  );
});

test('P10 1E.2 watch 25s idle ceiling: GET /api/watch clamps hold_ms to 25_000 inline (ceiling + omitted-default)', () => {
  const httpRel = 'src/daemon/http.ts';
  const httpSrc = readFileSync(path.join(REPO_ROOT, httpRel), 'utf8');
  // The watchHook clamp:
  //   const holdMs = Number.isFinite(holdRaw) ? Math.max(0, Math.min(holdRaw, 25_000)) : 25_000;
  // Pin BOTH literals: the finite-path ceiling AND the omitted/non-finite default
  // (proving an omitted hold_ms parks for a long window, never resolves instantly).
  const clampMatch = /Math\.max\(0,\s*Math\.min\(holdRaw,\s*([0-9_]+)\)\)\s*:\s*([0-9_]+)/.exec(
    httpSrc,
  );
  assert.ok(
    clampMatch,
    `${httpRel}: the watchHook clamp "Math.max(0, Math.min(holdRaw, <ms>)) : <ms>" is gone — the 1E.2 watch ceiling moved or was renamed`,
  );
  const clampLine = lineAt(httpSrc, clampMatch.index);
  const ceilingRaw = clampMatch[1];
  const defaultRaw = clampMatch[2];
  assert.ok(
    ceilingRaw && defaultRaw,
    `${httpRel}:${clampLine}: watch clamp matched but captured no numbers`,
  );
  const ceilingMs = Number(ceilingRaw.replace(/_/g, ''));
  const defaultMs = Number(defaultRaw.replace(/_/g, ''));
  assert.equal(
    ceilingMs,
    25_000,
    `1E.2 drift: watch idle ceiling is ${ceilingMs}ms (${httpRel}:${clampLine}), expected 25000`,
  );
  assert.equal(
    defaultMs,
    25_000,
    `1E.2 drift: watch omitted-hold_ms default is ${defaultMs}ms (${httpRel}:${clampLine}), expected 25000`,
  );
});
