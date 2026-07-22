// tests/gateway.test.mjs
//
// 0.15.0 — LLM gateway routing. A spawn can be pointed at an Anthropic-compatible
// proxy (CLIProxyAPI, a corporate gateway) instead of Anthropic, per session.
//
// The three properties worth a test here are the three that are expensive to be
// wrong about:
//
//   1. THE CREDENTIAL NEVER LEAVES THE DAEMON. resolveSettings() rides the
//      /state snapshot to every connected board — a phone over LAN mode
//      included — so `gateway_token` must be readable back as a boolean and
//      never as itself, from /api/settings, from /state, or from anywhere in
//      either payload at any depth.
//   2. ROUTING IS EXACTLY WHAT THE SPAWN ASKED FOR. A pane that asked for no
//      gateway must have all four gateway variables scrubbed (an ambient export
//      in the daemon's shell must not reroute it); a pane that asked for one
//      must actually receive it, which means the `env -u` prefix must NOT scrub
//      the very variables tmux is setting via `-e`.
//   3. REMOTE CONTROL IS REFUSED, LOUDLY. Claude Code disables Remote Control
//      whenever ANTHROPIC_BASE_URL points at a non-Anthropic host, so accepting
//      both would hand back a session whose 📱 link silently never appears.
//
// Like tests/spawn.test.mjs this never launches a real pane or a real billed
// session: FLEETDECK_SPAWN_CMD stands in for tmux and records the spec the
// daemon would have executed. The gateway env reaches that spec as `gateway_env`
// (a deliberate test-seam exception documented in spawns.mjs launchPane) — on
// the real tmux path it travels as `new-window -e` and never enters argv.

import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { randomPort, startDaemon } from './helpers/daemon.mjs';
import { postJson, getJson, postHook } from './helpers/http.mjs';
import { waitForSpecRecords } from './helpers/wait.mjs';
import { claudeTranscriptPath } from '../scripts/fleetd/helpers.mjs';
import { openDb } from '../scripts/fleetd/db.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.mjs');
try { chmodSync(SPAWN_CMD_FIXTURE, 0o755); } catch { /* best-effort, as in spawn.test.mjs */ }

const BASE_URL = 'http://127.0.0.1:8317';
const TOKEN = 'super-secret-gateway-credential';

const GATEWAY_VARS = [
  'ANTHROPIC_BASE_URL', 'ANTHROPIC_AUTH_TOKEN', 'ANTHROPIC_API_KEY',
  'CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY',
];

function scratchDir() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-gateway-'));
}

function rawJsonPost(port, pathname, body, headers = {}) {
  return new Promise((resolve, reject) => {
    const payload = JSON.stringify(body);
    const req = http.request({
      host: '127.0.0.1', port, path: pathname, method: 'POST',
      headers: { 'content-type': 'application/json', 'content-length': Buffer.byteLength(payload), ...headers },
    }, res => {
      let text = '';
      res.on('data', chunk => { text += chunk; });
      res.on('end', () => resolve({ status: res.statusCode, text }));
    });
    req.on('error', reject);
    req.end(payload);
  });
}

/** Boot a daemon with the spawn fixture wired up.
 *
 * HOME is pointed at a scratch directory because revive eligibility is decided
 * by the existence of ~/.claude/projects/<munged-cwd>/<sid>.jsonl — the revive
 * tests below fabricate one there (writeTranscript). Without this the revive
 * calls 410 on "resume transcript no longer exists" and the inheritance rules
 * they exist to prove go completely untested. */
async function gatewayDaemon(extraEnv = {}) {
  const record = path.join(scratchDir(), 'spec.jsonl');
  const userHome = scratchDir();
  const daemon = await startDaemon({
    env: {
      HOME: userHome,
      FLEETDECK_SPAWN_CMD: SPAWN_CMD_FIXTURE,
      FLEETDECK_TEST_SPAWN_RECORD: record,
      ...extraEnv,
    },
  });
  return { daemon, record, userHome };
}

