// tests/gateway.test.ts
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

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  chmodSync,
  readdirSync,
  existsSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { randomPort, startDaemon, type DaemonHandle } from './helpers/daemon.ts';
import { postJson, getJson, postHook, rawRequest } from './helpers/http.ts';
import { waitForSpecRecords } from './helpers/wait.ts';
import { claudeTranscriptPath } from '../scripts/fleetd/helpers.ts';
import { openDb } from '../scripts/fleetd/db.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPAWN_CMD_FIXTURE = path.join(HERE, 'helpers/spawn-cmd-fixture.ts');
try {
  chmodSync(SPAWN_CMD_FIXTURE, 0o755);
} catch {
  /* best-effort, as in spawn.test.mjs */
}

const BASE_URL = 'http://127.0.0.1:8317';
const TOKEN = 'super-secret-gateway-credential';

const GATEWAY_VARS = [
  'ANTHROPIC_BASE_URL',
  'ANTHROPIC_AUTH_TOKEN',
  'ANTHROPIC_API_KEY',
  'CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY',
];

// ── Facets this suite reads off the daemon's `unknown` JSON bodies and the
//    fixture's `unknown[]` spec records. Each interface is the narrow view of
//    the shape actually asserted on below. ──

// The gateway-env map the fixture records. Keys are named (not an index
// signature) so DOT access does not trip noPropertyAccessFromIndexSignature.
interface GatewayEnv {
  ANTHROPIC_BASE_URL?: string;
  ANTHROPIC_AUTH_TOKEN?: string;
  ANTHROPIC_API_KEY?: string;
  CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY?: string;
}
// One recorded spawn spec — the fixture writes `{ parsed: {...} }` per launch.
interface SpecRecord {
  parsed: {
    argv: string[];
    gateway: boolean;
    gateway_env: GatewayEnv | null;
  };
}
// The masked gateway profile carried by /api/settings and /state.
interface GatewayProfile {
  token_set: boolean;
  base_url: string | null;
  ready: boolean;
  auth_style: string;
  model_discovery: boolean;
  default: boolean;
}
interface SettingsResponse {
  settings: { gateway: GatewayProfile };
}
interface SessionCard {
  session_id: string;
  col?: string;
  note?: string;
  adopt?: { eligible?: string };
  spawn?: { gateway?: boolean; requested_branch?: string };
}
interface StateResponse {
  settings: { gateway: GatewayProfile };
  sessions: SessionCard[];
}
interface SpawnResponse {
  spawn_id?: string;
  session_id: string;
  reason?: string;
}
interface ReasonResponse {
  reason: string;
}

function scratchDir() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-gateway-'));
}

function rawJsonPost(
  port: number,
  pathname: string,
  body: unknown,
  headers: Record<string, string | number> = {},
) {
  const payload = JSON.stringify(body);
  return rawRequest({
    port,
    path: pathname,
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'content-length': Buffer.byteLength(payload),
      ...headers,
    },
    body: payload,
  });
}

/** Boot a daemon with the spawn fixture wired up.
 *
 * HOME is pointed at a scratch directory because revive eligibility is decided
 * by the existence of ~/.claude/projects/<munged-cwd>/<sid>.jsonl — the revive
 * tests below fabricate one there (writeTranscript). Without this the revive
 * calls 410 on "resume transcript no longer exists" and the inheritance rules
 * they exist to prove go completely untested. */
