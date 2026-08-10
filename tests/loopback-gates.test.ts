// tests/loopback-gates.test.ts
//
// 0.16.0 — token provisioning + the gated loopback powers. The daemon now
// mints $FLEETDECK_HOME/token on EVERY boot (default loopback included), and
// the powers a malicious local process / a fleet agent must not wield
// anonymously require it even with FLEETDECK_REQUIRE_TOKEN off: /ws/term,
// POST /mail, gateway_* settings writes, and the unsupervised arm. Ordinary
// loopback routes stay open. 0.16.1 adds an explicit plain-loopback opt-out for
// those four powers without weakening hook authentication.

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import {
  readFileSync,
  existsSync,
  mkdtempSync,
  rmSync,
  statSync,
  writeFileSync,
  chmodSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { WebSocket } from 'ws';
import { randomPort, spawnRaw, startDaemon } from './helpers/daemon.ts';
import { postJson, getJson } from './helpers/http.ts';

type WsOutcome = 'timeout' | 'opened' | 'refused';

function scratchHome(): string {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-loopback-gates-'));
}

function wsTermAttempt(url: string): Promise<WsOutcome> {
  return new Promise((resolve) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => {
      ws.terminate();
      resolve('timeout');
    }, 3000);
    ws.on('open', () => {
      clearTimeout(timer);
      ws.close();
      resolve('opened');
    });
    ws.on('error', () => {
      clearTimeout(timer);
      resolve('refused');
    });
  });
}

test('the token is minted 0600 on every boot, and the file matches the daemon', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const file = path.join(daemon.home, 'token');
  assert.ok(existsSync(file), 'token file exists in default loopback mode');
  const mode = statSync(file).mode & 0o777;
  assert.equal(mode, 0o600, 'token file is owner-only');
  assert.equal(readFileSync(file, 'utf8').trim(), daemon.token, 'handle surfaces the same token');
  assert.ok(daemon.token && daemon.token.length >= 32, 'token has real entropy');
});

test('a matching preexisting token file is tightened to 0600', async (t: TestContext) => {
  // BUG-117: an operator may preprovision the correct token — e.g. with the
  // documented FLEETDECK_TOKEN contract in mind — at a permissive mode. The
  // daemon used to chmod only on (re)write, so a matching 0644 file stayed
  // readable by other local accounts. Boot must tighten it unconditionally.
  const home = scratchHome();
  const file = path.join(home, 'token');
  writeFileSync(file, '0123456789abcdef0123456789abcdef', { encoding: 'utf8' });
  chmodSync(file, 0o644);
  const daemon = await startDaemon({ home });
  t.after(async () => {
    await daemon.stop({ keepHome: true });
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const mode = statSync(file).mode & 0o777;
  assert.equal(mode, 0o600, 'preexisting 0644 token file is tightened to owner-only');
  assert.equal(
    readFileSync(file, 'utf8').trim(),
    daemon.token,
    'token content is unchanged by the tightening',
  );
});

test('gateway_* settings writes require the bearer; plain settings keys stay open', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  // gateway_* keys: 401 without a token.
  const bare = await postJson(`${daemon.baseUrl}/api/settings`, {
    gateway_base_url: 'https://gateway.example.com',
  });
  assert.equal(bare.status, 401, 'tokenless gateway write must 401');

  // With the token: the normal validation path runs (and a valid value saves).
  const authed = await postJson(
    `${daemon.baseUrl}/api/settings`,
    { gateway_base_url: 'https://gateway.example.com' },
    { token: daemon.token },
  );
  assert.equal(
    authed.status,
    200,
    `authenticated gateway write succeeds: ${JSON.stringify(authed.json)}`,
  );

  // A non-gateway key keeps the loopback exemption.
  const plain = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: daemon.home });
  assert.equal(plain.status, 200, 'browse_root still open on loopback');
});

test('/ws/term refuses a tokenless loopback client and accepts the bearer', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const bare = await wsTermAttempt(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`);
  assert.equal(bare, 'refused', 'tokenless /ws/term must be refused at upgrade');

  const authed = await wsTermAttempt(
    `ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever&t=${String(daemon.token)}`,
  );
  // The upgrade itself must succeed; the spawn id is bogus so the bridge will
  // close us right after — 'opened' is the assertion, not a long-lived socket.
  assert.equal(authed, 'opened', 'bearer /ws/term passes the upgrade gate');
});

test('TRUST_LOOPBACK=on waives the four power gates only for plain loopback', async (t: TestContext) => {
  const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'on' } });
  t.after(() => daemon.stop());

  const term = await wsTermAttempt(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`);
  assert.equal(term, 'opened', 'tokenless /ws/term upgrade opens');

  const mail = await postJson(`${daemon.baseUrl}/mail`, {
    to: 'all',
    from: 'operator',
    text: 'trusted loopback',
  });
  assert.equal(mail.status, 200, `tokenless POST /mail opens: ${mail.text}`);

  const gateway = await postJson(`${daemon.baseUrl}/api/settings`, {
    gateway_base_url: 'https://gateway.example.com',
  });
  assert.equal(gateway.status, 200, `tokenless gateway_* write opens: ${gateway.text}`);

  const arm = await postJson(`${daemon.baseUrl}/api/spawn/arm-unsupervised`, {});
  assert.equal(arm.status, 200, `tokenless unsupervised arm opens: ${arm.text}`);
  const armBody = arm.json as { arm_token?: unknown };
  assert.ok(typeof armBody.arm_token === 'string' && armBody.arm_token, 'arm capability is minted');

  // Hook authentication cannot be opted out. Legacy tokenless hooks retain
  // their fail-open HTTP dialect, but are refused before any handler runs.
  const hook = await postJson(`${daemon.baseUrl}/hook/SessionStart`, {
    session_id: 'trust-loopback-forgery',
  });
  assert.equal(hook.status, 200, 'legacy hook refusal stays fail-open at HTTP level');
  const hookBody = hook.json as {
    ok?: unknown;
    hookSpecificOutput?: { additionalContext?: string };
  };
  assert.equal(hookBody.ok, undefined, 'tokenless hook was not authenticated');
  assert.match(hookBody.hookSpecificOutput?.additionalContext ?? '', /restart/i);
});