function withDb(home, fn) {
  const db = openDb(path.join(home, 'fleetd.db'));
  try { return fn(db); } finally { db.close(); }
}

/** Drive a freshly-spawned session all the way to REVIVABLE, the same way
 * tests/revive.test.mjs does: register it with a hook, fabricate the transcript
 * revive's H-R7 eligibility check insists on seeing, then settle the row
 * terminal in the DB (no real pane ever existed, so there is nothing to kill).
 * Returns the spawn id to revive. */
async function makeRevivable({ daemon, userHome, cwd, spawnBody }) {
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, ...spawnBody });
  assert.equal(spawned.status, 200, spawned.text);
  const { spawn_id, session_id } = spawned.json;
  await postHook(daemon.baseUrl, 'SessionStart', { session_id, cwd, source: 'startup' }, { token: daemon.token });

  const file = claudeTranscriptPath(cwd, session_id, userHome);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, '{"type":"summary"}\n');

  withDb(daemon.home, db => {
    db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(spawn_id);
    db.prepare("UPDATE sessions SET col = 'offline', note = 'pane gone', ended_at = ?, archived_at = ? WHERE session_id = ?")
      .run(Date.now(), Date.now(), session_id);
  });
  return spawn_id;
}

/** Configure a complete, usable gateway profile. */
async function configure(daemon, extra = {}) {
  const res = await postJson(`${daemon.baseUrl}/api/settings`, {
    gateway_base_url: BASE_URL,
    gateway_token: TOKEN,
    ...extra,
  }, { token: daemon.token });
  assert.equal(res.status, 200, res.text);
  return res;
}

/** Every `-u NAME` pair in an `env`-prefixed argv. */
function scrubbedNames(argv) {
  const out = new Set();
  for (let i = 0; i < argv.length; i++) if (argv[i] === '-u') out.add(argv[i + 1]);
  return out;
}

/** Does `value` contain `needle` as a substring anywhere, at any depth? */
function leaksAnywhere(value, needle, seen = new Set()) {
  if (typeof value === 'string') return value.includes(needle);
  if (value && typeof value === 'object') {
    if (seen.has(value)) return false;
    seen.add(value);
    for (const [k, v] of Object.entries(value)) {
      if (k.includes(needle)) return true;
      if (leaksAnywhere(v, needle, seen)) return true;
    }
  }
  return false;
}

// ---------------------------------------------------------------- masking

test('gateway: gateway_* writes require the bearer even under proxy trust — Host/Origin are forgeable', async (t) => {
  // arrivedViaTrustedProxy() reads the Host and Origin headers, both of which a
  // direct loopback caller controls. Everywhere else PROXY_AUTH=trust waives the
  // token on that signal — that is the documented 0.13.0 contract — but gateway_*
  // is the one write that reroutes every future session's LLM traffic and can
  // exfiltrate the gateway credential, so it must NOT ride a forgeable header. A
  // local process that forges the trusted hostname must still be refused.
  const proxyOrigin = 'https://board.example.com';
  const forgedHeaders = { host: 'board.example.com', origin: proxyOrigin };
  const trustPort = randomPort();
  let tokenPort = randomPort();
  while (tokenPort === trustPort) tokenPort = randomPort();
  const trust = await startDaemon({
    port: trustPort,
    env: { FLEETDECK_TRUSTED_ORIGINS: proxyOrigin, FLEETDECK_PROXY_AUTH: 'trust' },
  });
  t.after(() => trust.stop());
  const token = await startDaemon({
    port: tokenPort,
    env: { FLEETDECK_TRUSTED_ORIGINS: proxyOrigin, FLEETDECK_PROXY_AUTH: 'token' },
  });
  t.after(() => token.stop());

  // THE REGRESSION THIS TEST EXISTS FOR: a direct loopback request forging the
  // trusted proxy's Host/Origin under trust mode is refused, not waived.
  const forgedTrustWrite = await rawJsonPost(trust.port, '/api/settings', { gateway_base_url: BASE_URL }, forgedHeaders);
  assert.equal(forgedTrustWrite.status, 401,
    `forged trusted headers must not waive the gateway bearer under trust mode: ${forgedTrustWrite.text}`);

  // The real proxy (or anyone) presenting the bearer is accepted, in either mode.
  const trustWithBearer = await rawJsonPost(trust.port, '/api/settings', { gateway_base_url: BASE_URL }, {
    ...forgedHeaders, authorization: `Bearer ${trust.token}`,
  });
  assert.equal(trustWithBearer.status, 200, `trust mode accepts the bearer: ${trustWithBearer.text}`);

  const tokenWrite = await rawJsonPost(token.port, '/api/settings', { gateway_base_url: BASE_URL }, forgedHeaders);
  assert.equal(tokenWrite.status, 401, 'proxy token mode still requires the bearer');

  const authenticatedWrite = await rawJsonPost(token.port, '/api/settings', { gateway_base_url: BASE_URL }, {
    ...forgedHeaders, authorization: `Bearer ${token.token}`,
  });
  assert.equal(authenticatedWrite.status, 200, `proxy token mode accepts its bearer: ${authenticatedWrite.text}`);
});

