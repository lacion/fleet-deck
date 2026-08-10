// tests/board-vite-proxy.test.mjs
//
// BUG-004 — the documented scratch-board workflow proxied into the live fleet.
//
// CONTRIBUTING.md tells a contributor to run a scratch daemon on port 4712 so
// they don't collide with their real fleet on 4711 — but board/vite.config.js
// hardcoded every proxied HTTP and WebSocket target to 4711. A contributor who
// followed the guide ran `npm run dev` and the dev board read and mutated the
// REAL fleet (mail, commands, API writes, terminal input) while they believed
// it was isolated.
//
// The fix derives the proxy target from FLEETDECK_PORT — the same variable the
// daemon itself reads — defaulting to 4711. These tests import the real config
// (with 'vite' / '@vitejs/plugin-react' stubbed by helpers/vite-stub-loader.mjs,
// since board/node_modules is a separate tree the root install doesn't create)
// and pin the proxy targets for both the default and the scratch port.

import { register } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import assert from 'node:assert/strict';

register(path.resolve('tests/helpers/vite-stub-loader.mjs'), pathToFileURL(process.cwd() + '/'));

const CONFIG_URL = pathToFileURL(path.resolve('board/vite.config.js')).href;

interface ViteProxyEntry {
  target: string;
}
interface ViteConfig {
  server: { proxy: Record<string, ViteProxyEntry> };
}

// The config reads process.env.FLEETDECK_PORT at import time, so each case
// imports a fresh copy (cache-busting query) with the variable set — or
// deleted — around the import.
async function importConfigWithPort(port: string | undefined): Promise<ViteConfig> {
  const saved = process.env['FLEETDECK_PORT'];
  if (port === undefined) delete process.env['FLEETDECK_PORT'];
  else process.env['FLEETDECK_PORT'] = port;
  try {
    const mod = (await import(`${CONFIG_URL}?port=${port ?? 'default'}`)) as {
      default: ViteConfig;
    };
    return mod.default;
  } finally {
    if (saved === undefined) delete process.env['FLEETDECK_PORT'];
    else process.env['FLEETDECK_PORT'] = saved;
  }
}

function proxyTargets(config: ViteConfig): Record<string, string> {
  return Object.fromEntries(
    Object.entries(config.server.proxy).map(([route, entry]) => [route, entry.target]),
  );
}

test('vite proxy targets follow FLEETDECK_PORT so the dev board hits the scratch daemon', async () => {
  const config = await importConfigWithPort('4712');
  const targets = proxyTargets(config);
  for (const route of ['/state', '/health', '/mail', '/command', '/api']) {
    assert.equal(
      targets[route],
      'http://127.0.0.1:4712',
      `${route} must proxy to the scratch daemon`,
    );
  }
  assert.equal(
    targets['/ws'],
    'ws://127.0.0.1:4712',
    '/ws (and /ws/term) must upgrade against the scratch daemon',
  );
  // The Origin rewrite must claim to be the SAME daemon the request goes to,
  // or fleetd's C1 gate 403s it.
  const stateProxy = config.server.proxy['/state'];
  assert.ok(stateProxy, 'the /state proxy entry exists');
  assert.equal(stateProxy.target.replace('http', 'ws'), targets['/ws']);
});

test('vite proxy targets default to 4711 when FLEETDECK_PORT is unset', async () => {
  const config = await importConfigWithPort(undefined);
  const targets = proxyTargets(config);
  for (const route of ['/state', '/health', '/mail', '/command', '/api']) {
    assert.equal(
      targets[route],
      'http://127.0.0.1:4711',
      `${route} must default to the standard daemon port`,
    );
  }
  assert.equal(targets['/ws'], 'ws://127.0.0.1:4711');
});