async function gatewayDaemon(t: TestContext, extraEnv: Record<string, string> = {}) {
  const recordDir = scratchDir();
  const record = path.join(recordDir, 'spec.jsonl');
  const userHome = scratchDir();
  // Both scratch trees hold credential-bearing spawn specs and fabricated
  // transcripts, and daemon.stop() removes only the daemon's own home —
  // register their teardown BEFORE the boot so a failed startDaemon still
  // cleans them up (BUG-163).
  t.after(() => {
    rmSync(recordDir, { recursive: true, force: true });
  });
  t.after(() => {
    rmSync(userHome, { recursive: true, force: true });
  });
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

function withDb<T>(home: string, fn: (db: ReturnType<typeof openDb>) => T): T {
  const db = openDb(path.join(home, 'fleetd.db'));
  try {
    return fn(db);
  } finally {
    db.close();
  }
}

/** Drive a freshly-spawned session all the way to REVIVABLE, the same way
 * tests/revive.test.mjs does: register it with a hook, fabricate the transcript
 * revive's H-R7 eligibility check insists on seeing, then settle the row
 * terminal in the DB (no real pane ever existed, so there is nothing to kill).
 * Returns the spawn id to revive. */
async function makeRevivable({
  daemon,
  userHome,
  cwd,
  spawnBody,
}: {
  daemon: DaemonHandle;
  userHome: string;
  cwd: string;
  spawnBody: Record<string, unknown>;
}) {
  const spawned = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, ...spawnBody });
  assert.equal(spawned.status, 200, spawned.text);
  const { spawn_id, session_id } = spawned.json as SpawnResponse;
  assert.ok(spawn_id, 'a successful spawn returns a spawn id');
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    { session_id, cwd, source: 'startup' },
    { token: daemon.token },
  );

  const file = claudeTranscriptPath(cwd, session_id, userHome);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, '{"type":"summary"}\n');

  withDb(daemon.home, (db) => {
    db.prepare("UPDATE spawns SET status = 'gone' WHERE spawn_id = ?").run(spawn_id);
    db.prepare(
      "UPDATE sessions SET col = 'offline', note = 'pane gone', ended_at = ?, archived_at = ? WHERE session_id = ?",
    ).run(Date.now(), Date.now(), session_id);
  });
  return spawn_id;
}

/** Configure a complete, usable gateway profile. */
async function configure(daemon: DaemonHandle, extra: Record<string, unknown> = {}) {
  const res = await postJson(
    `${daemon.baseUrl}/api/settings`,
    {
      gateway_base_url: BASE_URL,
      gateway_token: TOKEN,
      ...extra,
    },
    { token: daemon.token },
  );
  assert.equal(res.status, 200, res.text);
  return res;
}

/** Every `-u NAME` pair in an `env`-prefixed argv. */
function scrubbedNames(argv: string[]) {
  const out = new Set<string | undefined>();
  for (let i = 0; i < argv.length; i++) if (argv[i] === '-u') out.add(argv[i + 1]);
  return out;
}

/** Does `value` contain `needle` as a substring anywhere, at any depth? */
function leaksAnywhere(value: unknown, needle: string, seen = new Set<object>()): boolean {
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
  const forgedTrustWrite = await rawJsonPost(
    trust.port,
    '/api/settings',
    { gateway_base_url: BASE_URL },
    forgedHeaders,
  );
  assert.equal(
    forgedTrustWrite.status,
    401,
    `forged trusted headers must not waive the gateway bearer under trust mode: ${forgedTrustWrite.text}`,
  );

  // The real proxy (or anyone) presenting the bearer is accepted, in either mode.
  const trustWithBearer = await rawJsonPost(
    trust.port,
    '/api/settings',
    { gateway_base_url: BASE_URL },
    {
      ...forgedHeaders,
      authorization: `Bearer ${trust.token}`,
    },
  );
  assert.equal(
    trustWithBearer.status,
    200,
    `trust mode accepts the bearer: ${trustWithBearer.text}`,
  );

  const tokenWrite = await rawJsonPost(
    token.port,
    '/api/settings',
    { gateway_base_url: BASE_URL },
    forgedHeaders,
  );
  assert.equal(tokenWrite.status, 401, 'proxy token mode still requires the bearer');

  const authenticatedWrite = await rawJsonPost(
    token.port,
    '/api/settings',
    { gateway_base_url: BASE_URL },
    {
      ...forgedHeaders,
      authorization: `Bearer ${token.token}`,
    },
  );
  assert.equal(
    authenticatedWrite.status,
    200,
    `proxy token mode accepts its bearer: ${authenticatedWrite.text}`,
  );
});

