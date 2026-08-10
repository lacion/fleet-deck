// tests/network-refresh.test.ts
//
// BUG-129: the HTTP Host allowlist and the share panel's LAN URLs were built
// from ONE startup snapshot of os.networkInterfaces(). A long-lived daemon on
// a roaming host (Wi-Fi change, DHCP renewal, VPN up/down) then rejected the
// board's own NEW address as a DNS-rebinding attempt and kept showing dead
// share URLs until restart. The fix refreshes the allowlist from the interface
// list on every checked request and hands the daemon a refreshLan() handle that
// swaps the share URLs atomically.
//
// os.networkInterfaces() cannot be monkey-patched after import (Node caches
// the builtin binding), so this suite swaps the whole `node:os` module for a
// mutable facade via module.register() — the same pattern as the daemon-spawn
// loader tests, without a child process.

import { register } from 'node:module';

register(new URL('./helpers/os-facade-loader.ts', import.meta.url));

interface NetIface {
  family: string;
  internal: boolean;
  address: string;
}

const { __setInterfaces } = (await import('node:os')) as unknown as {
  __setInterfaces: (entries: NetIface[]) => void;
};
const { openDb } = await import('../scripts/fleetd/db.ts');
const { createCore } = await import('../scripts/fleetd/derive.ts');
const { createHttp } = await import('../scripts/fleetd/http.ts');

import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

const LOOPBACK_ONLY = [{ family: 'IPv4', internal: true, address: '127.0.0.1' }];
const ON_LAN = [...LOOPBACK_ONLY, { family: 'IPv4', internal: false, address: '192.0.2.10' }];

// One raw request with a fully controlled Host header. fetch() forbids setting
// Host from script, and Host is the wall under test. Never rejects on non-2xx.
function raw(
  port: number,
  {
    method = 'GET',
    path: reqPath = '/',
    headers = {},
  }: { method?: string; path?: string; headers?: Record<string, string> } = {},
) {
  return new Promise<{ status: number | undefined; body: string }>((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: reqPath, method, headers }, (res) => {
      let body = '';
      res.on('data', (d: Buffer) => {
        body += d.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, body });
      });
    });
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    req.end();
  });
}

interface LanArg {
  enabled?: boolean;
  urls?: string[];
  mdns?: string | null;
}
type BoardHandle = ReturnType<typeof createHttp> & { port: number };

function startBoard(t: TestContext, { lan = null }: { lan?: LanArg | null } = {}) {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home' });
  // The Host wall pins the Host header's port to the daemon's configured port,
  // so bind FIRST and hand createHttp the real port — passing 0 would make
  // every request's `Host: 127.0.0.1:<actual>` read as foreign (this is a
  // test-harness constraint; fleetd always knows its port before listening).
  const probe = http.createServer();
  return new Promise<BoardHandle>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        const handle = createHttp(core, { port, token: null as unknown as string, lan });
        handle.server.once('error', reject);
        handle.server.listen(port, '127.0.0.1', () => {
          t.after(() => {
            handle.server.close();
            db.close();
          });
          resolve({ ...handle, port });
        });
      });
    });
  });
}

test('the Host allowlist follows the interface list as the network changes', async (t) => {
  __setInterfaces(LOOPBACK_ONLY); // booted before the LAN came up
  const { port } = await startBoard(t);
  const lanHost = { host: `192.0.2.10:${port}` };

  const refused = await raw(port, { path: '/state', headers: lanHost });
  assert.equal(
    refused.status,
    403,
    'an address the host does NOT have yet is a stranger and must stay refused',
  );

  // Wi-Fi associates / DHCP grants the lease: the board is now reachable at
  // 192.0.2.10 — the daemon must recognize its own new address immediately,
  // not after a restart.
  __setInterfaces(ON_LAN);
  const allowed = await raw(port, { path: '/state', headers: lanHost });
  assert.equal(
    allowed.status,
    200,
    'a request via the host’s NEW LAN address must pass the Host wall',
  );

  // The VPN drops: the address is gone again and stops being "us" at once —
  // a rebind window must not outlive the interface.
  __setInterfaces(LOOPBACK_ONLY);
  const stale = await raw(port, { path: '/state', headers: lanHost });
  assert.equal(stale.status, 403, 'a withdrawn LAN address must leave the allowlist');
});

test('refreshLan() swaps the share-panel URLs in one snapshot', async (t) => {
  __setInterfaces(LOOPBACK_ONLY);
  const { port, refreshLan } = await startBoard(t, {
    lan: {
      enabled: true,
      urls: ['http://192.0.2.10:9/?t=old'],
      mdns: 'http://fleetdeck.local:9/?t=old',
    },
  });
  const lanOf = async () => (await raw(port, { path: '/state' })).body;

  assert.match(await lanOf(), /192\.0\.2\.10:9/, 'the startup share URL is served at first');

  // The daemon's network poll hands us the address set the host has NOW.
  refreshLan({
    enabled: true,
    urls: ['http://198.51.100.20:9/?t=new'],
    mdns: 'http://fleetdeck.local:9/?t=new',
  });
  const after = await lanOf();
  assert.match(after, /198\.51\.100\.20:9/, 'the share panel must show the NEW address');
  assert.doesNotMatch(after, /192\.0\.2\.10:9/, 'the stale share URL must be gone');

  refreshLan({ enabled: false, urls: [] });
  assert.doesNotMatch(
    await lanOf(),
    /198\.51\.100\.20/,
    'losing the LAN entirely collapses the panel to local-only',
  );
});

test('the per-request allowlist refresh never evicts the advertised .local name', async (t) => {
  // The mDNS hostname is standing allowlist data, not interface data: the
  // refresh clears and rebuilds the address set per request, so it must re-add
  // the name every time — otherwise the first checked request via the mDNS URL
  // 403s as a DNS-rebinding attempt (regression caught during BUG-129 verify).
  __setInterfaces(LOOPBACK_ONLY);
  const { port } = await startBoard(t, {
    lan: { enabled: true, urls: [], mdns: `http://fleetdeck.local:9/?t=x` },
  });
  const mdnsHost = { host: `fleetdeck.local:${port}` };

  const first = await raw(port, { path: '/state', headers: mdnsHost });
  assert.equal(
    first.status,
    200,
    'the .local URL is the advertised entry point — its FIRST request must pass',
  );
  const second = await raw(port, { path: '/state', headers: mdnsHost });
  assert.equal(
    second.status,
    200,
    'a refresh triggered by any earlier request must not evict the .local name',
  );

  // And the refresh still happens: an address the host gains is recognized...
  __setInterfaces(ON_LAN);
  const gained = await raw(port, { path: '/state', headers: { host: `192.0.2.10:${port}` } });
  assert.equal(gained.status, 200, 'a newly gained LAN address passes');
  // ...while the .local name survives that refresh too.
  const third = await raw(port, { path: '/state', headers: mdnsHost });
  assert.equal(third.status, 200, 'the .local name survives an interface-set change');
});
