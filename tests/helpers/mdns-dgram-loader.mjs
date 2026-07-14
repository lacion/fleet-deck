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
  if (specifier === 'node:os' && context.parentURL?.endsWith('/scripts/fleetd/fleetd.mjs')) {
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
        appendFileSync(recordFile, JSON.stringify({ ...value, at: Date.now() }) + '\\n');
      }

      class MockSocket {
        on() { return this; }
        setMulticastTTL() {}
        setMulticastLoopback() {}
        addMembership() {}
        bind(_options, callback) { setImmediate(callback); return this; }
        send(packet, _port, _address, callback = () => {}) {
          const wire = Buffer.from(packet).toString('base64');
          record({ type: 'send', wire });
          const timer = setTimeout(() => {
            record({ type: 'callback', wire });
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
          return { server };
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
        export default {
          ...realOs,
          networkInterfaces: () => ({ ethernet: [{ family: 'IPv4', internal: false, address: '192.0.2.77' }] }),
        };
      `,
    };
  }
  return nextLoad(url, context);
}
