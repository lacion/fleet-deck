// Test-only ESM loader for fleetd-audit-regressions.test.mjs.
//
// WHY a loader instead of a real multicast listener: CI hosts legitimately lack
// multicast routes or have avahi bound to 5353, while repository sandboxes may
// reject every TCP bind. Replacing dgram, fleetd's HTTP factory and fleetd's OS
// interface view lets child-process lifecycle tests run without weakening the
// production modules or making the regressions environment-shaped.

const MOCK_URL = 'fleetdeck-test:mdns-dgram';
const HTTP_URL = 'fleetdeck-test:fleetd-http';
const OS_URL = 'fleetdeck-test:fleetd-os';

export async function resolve(specifier, context, nextResolve) {
  if (specifier === 'node:dgram' && context.parentURL?.endsWith('/scripts/fleetd/mdns.mjs')) {
    return { url: MOCK_URL, shortCircuit: true };
  }
  if (specifier === './http.mjs' && context.parentURL?.endsWith('/scripts/fleetd/fleetd.mjs')) {
    return { url: HTTP_URL, shortCircuit: true };
  }
  if (specifier === 'node:os' && (context.parentURL?.endsWith('/scripts/fleetd/fleetd.mjs') || context.parentURL?.endsWith('/scripts/fleetd/mdns.mjs'))) {
    return { url: OS_URL, shortCircuit: true };
  }
  return nextResolve(specifier, context);
}

export async function load(url, context, nextLoad) {
  if (url === MOCK_URL) {
    return {
      format: 'module',
      shortCircuit: true,
      source: `
      import { appendFileSync } from 'node:fs';

      const recordFile = process.env.FLEETDECK_MDNS_RECORD;
      const delay = Number(process.env.FLEETDECK_MDNS_SEND_DELAY_MS || 150);
      function record(value) {
        if (!recordFile) return;
        appendFileSync(recordFile, JSON.stringify({ ...value, at: Date.now() }) + '\\n');
      }

      let nextId = 0;
      class MockSocket {
        constructor() { this.id = nextId++; }
        on() { return this; }
        // FLEETDECK_MDNS_FAIL_TTL=unicast|multicast simulates a platform that
        // refuses the corresponding setsockopt, so tests can drive the
        // responder's TTL degradation path.
        setTTL(value) {
          record({ type: 'setTTL', value });
          if (process.env.FLEETDECK_MDNS_FAIL_TTL === 'unicast') throw new Error('mock: IP_TTL refused');
        }
        setMulticastTTL(value) {
          record({ type: 'setMulticastTTL', value });
          if (process.env.FLEETDECK_MDNS_FAIL_TTL === 'multicast') throw new Error('mock: IP_MULTICAST_TTL refused');
        }
        setMulticastLoopback() {}
        setMulticastInterface(address) { record({ type: 'setiface', id: this.id, address }); }
        // BUG-122 regression seam: FLEETDECK_MDNS_JOIN_FAILS=1 models a network
        // with no multicast route — every membership join fails and the
        // responder must terminally disable itself after bind.
        addMembership(_group, address) {
          record({ type: 'join', id: this.id, address: address || null });
          if (process.env.FLEETDECK_MDNS_JOIN_FAILS) throw new Error('no multicast route');
        }
        bind(_options, callback) { setImmediate(callback); return this; }
        send(packet, _port, address, callback = () => {}) {
          const wire = Buffer.from(packet).toString('base64');
          record({ type: 'send', id: this.id, wire, address });
          const timer = setTimeout(() => {
            record({ type: 'callback', id: this.id, wire });
            callback();
          }, delay);
          timer.unref?.();
        }
        close(callback) { setImmediate(() => callback?.()); }
      }

      export default { createSocket: () => new MockSocket() };
    `,
    };
  }
  if (url === HTTP_URL) {
    return {
      format: 'module',
      shortCircuit: true,
      source: `
        import { EventEmitter } from 'node:events';
        import { appendFileSync } from 'node:fs';
        const consoleRecord = process.env.FLEETDECK_TEST_CONSOLE_RECORD;
        if (consoleRecord) {
          const originalLog = console.log;
          console.log = (...args) => {
            appendFileSync(consoleRecord, args.map(String).join(' ') + '\\n');
            originalLog(...args);
          };
        }
        export function isLoopbackAddress(value) {
          return ['127.0.0.1', '::1', 'localhost'].includes(String(value).toLowerCase());
        }
        // fleetd.mjs imports this at startup. These suites exercise mDNS, the HOME
        // pidfile lock and LAN log redaction — none of which configure a trusted
        // origin — so an empty parse is the faithful stub. The real parser is
        // covered directly in tests/csrf-guard.test.mjs.
        export function parseTrustedOrigins() {
          return [];
        }
        export function createHttp() {
          const server = new EventEmitter();
          server.listen = (_port, _bind, callback) => {
            server.keepalive = setInterval(() => {}, 60_000);
            setImmediate(callback);
          };
          // refreshLan mirrors the real createHttp's return contract: fleetd's
          // LAN watcher calls it when the mocked interfaces change. Recording
          // the calls lets a test assert the share-panel state followed a roam.
          const lanRefreshes = [];
          function refreshLan(nextLan) {
            lanRefreshes.push(nextLan);
            if (consoleRecord) appendFileSync(consoleRecord, 'refreshLan ' + JSON.stringify(nextLan) + '\\n');
          }
          return { server, refreshLan };
        }
      `,
    };
  }
  if (url === OS_URL) {
    return {
      format: 'module',
      shortCircuit: true,
      source: `
        import realOs from 'node:os';
        import { readFileSync } from 'node:fs';
        // Two interface seams compose: FLEETDECK_TEST_NET_FILE models a DHCP
        // "roam" (fleetd's LAN watcher polls os.networkInterfaces(), and a file
        // can be rewritten mid-run while env vars cannot), while
        // FLEETDECK_TEST_NETIFS statically pins a multihomed interface set at
        // spawn. NET_FILE wins when present; absent both seams the set is
        // network A — exactly what the pre-existing suites relied on.
        const NET_A = [{ family: 'IPv4', internal: false, address: '192.0.2.77' }];
        const seamFile = process.env.FLEETDECK_TEST_NET_FILE;
        const staticNetifs = process.env.FLEETDECK_TEST_NETIFS ? JSON.parse(process.env.FLEETDECK_TEST_NETIFS) : null;
        const ifaces = () => {
          if (seamFile) {
            try {
              const parsed = JSON.parse(readFileSync(seamFile, 'utf8'));
              if (Array.isArray(parsed) && parsed.length) return { ethernet: parsed };
            } catch { /* mid-write or absent: keep advertising network A */ }
            return { ethernet: NET_A };
          }
          if (staticNetifs) return staticNetifs;
          return { ethernet: NET_A };
        };
        export default {
          ...realOs,
          networkInterfaces: () => ifaces(),
        };
      `,
    };
  }
  return nextLoad(url, context);
}
