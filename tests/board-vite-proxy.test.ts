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

import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';

// board/vite.config.js imports 'vite' and '@vitejs/plugin-react' from
// board/node_modules — a separate dependency tree the root install does not
// create — so the module cannot simply be imported here. The proxy target
// config is plain data, so we evaluate the shipped config SOURCE directly with
// identity/no-op stand-ins for those two imports (defineConfig is identity,
// react() returns a stub plugin). Evaluating the source is byte-faithful to the
// shipped file — the same pattern board-util.test.ts uses on TermPane — and runs
// identically on Node and Bun, needing neither ESM loader hooks (which Bun
// ignores) nor import-cache busting (which Bun does not honour on a ?query).
const CONFIG_SRC = readFileSync(path.resolve('board/vite.config.js'), 'utf8');
// Drop the two bare-package import lines and turn `export default X` into a
// return, so the ESM config body evaluates as a plain function, fresh per port.
const CONFIG_BODY = CONFIG_SRC.replace(/^import[^\n]*\n/gm, '').replace(
  /export default /,
  'return ',
);

interface ViteProxyEntry {
  target: string;
}
interface ViteConfig {
  server: { proxy: Record<string, ViteProxyEntry> };
}

type DefineConfig = (config: ViteConfig) => ViteConfig;
type ReactStub = () => { name: string };

// The config reads process.env.FLEETDECK_PORT when its body runs, so each case
// re-evaluates a fresh copy with the variable set — or deleted — around the call.
function importConfigWithPort(port: string | undefined): ViteConfig {
  const saved = process.env['FLEETDECK_PORT'];
  if (port === undefined) delete process.env['FLEETDECK_PORT'];
  else process.env['FLEETDECK_PORT'] = port;
  try {
    const defineConfig: DefineConfig = (config) => config;
    const react: ReactStub = () => ({ name: 'react-stub' });
    // The slice is the repo's own committed config — no user input. new Function
    // evaluates it as a plain body; process is the real global it reads env from.
    // eslint-disable-next-line @typescript-eslint/no-implied-eval
    const factory = new Function('defineConfig', 'react', CONFIG_BODY) as (
      d: DefineConfig,
      r: ReactStub,
    ) => ViteConfig;
    return factory(defineConfig, react);
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