test('gateway: the token is stored, usable, and never served back to a client', async (t) => {
  const { daemon } = await gatewayDaemon();
  t.after(() => daemon.stop());

  const saved = await configure(daemon);

  // The POST response, GET /api/settings and /state are three separate doors on
  // the same resolveSettings() object; all three must be masked. /state is the
  // one that matters most — it is broadcast, not requested.
  const fetched = await getJson(`${daemon.baseUrl}/api/settings`);
  const state = await getJson(`${daemon.baseUrl}/state`);

  for (const [label, payload] of [
    ['the POST /api/settings response', saved.json],
    ['GET /api/settings', fetched.json],
    ['the /state snapshot', state.json],
  ]) {
    assert.equal(leaksAnywhere(payload, TOKEN), false,
      `${label} must not contain the gateway credential anywhere at any depth`);
  }

  // …but it IS configured, and the board can tell.
  const gw = fetched.json.settings.gateway;
  assert.equal(gw.token_set, true, 'token_set must report that a credential exists');
  assert.equal(gw.base_url, BASE_URL, 'the base URL is not secret — the board shows it');
  assert.equal(gw.ready, true, 'base_url + token ⇒ ready');
  assert.equal(gw.auth_style, 'bearer', 'bearer is the default auth style');
  assert.equal(gw.model_discovery, true, 'model discovery defaults on');
  assert.equal(gw.default, false, 'routing every spawn through the gateway is opt-in');
  assert.equal(Object.hasOwn(gw, 'token'), false, 'there must be no token field at all');

  assert.equal(state.json.settings.gateway.token_set, true,
    'the snapshot carries the masked profile so the board can gate its toggle');
});

test('gateway: a half-configured profile is not ready and refuses a spawn that asked for it', async (t) => {
  const { daemon } = await gatewayDaemon();
  t.after(() => daemon.stop());

  // A base URL with no credential would reach the proxy and 401 — which reads
  // as a Claude Code bug rather than a settings mistake. Refuse it up front.
  const res = await postJson(`${daemon.baseUrl}/api/settings`, { gateway_base_url: BASE_URL }, { token: daemon.token });
  assert.equal(res.status, 200, res.text);
  assert.equal(res.json.settings.gateway.ready, false, 'no token ⇒ not ready');

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });
  assert.equal(spawn.status, 400, spawn.text);
  assert.match(spawn.json.reason, /not configured/i);
  assert.match(spawn.json.reason, /gateway_token/, 'the refusal must name the missing piece');
});

