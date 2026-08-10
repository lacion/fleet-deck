// tests/coder-doc.test.ts
//
// Documentation contract for docs/CODER.md (BUG-019).
//
// The Coder deployment guide tells operators which origin to put in
// FLEETDECK_TRUSTED_ORIGINS. Fleet Deck matches that entry literally against
// the browser-facing Host/Origin (scripts/fleetd/http.mjs), so a hostname that
// Coder never generates yields a board whose shell loads and then 403s on
// /state, both WebSocket upgrades, and every control request.
//
// Coder v2.34.7 (coderd/database/db2sdk/db2sdk.go AppSubdomain) generates:
//   named coder_app (e.g. slug "fleetdeck"): <slug>--<workspace>--<owner>
//   raw-port app (e.g. url = "...:4711"):    <port>--<agent>--<workspace>--<owner>
// The agent name appears ONLY in the port form. The doc's template and every
// concrete example must therefore use the three-component named form, which is
// what tests/fixtures/coder-app-hostnames.json pins.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

interface CoderAppHostnames {
  named_app: { hostname: string; origin: string };
  port_app: { hostname: string };
  never_trusted: string[];
}

const HERE = path.dirname(fileURLToPath(import.meta.url));
const DOC = readFileSync(path.resolve(HERE, '../docs/CODER.md'), 'utf8');
const CONTRACT = JSON.parse(
  readFileSync(path.resolve(HERE, 'fixtures/coder-app-hostnames.json'), 'utf8'),
) as CoderAppHostnames;

// Hostnames the doc presents as something to trust: values assigned to
// FLEETDECK_TRUSTED_ORIGINS (HCL template and sh example). Mentions elsewhere
// in prose — e.g. the "what NOT to trust" warning — are not extracted.
function trustedHostnames(doc: string): string[] {
  const hosts = new Set<string>();
  for (const m of doc.matchAll(/FLEETDECK_TRUSTED_ORIGINS="https:\/\/(fleetdeck--[^"]+)"/g)) {
    const host = m[1];
    if (host !== undefined) hosts.add(host);
  }
  return [...hosts];
}

test('CODER.md template builds the named-app hostname Coder actually generates', () => {
  // The HCL template must interpolate workspace THEN owner, with no agent
  // component: fleetdeck--<workspace>--<owner>.<wildcard>
  assert.match(
    DOC,
    /FLEETDECK_TRUSTED_ORIGINS="https:\/\/fleetdeck--\$\{data\.coder_workspace\.me\.name\}--\$\{data\.coder_workspace_owner\.me\.name\}\.\$\{var\.coder_wildcard_domain\}"/,
    'template must be fleetdeck--${data.coder_workspace.me.name}--${data.coder_workspace_owner.me.name}',
  );
});

test('every trusted fleetdeck-- hostname in CODER.md is the named-app form', () => {
  const hosts = trustedHostnames(DOC);
  assert.ok(hosts.length >= 2, 'expected the HCL template and the sh example');
  const hcl = hosts.find((h) => h.includes('${'));
  const concrete = hosts.filter((h) => !h.includes('${'));
  // The HCL template: workspace THEN owner, no agent.
  assert.equal(
    hcl,
    '${data.coder_workspace.me.name}--${data.coder_workspace_owner.me.name}.${var.coder_wildcard_domain}'.replace(
      /^/,
      'fleetdeck--',
    ),
  );
  // Every concrete example equals the fixture's named-app origin host.
  for (const host of concrete) {
    assert.equal(
      host,
      CONTRACT.named_app.origin.replace('https://', ''),
      `doc trusts ${host}, but Coder generates ${CONTRACT.named_app.hostname} for a named coder_app`,
    );
  }
});

test('no trusted hostname in CODER.md carries the agent component', () => {
  // Skip the HCL template (its `${…}` placeholders are covered by the
  // template-ordering test); check the concrete examples.
  for (const host of trustedHostnames(DOC).filter((h) => !h.includes('${'))) {
    for (const never of CONTRACT.never_trusted) {
      assert.notEqual(
        host,
        never,
        `${host} includes the agent name — Coder only does that for raw-port apps`,
      );
    }
    // Three `--`-separated components: slug--workspace--owner.
    assert.equal(
      host.split('.')[0]?.split('--').length,
      3,
      `${host} must be <slug>--<workspace>--<owner>`,
    );
  }
});

test('CODER.md documents the named-vs-port hostname distinction', () => {
  // The port-app form (agent included) must appear only as an explanation of
  // what NOT to trust, so operators can tell the two apart.
  assert.ok(
    DOC.includes(CONTRACT.port_app.hostname),
    'doc should show the raw-port hostname form to distinguish it from the named form',
  );
});
