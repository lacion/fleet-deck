// tests/accept-scripts-hook-wiring.test.mjs
//
// BUG-010 regression guard: the live acceptance gates must wire the current
// checkout's AUTHENTICATED command shims, never tokenless native HTTP hooks.
//
// Since 0.16.0 every /hook/* route requires the bearer token, and
// scripts/fleetd/http.mjs answers unauthenticated hook POSTs with the legacy
// refusal — so a gate that renders `"type": "http"` hooks into
// demo/project/.claude/settings.json either fails for a correct source tree,
// or silently passes while an installed Fleet Deck plugin's duplicate hooks
// do the real work with cached code. This test greps the three gates for the
// defect shape and for the isolation that keeps an installed plugin from
// masking the checkout. run-smoke.sh is the proven reference wiring and is
// asserted too, so a future edit can't drift it back.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const GATES = [
  'demo/run-smoke.sh',
  'demo/run-accept-phase3.sh',
  'demo/run-accept-plan.sh',
  'demo/run-accept-spawn.sh',
];

// Every hook event the daemon ingests must reach it through the authenticated
// fleet-hook.mjs shim (SessionStart and the Stop asyncRewake leg have their
// own dedicated scripts).
const SHIMMED_EVENTS = [
  'UserPromptSubmit',
  'PostToolUse',
  'AskUserQuestion',
  'PermissionRequest',
  'Elicitation',
  'Notification',
  'Stop',
  'SessionEnd',
];

for (const gate of GATES) {
  const src = readFileSync(path.join(ROOT, gate), 'utf8');

  test(`${gate}: no tokenless native HTTP hooks (BUG-010)`, () => {
    assert.ok(
      !src.includes('"type": "http"'),
      `${gate} renders a native HTTP hook; http hooks cannot attach the bearer ` +
      'token required since 0.16.0, so the daemon refuses them and the gate ' +
      'tests the refusal path, not the checkout',
    );
  });

  test(`${gate}: every hook event goes through the authenticated fleet-hook shim`, () => {
    assert.ok(
      src.includes('FLEET_HOOK_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-hook.mjs"'),
      `${gate} must resolve the checkout's fleet-hook.mjs shim`,
    );
    // The shim path may be quoted (run-smoke.sh style, possibly backslash-
    // escaped inside a heredoc) or bare — normalize, then match.
    const wired = src.replace(/\\?"\$FLEET_HOOK_SCRIPT\\?"/g, '$FLEET_HOOK_SCRIPT');
    for (const event of SHIMMED_EVENTS) {
      assert.ok(
        wired.includes(`$FLEET_HOOK_SCRIPT ${event}`),
        `${gate} must wire ${event} as a command hook through fleet-hook.mjs`,
      );
    }
  });
}

for (const gate of GATES.filter(g => g !== 'demo/run-smoke.sh')) {
  const src = readFileSync(path.join(ROOT, gate), 'utf8');

  test(`${gate}: an installed Fleet Deck plugin cannot mask the checkout (BUG-010)`, () => {
    assert.ok(
      src.includes('"enabledPlugins": { "fleetdeck@fleetdeck": false }'),
      `${gate} must disable the installed fleetdeck plugin in the generated ` +
      'project settings, or its duplicate hooks can exercise cached installed ' +
      'code instead of the checkout',
    );
  });
}

test('run-accept-phase3.sh: source validation shadows any installed plugin with --plugin-dir', () => {
  const src = readFileSync(path.join(ROOT, 'demo/run-accept-phase3.sh'), 'utf8');
  assert.ok(
    src.includes('--plugin-dir "$FLEETDECK_ROOT"'),
    'the direct `claude` launches in run-accept-phase3.sh must load the checkout ' +
    'via --plugin-dir so a same-named installed plugin is shadowed for the session',
  );
});