test('gateway: the token is stored, usable, and never served back to a client', async (t) => {
  const { daemon } = await gatewayDaemon(t);
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
  ] as [string, unknown][]) {
    assert.equal(
      leaksAnywhere(payload, TOKEN),
      false,
      `${label} must not contain the gateway credential anywhere at any depth`,
    );
  }

  // …but it IS configured, and the board can tell.
  const gw = (fetched.json as SettingsResponse).settings.gateway;
  assert.equal(gw.token_set, true, 'token_set must report that a credential exists');
  assert.equal(gw.base_url, BASE_URL, 'the base URL is not secret — the board shows it');
  assert.equal(gw.ready, true, 'base_url + token ⇒ ready');
  assert.equal(gw.auth_style, 'bearer', 'bearer is the default auth style');
  assert.equal(gw.model_discovery, true, 'model discovery defaults on');
  assert.equal(gw.default, false, 'routing every spawn through the gateway is opt-in');
  assert.equal(Object.hasOwn(gw, 'token'), false, 'there must be no token field at all');

  assert.equal(
    (state.json as StateResponse).settings.gateway.token_set,
    true,
    'the snapshot carries the masked profile so the board can gate its toggle',
  );
});

test('gateway: a half-configured profile is not ready and refuses a spawn that asked for it', async (t) => {
  const { daemon } = await gatewayDaemon(t);
  t.after(() => daemon.stop());

  // A base URL with no credential would reach the proxy and 401 — which reads
  // as a Claude Code bug rather than a settings mistake. Refuse it up front.
  const res = await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_base_url: BASE_URL },
    { token: daemon.token },
  );
  assert.equal(res.status, 200, res.text);
  assert.equal(
    (res.json as SettingsResponse).settings.gateway.ready,
    false,
    'no token ⇒ not ready',
  );

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });
  assert.equal(spawn.status, 400, spawn.text);
  assert.match((spawn.json as ReasonResponse).reason, /not configured/i);
  assert.match(
    (spawn.json as ReasonResponse).reason,
    /gateway_token/,
    'the refusal must name the missing piece',
  );
});

test('gateway: settings validation refuses bad URLs, schemes and auth styles', async (t) => {
  const { daemon } = await gatewayDaemon(t);
  t.after(() => daemon.stop());

  const bad: [Record<string, string>, RegExp][] = [
    [{ gateway_base_url: 'not-a-url' }, /not a valid URL/i],
    [{ gateway_base_url: 'file:///etc/passwd' }, /http:\/\/ or https:\/\//i],
    [{ gateway_auth_style: 'basic' }, /bearer or api-key/i],
    [{ gateway_token: '' }, /non-empty string/i],
    [{ gateway_model_discovery: 'yes' }, /must be a boolean/i],
    [{ gateway_default: 'on' }, /must be a boolean/i],
    [{ gateway_token: 'x'.repeat(4097) }, /4096 characters or fewer/i],
    [{ gateway_token: 'tok en' }, /control characters/i],
    [{ gateway_base_url: 'http://gw.example.com' }, /control characters/i],
    // SECURITY (see validateGatewayBaseUrl): base_url is served UNMASKED to
    // every board over /state, so a credential spelled into it would ride that
    // public path. Both smuggling routes are refused at the door — url.href
    // preserves userinfo and query, so normalization would not have saved us.
    [{ gateway_base_url: 'https://user:hunter2@gw.example.com' }, /must not embed credentials/i],
    [
      { gateway_base_url: 'https://gw.example.com/?api_key=sekrit' },
      /must not carry a query string/i,
    ],
  ];
  for (const [body, re] of bad) {
    const res = await postJson(`${daemon.baseUrl}/api/settings`, body, { token: daemon.token });
    assert.equal(res.status, 400, `${JSON.stringify(body)} → ${res.text}`);
    assert.match((res.json as ReasonResponse).reason, re);
  }

  // validate-all-then-apply-all: one bad field must leave the store untouched.
  const mixed = await postJson(
    `${daemon.baseUrl}/api/settings`,
    {
      gateway_base_url: BASE_URL,
      gateway_auth_style: 'nonsense',
    },
    { token: daemon.token },
  );
  assert.equal(mixed.status, 400, mixed.text);
  const after = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal(
    (after.json as SettingsResponse).settings.gateway.base_url,
    null,
    'a rejected mixed body must not have half-applied the valid key',
  );

  // A trailing slash is normalized once, at the door, so /state and the injected
  // env can never disagree on spelling.
  await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_base_url: `${BASE_URL}/` },
    { token: daemon.token },
  );
  const normalized = await getJson(`${daemon.baseUrl}/api/settings`);
  assert.equal((normalized.json as SettingsResponse).settings.gateway.base_url, BASE_URL);
});