test('TRUST_LOOPBACK refuses contradictory and invalid startup configuration', async (t: TestContext) => {
  const cases: { name: string; env: Record<string, string>; message: RegExp }[] = [
    {
      name: 'REQUIRE_TOKEN=on',
      env: { FLEETDECK_TRUST_LOOPBACK: 'on', FLEETDECK_REQUIRE_TOKEN: 'on' },
      message: /FLEETDECK_TRUST_LOOPBACK=on conflicts with FLEETDECK_REQUIRE_TOKEN=on/,
    },
    {
      name: 'LAN bind',
      env: { FLEETDECK_TRUST_LOOPBACK: 'on', FLEETDECK_BIND: '0.0.0.0' },
      message: /FLEETDECK_TRUST_LOOPBACK=on requires a loopback FLEETDECK_BIND/,
    },
    {
      name: 'invalid value',
      env: { FLEETDECK_TRUST_LOOPBACK: 'sometimes' },
      message: /FLEETDECK_TRUST_LOOPBACK must be 'on' or 'off'/,
    },
  ];

  for (const entry of cases) {
    await t.test(entry.name, async () => {
      const home = scratchHome();
      const raw = spawnRaw({ port: randomPort(), home, env: entry.env });
      try {
        const code = await raw.waitForExit();
        assert.notEqual(code, 0, 'contradictory configuration must not start');
        assert.match(raw.stderr, entry.message);
      } finally {
        await raw.kill();
        rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      }
    });
  }
});

test('ordinary loopback routes keep the historical exemption', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  assert.equal((await getJson(`${daemon.baseUrl}/state`)).status, 200, '/state open');
  assert.equal((await getJson(`${daemon.baseUrl}/health`)).status, 200, '/health open');
  assert.equal((await getJson(`${daemon.baseUrl}/api/settings`)).status, 200, 'GET settings open');
  const cleanup = await postJson(`${daemon.baseUrl}/api/cleanup`, {});
  assert.equal(cleanup.status, 200, 'POST /api/cleanup open');
});

// BUG-186 — /health must SAY whether a tokenless caller can upgrade /ws/term,
// because the board's pre-frame close diagnosis keys off it (board/src/
// termDiag.js). Under a mode that waives the key, "you need a key" is a false
// diagnosis — the real fault is the proxy's upgrade forwarding or the
// transport. Each assertion pairs the capability with the ground truth it
// claims: an actual tokenless upgrade attempt.
test('/health auth.term_token mirrors the /ws/term gate under every trust mode', async (t: TestContext) => {
  await t.test('default loopback: gated, and the upgrade really refuses', async () => {
    const daemon = await startDaemon();
    t.after(() => daemon.stop());

    const health = await getJson(`${daemon.baseUrl}/health`);
    const body = health.json as { auth?: { term_token?: boolean } };
    assert.equal(body.auth?.term_token, true, 'default loopback gates /ws/term on the key');
    assert.equal(
      await wsTermAttempt(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`),
      'refused',
      'the capability must not contradict the gate it describes',
    );
  });

  await t.test('TRUST_LOOPBACK=on: waived, and the upgrade really opens', async () => {
    const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'on' } });
    t.after(() => daemon.stop());

    const health = await getJson(`${daemon.baseUrl}/health`);
    const body = health.json as { auth?: { term_token?: boolean } };
    assert.equal(body.auth?.term_token, false, 'trust-loopback waives the /ws/term key');
    assert.equal(
      await wsTermAttempt(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`),
      'opened',
      'a tokenless upgrade must open when the capability says it can',
    );
  });

  await t.test(
    'PROXY_AUTH=trust: waived, and a proxied upgrade really opens tokenless',
    async () => {
      const PROXY_HOST = 'fd.example.com';
      const daemon = await startDaemon({
        env: {
          FLEETDECK_TRUSTED_ORIGINS: `https://${PROXY_HOST}`,
          FLEETDECK_PROXY_AUTH: 'trust',
        },
      });
      t.after(() => daemon.stop());

      const health = await getJson(`${daemon.baseUrl}/health`);
      const body = health.json as { auth?: { term_token?: boolean } };
      assert.equal(body.auth?.term_token, false, 'proxy-trust waives the /ws/term key');
      // The tokenless browser the bug is about: arriving THROUGH the trusted
      // proxy (its Host), the upgrade opens without any ?t=.
      const proxied = await new Promise<WsOutcome>((resolve) => {
        const ws = new WebSocket(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`, {
          headers: { host: PROXY_HOST, origin: `https://${PROXY_HOST}` },
        });
        const timer = setTimeout(() => {
          ws.terminate();
          resolve('timeout');
        }, 3000);
        ws.on('open', () => {
          clearTimeout(timer);
          ws.close();
          resolve('opened');
        });
        ws.on('error', () => {
          clearTimeout(timer);
          resolve('refused');
        });
      });
      assert.equal(
        proxied,
        'opened',
        'the trusted-proxy browser needs no key — the diagnosis must not claim it does',
      );
    },
  );
});