test('gateway: settings validation refuses bad URLs, schemes and auth styles', async (t) => {
  const { daemon } = await gatewayDaemon();
  t.after(() => daemon.stop());

  const bad = [
    [{ gateway_base_url: 'not-a-url' }, /not a valid URL/i],
    [{ gateway_base_url: 'file:///etc/passwd' }, /http:\/\/ or https:\/\//i],
    [{ gateway_auth_style: 'basic' }, /bearer or api-key/i],
    [{ gateway_token: '' }, /non-empty string/i],
    [{ gateway_model_discovery: 'yes' }, /must be a boolean/i],
    [{ gateway_default: 'on' }, /must be a boolean/i],
    [{ gateway_token: 'x'.repeat(4097) }, /4096 characters or fewer/i],
    [{ gateway_token: 'tok\u0000en' }, /control characters/i],
    [{ gateway_base_url: 'http://gw\u001f.example.com' }, /control characters/i],
    // SECURITY (see validateGatewayBaseUrl): base_url is served UNMASKED to
    // every board over /state, so a credential spelled into it would ride that
    // public path. Both smuggling routes are refused at the door — url.href
    // preserves userinfo and query, so normalization would not have saved us.
    [{ gateway_base_url: 'https://user:hunter2@gw.example.com' }, /must not embed credentials/i],
    [{ gateway_base_url: 'https://gw.example.com/?api_key=sekrit' }, /must not carry a query string/i],
  ];
  for (const [body, re] of bad) {
    const res = await postJson(`${daemon.baseUrl}/api/settings`, body, { token: daemon.token });
    assert.equal(res.status, 400, `${JSON.stringify(body)} → ${res.text}`);
    assert.match(res.json.reason, re);
  }

  // validate-all-then-apply-all: one bad field must leave the store untouched.
  const mixed = await postJson(`${daemon.baseUrl}/api/settings`, {
    gateway_base_url: BASE_URL, gateway_auth_style: 'nonsense',
  }, { token: daemon.token });
  assert.equal(mixed.status, 400, mixed.text);
  const after = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(after.json.settings.gateway.base_url, null,
    'a rejected mixed body must not have half-applied the valid key');

  // A trailing slash is normalized once, at the door, so /state and the injected
  // env can never disagree on spelling.
  await postJson(`${daemon.baseUrl}/api/settings`, { gateway_base_url: `${BASE_URL}/` }, { token: daemon.token });
  const normalized = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(normalized.json.settings.gateway.base_url, BASE_URL);
});

// ---------------------------------------------------------------- routing

test('gateway: a spawn that did not ask for one has all four variables scrubbed', async (t) => {
  const { daemon, record } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);   // configured, but this spawn does not opt in

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(spawn.status, 200, spawn.text);

  const [rec] = await waitForSpecRecords(record, 1);
  const scrubbed = scrubbedNames(rec.parsed.argv);
  for (const name of GATEWAY_VARS) {
    assert.equal(scrubbed.has(name), true,
      `${name} must be scrubbed from a non-gateway pane — an ambient export in the daemon's shell must never reroute a session`);
  }
  assert.equal(rec.parsed.gateway, false);
  assert.equal(rec.parsed.gateway_env, null);
});

test('gateway: gateway:true delivers the env and exempts exactly those names from the scrub', async (t) => {
  const { daemon, record } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });
  assert.equal(spawn.status, 200, spawn.text);

  const [rec] = await waitForSpecRecords(record, 1);
  assert.equal(rec.parsed.gateway, true);
  assert.deepEqual(rec.parsed.gateway_env, {
    ANTHROPIC_BASE_URL: BASE_URL,
    ANTHROPIC_AUTH_TOKEN: TOKEN,
    CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY: '1',
  });

  // THE REGRESSION THIS TEST EXISTS FOR: tmux sets these via `-e`, and the
  // pane's own `env -u` prefix runs AFTER that. If the prefix still unset them
  // the pane would silently route to Anthropic despite everything above.
  const scrubbed = scrubbedNames(rec.parsed.argv);
  for (const name of Object.keys(rec.parsed.gateway_env)) {
    assert.equal(scrubbed.has(name), false,
      `${name} is being supplied via tmux -e, so the env -u prefix must not strip it back off`);
  }
  // Only the supplied names are exempt: an ambient x-api-key credential is
  // still scrubbed from a bearer-style gateway pane.
  assert.equal(scrubbed.has('ANTHROPIC_API_KEY'), true,
    'a variable the launch did NOT supply stays scrubbed');

  // The credential must not reach argv on any path.
  assert.equal(rec.parsed.argv.some(a => String(a).includes(TOKEN)), false,
    'the credential must never appear in the pane argv');
});