// ---------------------------------------------------------------- routing

test('gateway: a spawn that did not ask for one has all four variables scrubbed', async (t) => {
  const { daemon, record } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon); // configured, but this spawn does not opt in

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  assert.equal(spawn.status, 200, spawn.text);

  const [rec] = (await waitForSpecRecords(record, 1)) as SpecRecord[];
  assert.ok(rec, 'the spawn recorded its spec');
  const scrubbed = scrubbedNames(rec.parsed.argv);
  for (const name of GATEWAY_VARS) {
    assert.equal(
      scrubbed.has(name),
      true,
      `${name} must be scrubbed from a non-gateway pane — an ambient export in the daemon's shell must never reroute a session`,
    );
  }
  assert.equal(rec.parsed.gateway, false);
  assert.equal(rec.parsed.gateway_env, null);
});

test('gateway: gateway:true delivers the env and exempts exactly those names from the scrub', async (t) => {
  const { daemon, record } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });
  assert.equal(spawn.status, 200, spawn.text);

  const [rec] = (await waitForSpecRecords(record, 1)) as SpecRecord[];
  assert.ok(rec, 'the spawn recorded its spec');
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
    assert.equal(
      scrubbed.has(name),
      false,
      `${name} is being supplied via tmux -e, so the env -u prefix must not strip it back off`,
    );
  }
  // Only the supplied names are exempt: an ambient x-api-key credential is
  // still scrubbed from a bearer-style gateway pane.
  assert.equal(
    scrubbed.has('ANTHROPIC_API_KEY'),
    true,
    'a variable the launch did NOT supply stays scrubbed',
  );

  // The credential must not reach argv on any path.
  assert.equal(
    rec.parsed.argv.some((a) => a.includes(TOKEN)),
    false,
    'the credential must never appear in the pane argv',
  );
});

test('gateway: auth_style picks the header, so it picks the variable', async (t) => {
  const { daemon, record } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  // ANTHROPIC_API_KEY travels as x-api-key; ANTHROPIC_AUTH_TOKEN as
  // Authorization: Bearer. A credential in the wrong one 401s at the gateway.
  await configure(daemon, { gateway_auth_style: 'api-key', gateway_model_discovery: false });

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });

  const [rec] = (await waitForSpecRecords(record, 1)) as SpecRecord[];
  assert.ok(rec, 'the spawn recorded its spec');
  assert.deepEqual(
    rec.parsed.gateway_env,
    {
      ANTHROPIC_BASE_URL: BASE_URL,
      ANTHROPIC_API_KEY: TOKEN,
    },
    'api-key style sets ANTHROPIC_API_KEY, and discovery:false omits the flag entirely',
  );

  const scrubbed = scrubbedNames(rec.parsed.argv);
  assert.equal(
    scrubbed.has('ANTHROPIC_AUTH_TOKEN'),
    true,
    'the bearer variable is not supplied here, so it stays scrubbed',
  );
  assert.equal(
    scrubbed.has('CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY'),
    true,
    'discovery was turned off, so that flag is scrubbed rather than set',
  );
});

test('gateway: gateway_default routes a spawn that says nothing, and gateway:false still opts out', async (t) => {
  const { daemon, record } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon, { gateway_default: true });

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  await postJson(`${daemon.baseUrl}/api/spawn`, { cwd });
  const [silent] = (await waitForSpecRecords(record, 1)) as SpecRecord[];
  assert.ok(silent, 'the silent spawn recorded its spec');
  assert.equal(silent.parsed.gateway, true, 'silence defers to gateway_default');

  // An explicit false always wins over the default — the escape hatch for the
  // one session you want billed to Anthropic.
  await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: false });
  const recs = (await waitForSpecRecords(record, 2)) as SpecRecord[];
  const second = recs[1];
  assert.ok(second, 'the second spawn was recorded');
  assert.equal(second.parsed.gateway, false, 'gateway:false overrides gateway_default');
  assert.equal(scrubbedNames(second.parsed.argv).has('ANTHROPIC_BASE_URL'), true);
});

