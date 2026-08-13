// tests/lan-mdns-state.test.ts
//
// BUG-122: the share panel must stop advertising the .local URL once the mDNS
// responder is down. fleetd hands createHttp a lan THUNK whose mdns field is
// nulled whenever mdns.alive() is false; /state must therefore resolve lan per
// snapshot, not freeze the boot-time advertisement. These tests drive a real
// createHttp server in-process (loopback /state needs no token) and flip the
// thunk between requests — no multicast, no real responder, no subprocess.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createHttp } from '../src/daemon/http.ts';
import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { randomPort } from './helpers/daemon.ts';
import type { StateResponse } from '../contracts/state.ts';

// The daemon's server is the Bun.serve shim createHttp returns, not node's http
// Server — derive its type from the daemon surface (as LanArg does below) rather
// than restate it.
type HttpServer = ReturnType<typeof createHttp>['server'];

// createHttp's `lan` wiring accepts a static source or a per-snapshot thunk;
// derive the exact accepted type from the daemon's own surface rather than
// restate it (and strip the null/undefined these tests never pass).
type LanArg = NonNullable<NonNullable<Parameters<typeof createHttp>[1]>['lan']>;

// The /state view of lan is looser than the contract union at the read site:
// these tests probe `.mdns` on both the enabled and disabled branches, so keep
// it a single optional field here rather than the discriminated `Lan` union.
interface LanState {
  enabled: boolean;
  urls: string[];
  mdns?: string | null;
}

const MDNS_URL = 'http://fleetdeck.local:4711/?t=sekret';
const IP_URL = 'http://192.0.2.7:4711/?t=sekret';

function listen(server: HttpServer, port: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => {
      resolve();
    });
  });
}

async function withStateServer(
  lan: LanArg,
  run: (baseUrl: string) => Promise<void>,
): Promise<void> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-lan-mdns-'));
  const db = openDb(path.join(home, 'fleetd.db'));
  const port = randomPort();
  // createHttp's `port` is the advertised daemon port: the Host allowlist
  // compares the request's authority against it, so it must be the port this
  // test server actually listens on.
  const core = createCore(db, { port, version: '0.0.0-test' });
  const { server } = createHttp(core, { port, token: 'sekret', lan, version: '0.0.0-test' });
  await listen(server, port);
  try {
    await run(`http://127.0.0.1:${port}`);
  } finally {
    await new Promise<void>((resolve) => {
      server.close(() => {
        resolve();
      });
    });
    try {
      db.close();
    } catch {
      /* best effort */
    }
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  }
}

const stateAt = async (baseUrl: string): Promise<LanState> => {
  const res = await fetch(`${baseUrl}/state`);
  const body = (await res.json()) as StateResponse;
  return body.lan;
};

test('/state reflects a lan THUNK per snapshot: mdns appears while alive and vanishes once the responder stands down', async () => {
  let mdnsAlive = true;
  // The exact shape fleetd.mjs installs: drop mdns from the advertisement the
  // moment the responder is no longer alive.
  const lan = () => ({
    enabled: true,
    urls: [IP_URL],
    mdns: mdnsAlive ? MDNS_URL : null,
  });

  await withStateServer(lan, async (baseUrl) => {
    const live = await stateAt(baseUrl);
    assert.equal(live.mdns, MDNS_URL, 'a live responder keeps its .local URL in the share panel');
    assert.deepEqual(live.urls, [IP_URL]);

    mdnsAlive = false; // the responder disabled itself after start()
    const down = await stateAt(baseUrl);
    assert.equal(
      down.mdns,
      null,
      'a dead responder must not keep advertising a URL that cannot resolve',
    );
    assert.deepEqual(down.urls, [IP_URL], 'the IP URLs remain — the board still works over its IP');
    assert.equal(down.enabled, true);
  });
});

test('/state still accepts a plain lan OBJECT (the historical shape), including LAN-off', async () => {
  await withStateServer({ enabled: true, urls: [IP_URL], mdns: MDNS_URL }, async (baseUrl) => {
    const lan = await stateAt(baseUrl);
    assert.equal(lan.mdns, MDNS_URL);
    assert.deepEqual(lan.urls, [IP_URL]);
  });

  await withStateServer({ enabled: false, urls: [] }, async (baseUrl) => {
    const lan = await stateAt(baseUrl);
    assert.deepEqual(lan, { enabled: false, urls: [] }, 'LAN-off boards keep the local-only shape');
  });
});