test('gateway: auth_style picks the header, so it picks the variable', async (t) => {
  const { daemon, record } = await gatewayDaemon();
  t.after(() => daemon.stop());
  // ANTHROPIC_API_KEY travels as x-api-key; ANTHROPIC_AUTH_TOKEN as
  // Authorization: Bearer. A credential in the wrong one 401s at the gateway.
  await configure(daemon, { gateway_auth_style: 'api-key', gateway_model_discovery: false });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });

  const [rec] = await waitForSpecRecords(record, 1);
  assert.deepEqual(rec.parsed.gateway_env, {
    ANTHROPIC_BASE_URL: BASE_URL,
    ANTHROPIC_API_KEY: TOKEN,
  }, 'api-key style sets ANTHROPIC_API_KEY, and discovery:false omits the flag entirely');

  const scrubbed = scrubbedNames(rec.parsed.argv);
  assert.equal(scrubbed.has('ANTHROPIC_AUTH_TOKEN'), true,
    'the bearer variable is not supplied here, so it stays scrubbed');
  assert.equal(scrubbed.has('CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY'), true,
    'discovery was turned off, so that flag is scrubbed rather than set');
});

test('gateway: gateway_default routes a spawn that says nothing, and gateway:false still opts out', async (t) => {
  const { daemon, record } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon, { gateway_default: true });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  const [silent] = await waitForSpecRecords(record, 1);
  assert.equal(silent.parsed.gateway, true, 'silence defers to gateway_default');

  // An explicit false always wins over the default — the escape hatch for the
  // one session you want billed to Anthropic.
  await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: false });
  const recs = await waitForSpecRecords(record, 2);
  assert.equal(recs[1].parsed.gateway, false, 'gateway:false overrides gateway_default');
  assert.equal(scrubbedNames(recs[1].parsed.argv).has('ANTHROPIC_BASE_URL'), true);
});

// ------------------------------------------------- remote-control conflict

test('gateway: remote control and the gateway are refused together, with a reason that explains why', async (t) => {
  const { daemon } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd, gateway: true, remote_control: true,
  });
  assert.equal(res.status, 400, res.text);
  assert.match(res.json.reason, /remote control is unavailable on a gateway-routed session/i);
  assert.match(res.json.reason, /ANTHROPIC_BASE_URL/,
    'the refusal must say WHY — this is a Claude Code behaviour, not a Fleet Deck policy');

  // The same collision via gateway_default rather than an explicit flag.
  await postJson(`${daemon.baseUrl}/api/settings`, { gateway_default: true }, { token: daemon.token });
  const viaDefault = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, remote_control: true });
  assert.equal(viaDefault.status, 400, viaDefault.text);
  assert.match(viaDefault.json.reason, /remote control is unavailable/i);

  // Either one alone is fine.
  await postJson(`${daemon.baseUrl}/api/settings`, { gateway_default: false }, { token: daemon.token });
  const rcOnly = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, remote_control: true });
  assert.equal(rcOnly.status, 200, rcOnly.text);
});

test('gateway: a non-boolean gateway flag is refused', async (t) => {
  const { daemon } = await gatewayDaemon();
  t.after(() => daemon.stop());
  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: 'yes' });
  assert.equal(res.status, 400, res.text);
  assert.match(res.json.reason, /gateway must be a boolean/);
});

// ------------------------------------------------------------------ revive