// ------------------------------------------------- remote-control conflict

test('gateway: remote control and the gateway are refused together, with a reason that explains why', async (t) => {
  const { daemon } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, {
    cwd,
    gateway: true,
    remote_control: true,
  });
  assert.equal(res.status, 400, res.text);
  assert.match(
    (res.json as ReasonResponse).reason,
    /remote control is unavailable on a gateway-routed session/i,
  );
  assert.match(
    (res.json as ReasonResponse).reason,
    /ANTHROPIC_BASE_URL/,
    'the refusal must say WHY — this is a Claude Code behaviour, not a Fleet Deck policy',
  );

  // The same collision via gateway_default rather than an explicit flag.
  await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_default: true },
    { token: daemon.token },
  );
  const viaDefault = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, remote_control: true });
  assert.equal(viaDefault.status, 400, viaDefault.text);
  assert.match((viaDefault.json as ReasonResponse).reason, /remote control is unavailable/i);

  // Either one alone is fine.
  await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_default: false },
    { token: daemon.token },
  );
  const rcOnly = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, remote_control: true });
  assert.equal(rcOnly.status, 200, rcOnly.text);
});

test('gateway: a non-boolean gateway flag is refused', async (t) => {
  const { daemon } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  const res = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: 'yes' });
  assert.equal(res.status, 400, res.text);
  assert.match((res.json as ReasonResponse).reason, /gateway must be a boolean/);
});

// ------------------------------------------------------------------ revive

test('gateway: routing survives death — a revive inherits the row, not the current default', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  // gateway_default STARTS ON so the flip below is a real state change. Without
  // this the "flip it off" POST is a no-op (the default is already false) and
  // the test silently proves less than its name claims.
  await configure(daemon, { gateway_default: true });

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  // A gateway-routed spawn, dead, then revived: the resumed pane must keep
  // talking to the same provider that produced the transcript it resumes.
  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: true } });
  await waitForSpecRecords(record, 1);

  // Flip the global default OFF. A revive must NOT consult it — it asks what
  // this lineage was doing, not what a new spawn would do.
  const flipped = await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_default: false },
    { token: daemon.token },
  );
  assert.equal(
    (flipped.json as SettingsResponse).settings.gateway.default,
    false,
    'sanity: the default really did change',
  );

  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {});
  assert.equal(revive.status, 200, revive.text);

  const recs = (await waitForSpecRecords(record, 2)) as SpecRecord[];
  const second = recs[1];
  assert.ok(second, 'the revive recorded a second spec');
  const resumed = second.parsed;
  assert.ok(resumed.argv.includes('--resume'), 'sanity: the second launch is a resume');
  assert.equal(resumed.gateway, true, "a revived pane must inherit its lineage's gateway routing");
  assert.equal(resumed.gateway_env?.ANTHROPIC_BASE_URL, BASE_URL);
  assert.equal(resumed.gateway_env.ANTHROPIC_AUTH_TOKEN, TOKEN);
  assert.equal(
    scrubbedNames(resumed.argv).has('ANTHROPIC_BASE_URL'),
    false,
    'the resume prefix must exempt the supplied gateway names, exactly like a fresh spawn',
  );
});

test('gateway: a lineage that never used the gateway is not rerouted by flipping the default on', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: false } });
  await waitForSpecRecords(record, 1);

  // Turning the default ON must not retroactively reroute an existing lineage:
  // gatewayDecision is handed a BOOLEAN from the row, never null, precisely so
  // this cannot consult gateway_default.
  await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_default: true },
    { token: daemon.token },
  );
  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {});
  assert.equal(revive.status, 200, revive.text);

  const recs = (await waitForSpecRecords(record, 2)) as SpecRecord[];
  const second = recs[1];
  assert.ok(second, 'the revive recorded a second spec');
  assert.equal(
    second.parsed.gateway,
    false,
    'flipping gateway_default on must not reroute a lineage that never used it',
  );
  assert.equal(
    scrubbedNames(second.parsed.argv).has('ANTHROPIC_BASE_URL'),
    true,
    'and its resumed pane keeps the full scrub',
  );
});