test('gateway: routing survives death — a revive inherits the row, not the current default', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon();
  t.after(() => daemon.stop());
  // gateway_default STARTS ON so the flip below is a real state change. Without
  // this the "flip it off" POST is a no-op (the default is already false) and
  // the test silently proves less than its name claims.
  await configure(daemon, { gateway_default: true });

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  // A gateway-routed spawn, dead, then revived: the resumed pane must keep
  // talking to the same provider that produced the transcript it resumes.
  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: true } });
  await waitForSpecRecords(record, 1);

  // Flip the global default OFF. A revive must NOT consult it — it asks what
  // this lineage was doing, not what a new spawn would do.
  const flipped = await postJson(`${daemon.baseUrl}/api/settings`, { gateway_default: false }, { token: daemon.token });
  assert.equal(flipped.json.settings.gateway.default, false, 'sanity: the default really did change');

  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {});
  assert.equal(revive.status, 200, revive.text);

  const recs = await waitForSpecRecords(record, 2);
  const resumed = recs[1].parsed;
  assert.ok(resumed.argv.includes('--resume'), 'sanity: the second launch is a resume');
  assert.equal(resumed.gateway, true, "a revived pane must inherit its lineage's gateway routing");
  assert.equal(resumed.gateway_env.ANTHROPIC_BASE_URL, BASE_URL);
  assert.equal(resumed.gateway_env.ANTHROPIC_AUTH_TOKEN, TOKEN);
  assert.equal(scrubbedNames(resumed.argv).has('ANTHROPIC_BASE_URL'), false,
    'the resume prefix must exempt the supplied gateway names, exactly like a fresh spawn');
});

test('gateway: a lineage that never used the gateway is not rerouted by flipping the default on', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: false } });
  await waitForSpecRecords(record, 1);

  // Turning the default ON must not retroactively reroute an existing lineage:
  // gatewayDecision is handed a BOOLEAN from the row, never null, precisely so
  // this cannot consult gateway_default.
  await postJson(`${daemon.baseUrl}/api/settings`, { gateway_default: true }, { token: daemon.token });
  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {});
  assert.equal(revive.status, 200, revive.text);

  const recs = await waitForSpecRecords(record, 2);
  assert.equal(recs[1].parsed.gateway, false,
    'flipping gateway_default on must not reroute a lineage that never used it');
  assert.equal(scrubbedNames(recs[1].parsed.argv).has('ANTHROPIC_BASE_URL'), true,
    'and its resumed pane keeps the full scrub');
});

test('gateway: an explicit flag on the revive overrides what the row inherited', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: true } });
  await waitForSpecRecords(record, 1);

  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, { gateway: false });
  assert.equal(revive.status, 200, revive.text);
  const recs = await waitForSpecRecords(record, 2);
  assert.equal(recs[1].parsed.gateway, false, 'a human can move a lineage off the gateway on revive');
});

// ------------------------------------------------------------------ clearing

test('gateway: clearing the token disarms the profile without forgetting the URL', async (t) => {
  const { daemon } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);

  const cleared = await postJson(`${daemon.baseUrl}/api/settings`, { gateway_token: null }, { token: daemon.token });
  assert.equal(cleared.status, 200, cleared.text);
  assert.equal(cleared.json.settings.gateway.token_set, false);
  assert.equal(cleared.json.settings.gateway.ready, false, 'no credential ⇒ not spawnable');
  assert.equal(cleared.json.settings.gateway.base_url, BASE_URL,
    'clearing the credential must not also forget where the gateway lives');

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });
  assert.equal(spawn.status, 400, 'a disarmed profile must refuse, not silently bill Anthropic');
});

test('gateway: a revive stranded by a cleared token blames the settings, not the caller', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: true } });
  await waitForSpecRecords(record, 1);

  // The operator clears the credential. This revive INHERITED gateway:true from
  // its row — nobody asked for it on this call — so an error phrased as
  // "gateway:true was requested" would send them hunting for a bug in a request
  // they never made.
  await postJson(`${daemon.baseUrl}/api/settings`, { gateway_token: null }, { token: daemon.token });
  const stranded = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {});
  assert.equal(stranded.status, 400, stranded.text);
  assert.doesNotMatch(stranded.json.reason, /was requested/,
    'the caller requested nothing — the flag came off the row');
  assert.match(stranded.json.reason, /no longer configured/i);
  assert.match(stranded.json.reason, /"gateway":false/,
    'the refusal must name the escape hatch, or the lineage reads as permanently stuck');

  // And that escape hatch actually works.
  const rescued = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, { gateway: false });
  assert.equal(rescued.status, 200, rescued.text);
  const recs = await waitForSpecRecords(record, 2);
  assert.equal(recs[1].parsed.gateway, false);
});

// -------------------------------------------------------------------- adopt

test('gateway: adopt consults the default, because it has no lineage to inherit', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon, { gateway_default: true });

  // A session Fleet Deck never spawned: it registers by hook, so there is no
  // spawn row carrying a routing decision. The default is the only answer
  // available, which is exactly what a default is for.
  const cwd = scratchDir();
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', { session_id: sid, cwd, source: 'startup' }, { token: daemon.token });
  mkdirSync(path.dirname(claudeTranscriptPath(cwd, sid, userHome)), { recursive: true });
  writeFileSync(claudeTranscriptPath(cwd, sid, userHome), '{"type":"summary"}\n');
  // 'logout' is a hook-PROVEN end. NOT 'clear': that ends the session as
  // 'superseded' (the conversation continued under a new id), which is
  // deliberately never resumable — an earlier draft used it and the adopt was
  // refused before it could exercise anything.
  await postHook(daemon.baseUrl, 'SessionEnd', { session_id: sid, cwd, reason: 'logout' }, { token: daemon.token });

  const card = (await getJson(`${daemon.baseUrl}/state`)).json.sessions.find(s => s.session_id === sid);
  assert.equal(card.adopt.eligible, 'now', 'sanity: the session must actually be adoptable');
  assert.equal(card.spawn, undefined, 'sanity: no spawn row, so there is nothing to inherit');

  const adopt = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  // Fail loudly rather than skip: a conditional here would let the one path with
  // INVERTED gateway semantics quietly go untested.
  assert.equal(adopt.status, 200, `adopt did not launch, so its gateway rule went untested: ${adopt.text}`);
  const [rec] = await waitForSpecRecords(record, 1);
  assert.equal(rec.parsed.gateway, true, 'adopt honours gateway_default');
  assert.equal(rec.parsed.gateway_env.ANTHROPIC_BASE_URL, BASE_URL);
});

// ---------------------------------------------------------------- repo mode

test('gateway: a repo-mode spawn persists and delivers routing too', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon();
  t.after(() => daemon.stop());
  await configure(daemon);

  // Repo mode uses a DIFFERENT insertProvisionalSpawn call site than cwd mode —
  // the one with origin_url / requested_branch / branch_mode populated, where a
  // positional bind drifting by one would silently write `gateway` into the
  // wrong column. Every other gateway test here is cwd-mode.
  const root = scratchDir();
  t.after(() => rmSync(root, { recursive: true, force: true }));
  execFileSync('git', ['init', '-q', root]);
  execFileSync('git', ['-C', root, 'commit', '-q', '--allow-empty', '-m', 'init'],
    { env: { ...process.env, GIT_AUTHOR_NAME: 't', GIT_AUTHOR_EMAIL: 't@t', GIT_COMMITTER_NAME: 't', GIT_COMMITTER_EMAIL: 't@t' } });

  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: root, branch: 'gw-probe', branch_mode: 'worktree', gateway: true,
  });
  assert.equal(spawn.status, 200, spawn.text);

  const [rec] = await waitForSpecRecords(record, 1);
  assert.equal(rec.parsed.gateway, true);
  assert.equal(rec.parsed.gateway_env.ANTHROPIC_BASE_URL, BASE_URL);

  // The row is what a later revive reads, so prove the column — not just the
  // launch — actually carries the flag through the repo-mode insert.
  const card = (await getJson(`${daemon.baseUrl}/state`)).json
    .sessions.find(s => s.session_id === spawn.json.session_id);
  assert.equal(card.spawn.gateway, true,
    'the snapshot field that drives the card chip must reflect the persisted column');
  assert.equal(card.spawn.requested_branch, 'gw-probe',
    'sanity: the neighbouring columns are still aligned');
});