test('gateway: an explicit flag on the revive overrides what the row inherited', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });

  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: true } });
  await waitForSpecRecords(record, 1);

  const revive = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {
    gateway: false,
  });
  assert.equal(revive.status, 200, revive.text);
  const recs = (await waitForSpecRecords(record, 2)) as SpecRecord[];
  const second = recs[1];
  assert.ok(second, 'the revive recorded a second spec');
  assert.equal(
    second.parsed.gateway,
    false,
    'a human can move a lineage off the gateway on revive',
  );
});

// ------------------------------------------------------------------ clearing

test('gateway: clearing the token disarms the profile without forgetting the URL', async (t) => {
  const { daemon } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon);

  const cleared = await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_token: null },
    { token: daemon.token },
  );
  assert.equal(cleared.status, 200, cleared.text);
  assert.equal((cleared.json as SettingsResponse).settings.gateway.token_set, false);
  assert.equal(
    (cleared.json as SettingsResponse).settings.gateway.ready,
    false,
    'no credential ⇒ not spawnable',
  );
  assert.equal(
    (cleared.json as SettingsResponse).settings.gateway.base_url,
    BASE_URL,
    'clearing the credential must not also forget where the gateway lives',
  );

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, { cwd, gateway: true });
  assert.equal(spawn.status, 400, 'a disarmed profile must refuse, not silently bill Anthropic');
});

test('gateway: a revive stranded by a cleared token blames the settings, not the caller', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon);

  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const spawnId = await makeRevivable({ daemon, userHome, cwd, spawnBody: { gateway: true } });
  await waitForSpecRecords(record, 1);

  // The operator clears the credential. This revive INHERITED gateway:true from
  // its row — nobody asked for it on this call — so an error phrased as
  // "gateway:true was requested" would send them hunting for a bug in a request
  // they never made.
  await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_token: null },
    { token: daemon.token },
  );
  const stranded = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {});
  assert.equal(stranded.status, 400, stranded.text);
  assert.doesNotMatch(
    (stranded.json as ReasonResponse).reason,
    /was requested/,
    'the caller requested nothing — the flag came off the row',
  );
  assert.match((stranded.json as ReasonResponse).reason, /no longer configured/i);
  assert.match(
    (stranded.json as ReasonResponse).reason,
    /"gateway":false/,
    'the refusal must name the escape hatch, or the lineage reads as permanently stuck',
  );

  // And that escape hatch actually works.
  const rescued = await postJson(`${daemon.baseUrl}/api/spawn/${spawnId}/revive`, {
    gateway: false,
  });
  assert.equal(rescued.status, 200, rescued.text);
  const recs = (await waitForSpecRecords(record, 2)) as SpecRecord[];
  const second = recs[1];
  assert.ok(second, 'the rescue recorded a second spec');
  assert.equal(second.parsed.gateway, false);
});

// -------------------------------------------------------------------- adopt

test('gateway: adopt consults the default, because it has no lineage to inherit', async (t) => {
  const { daemon, record, userHome } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon, { gateway_default: true });

  // A session Fleet Deck never spawned: it registers by hook, so there is no
  // spawn row carrying a routing decision. The default is the only answer
  // available, which is exactly what a default is for.
  const cwd = scratchDir();
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true });
  });
  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    { session_id: sid, cwd, source: 'startup' },
    { token: daemon.token },
  );
  mkdirSync(path.dirname(claudeTranscriptPath(cwd, sid, userHome)), { recursive: true });
  writeFileSync(claudeTranscriptPath(cwd, sid, userHome), '{"type":"summary"}\n');
  // 'logout' is a hook-PROVEN end. NOT 'clear': that ends the session as
  // 'superseded' (the conversation continued under a new id), which is
  // deliberately never resumable — an earlier draft used it and the adopt was
  // refused before it could exercise anything.
  await postHook(
    daemon.baseUrl,
    'SessionEnd',
    { session_id: sid, cwd, reason: 'logout' },
    { token: daemon.token },
  );

  const card = ((await getJson(`${daemon.baseUrl}/state`)).json as StateResponse).sessions.find(
    (s) => s.session_id === sid,
  );
  assert.ok(card, 'sanity: the registered session has a card');
  assert.equal(card.adopt?.eligible, 'now', 'sanity: the session must actually be adoptable');
  assert.equal(card.spawn, undefined, 'sanity: no spawn row, so there is nothing to inherit');

  const adopt = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/adopt`, {});
  // Fail loudly rather than skip: a conditional here would let the one path with
  // INVERTED gateway semantics quietly go untested.
  assert.equal(
    adopt.status,
    200,
    `adopt did not launch, so its gateway rule went untested: ${adopt.text}`,
  );
  const [rec] = (await waitForSpecRecords(record, 1)) as SpecRecord[];
  assert.ok(rec, 'the spawn recorded its spec');
  assert.equal(rec.parsed.gateway, true, 'adopt honours gateway_default');
  assert.equal(rec.parsed.gateway_env?.ANTHROPIC_BASE_URL, BASE_URL);
});

// ---------------------------------------------------------------- repo mode

test('gateway: a repo-mode spawn persists and delivers routing too', async (t) => {
  const { daemon, record } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  await configure(daemon);

  // Repo mode uses a DIFFERENT insertProvisionalSpawn call site than cwd mode —
  // the one with origin_url / requested_branch / branch_mode populated, where a
  // positional bind drifting by one would silently write `gateway` into the
  // wrong column. Every other gateway test here is cwd-mode.
  const root = scratchDir();
  // Worktree mode materializes the checkout BESIDE the repo
  // (<root>--fd-gw-probe in repos.mjs materializeBranch), so removing only
  // root would strand the worktree directory and its admin record in tmp.
  // Remove it through git BEFORE root goes away (afterwards the repo that owns
  // the worktree metadata is gone), then prune in case anything still points
  // at it.
  const worktree = `${root}--fd-gw-probe`;
  t.after(() => {
    try {
      execFileSync('git', ['-C', root, 'worktree', 'remove', '--force', worktree]);
    } catch {
      /* best effort */
    }
    try {
      execFileSync('git', ['-C', root, 'worktree', 'prune']);
    } catch {
      /* best effort */
    }
    rmSync(root, { recursive: true, force: true });
  });
  execFileSync('git', ['init', '-q', root]);
  execFileSync('git', ['-C', root, 'commit', '-q', '--allow-empty', '-m', 'init'], {
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: 't',
      GIT_AUTHOR_EMAIL: 't@t',
      GIT_COMMITTER_NAME: 't',
      GIT_COMMITTER_EMAIL: 't@t',
    },
  });

  // Baseline before the spawn: the drain assertion below must prove this test
  // leaves NOTHING behind, not merely that it cleans up after itself. A
  // pre-existing sibling from an older, leakier run would make that proof
  // vacuous.
  assert.equal(
    readdirSync(tmpdir()).some((name) => name.endsWith('--fd-gw-probe')),
    false,
    'a --fd-gw-probe sibling from an earlier run is still in tmpdir; remove it before this test can prove its own teardown',
  );

  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: root,
    branch: 'gw-probe',
    branch_mode: 'worktree',
    gateway: true,
  });
  assert.equal(spawn.status, 200, spawn.text);

  // The drain probe. The t.after above is what makes it pass; without the
  // worktree removal there, the sibling checkout the daemon just created would
  // still be sitting in tmpdir at process exit.
  t.after(() => {
    assert.equal(
      existsSync(worktree),
      false,
      `teardown must not strand the repo-mode worktree ${worktree}`,
    );
  });

  const [rec] = (await waitForSpecRecords(record, 1)) as SpecRecord[];
  assert.ok(rec, 'the spawn recorded its spec');
  assert.equal(rec.parsed.gateway, true);
  assert.equal(rec.parsed.gateway_env?.ANTHROPIC_BASE_URL, BASE_URL);

  // The row is what a later revive reads, so prove the column — not just the
  // launch — actually carries the flag through the repo-mode insert.
  const card = ((await getJson(`${daemon.baseUrl}/state`)).json as StateResponse).sessions.find(
    (s) => s.session_id === (spawn.json as SpawnResponse).session_id,
  );
  assert.ok(card, 'sanity: the repo-mode spawn has a card');
  assert.equal(
    card.spawn?.gateway,
    true,
    'the snapshot field that drives the card chip must reflect the persisted column',
  );
  assert.equal(
    card.spawn.requested_branch,
    'gw-probe',
    'sanity: the neighbouring columns are still aligned',
  );
});

// ---------------------------------------------------------------------------
// UX 2.3 — the /api/spawn 500 guard. What used to escape spawn() as a bare
// {reason:'internal'} (http.mjs) is now either classified in spawns.mjs (the
// guarded repo-mode card-creation block) or bounded/redacted at the catch.
// ---------------------------------------------------------------------------

test('2.3: a credentialed origin URL reaches NO surface through the guarded card-creation path', async (t) => {
  // The one repo-mode failure that was invisible end-to-end: an accepted 202
  // clone whose request ECHOED a credential in the origin URL. origin_url is
  // the one spawns column never in the snapshot (snapshot.mjs: "it is the one
  // field that can carry credentials verbatim"), and the clone is shimmed to
  // fail instantly, so the tombstone note / fail_detail / durable event all
  // derive from git's stderr — which the daemon itself hardened. This pins
  // that the guarded synchronous path, the detached failure path AND the
  // snapshot gate agree: the credential is nowhere.
  const secret = 'glpat-DEADBEEFdeadbeef00';
  const origin = `https://fdtest:${secret}@127.0.0.1:1/x.git`;
  const { daemon } = await gatewayDaemon(t, {
    FLEETDECK_CLONE_TIMEOUT_MS: '1',
    GIT_SSH_COMMAND: 'false',
  });
  t.after(() => daemon.stop());

  const spawn = await postJson(`${daemon.baseUrl}/api/spawn`, {
    repo: origin,
    branch: 'main',
    branch_mode: 'in-place',
  });
  assert.equal(spawn.status, 202, spawn.text);

  // The tombstone lands offline (the clone dies on the 1ms timeout / false
  // ssh). The WHOLE snapshot — note, ticker, fail_detail — must be clean.
  const deadline = Date.now() + 12_000;
  let card: SessionCard | null | undefined = null;
  while (Date.now() < deadline) {
    const state = await getJson(`${daemon.baseUrl}/state`);
    assert.equal(
      state.text.includes(secret),
      false,
      'the credential must appear NOWHERE in /state',
    );
    assert.equal(state.text.includes('fdtest:'), false, 'nor the userinfo it sat in');
    card = (state.json as StateResponse).sessions.find(
      (s) => s.session_id === (spawn.json as SpawnResponse).session_id,
    );
    if (card?.col === 'offline') break;
    await new Promise((r) => setTimeout(r, 100));
  }
  assert.equal(card?.col, 'offline', 'the failed clone tombstones the card');
  assert.match(card.note ?? '', /spawn failed:/);
  assert.equal((card.note ?? '').includes(secret), false, 'the tombstone note is hardened');
});

// ------------------------------------------------------------------ lifecycle

test('gateway: helper scratch trees (record dir + user home) are torn down after the test', async (t) => {
  // BUG-163: gatewayDaemon() allocated two credential-bearing scratch trees
  // per call (the spawn-spec record dir and the fabricated user HOME) and
  // returned no cleanup owner for either — daemon.stop() removes only the
  // daemon's own home, so every helper invocation leaked both. Boot one
  // daemon, note the paths, and assert they are gone once this test's
  // t.after hooks have run.
  const { daemon, record, userHome } = await gatewayDaemon(t);
  t.after(() => daemon.stop());
  const recordDir = path.dirname(record);
  assert.ok(existsSync(recordDir));
  assert.ok(existsSync(userHome));
  t.after(() => {
    assert.equal(
      existsSync(recordDir),
      false,
      'the record scratch dir must be removed after the test',
    );
    assert.equal(
      existsSync(userHome),
      false,
      'the user-home scratch dir must be removed after the test',
    );
  });
});
