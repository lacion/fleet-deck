// test-seam.ts — the runtime-agnostic network mock for the audit-regression suite.
//
// The daemon's LAN-share path — mDNS over node:dgram, the interface list, the
// startup/roam console banner — can only be exercised end-to-end against a
// spawned daemon, yet a CI host has no multicast route and a sandbox may refuse
// every bind. The suite once swapped node:dgram / node:os / ./http.ts inside the
// child by injecting `node --experimental-loader` into its NODE_OPTIONS; Bun
// ignores node ESM loader hooks, so that mechanism is inert under the Bun-primary
// runtime (foundations-hardening §16).
//
// This is the replacement, and it rides seams the daemon already owns: the dgram
// mock enters through createMdns's `inject`, the interface list through os-net.ts,
// and the console / refreshLan recorders tee to a file. One env flag
// (FLEETDECK_TEST_NET_MOCK=1) arms everything; every entry point below is a no-op
// when the flag (or a record-sink var) is unset, so production is byte-identical
// and the seam is announced at boot alongside every other one (fleetd.ts TEST-SEAM
// block). Under Node this is behaviourally identical to the loader it replaces.

import { appendFileSync } from 'node:fs';

type DgramModule = typeof import('node:dgram');

/** True only inside a daemon the audit-regression suite spawned. */
export function testNetMock(): boolean {
  return process.env['FLEETDECK_TEST_NET_MOCK'] === '1';
}

// The JSONL the dgram mock appends, one object per line, keyed to FLEETDECK_MDNS_RECORD.
interface MdnsRecord {
  type: string;
  id?: number;
  value?: number;
  wire?: string;
  address?: string | null;
}

function recordMdns(value: MdnsRecord): void {
  const file = process.env['FLEETDECK_MDNS_RECORD'];
  if (!file) return;
  appendFileSync(file, `${JSON.stringify({ ...value, at: Date.now() })}\n`);
}

// A dgram.Socket stand-in covering exactly the surface mdns.ts drives. It never
// touches the network: bind/send resolve on a timer, membership and TTL calls
// record and (when the matching env seam is armed) throw to exercise the
// responder's degradation paths. Kept byte-faithful to the retired loader mock.
let nextSocketId = 0;
class MockSocket {
  private readonly id = nextSocketId++;
  on(): this {
    return this;
  }
  // FLEETDECK_MDNS_FAIL_TTL=unicast|multicast models a platform that refuses the
  // corresponding setsockopt, driving the responder's TTL-degradation path.
  setTTL(value: number): void {
    recordMdns({ type: 'setTTL', value });
    if (process.env['FLEETDECK_MDNS_FAIL_TTL'] === 'unicast')
      throw new Error('mock: IP_TTL refused');
  }
  setMulticastTTL(value: number): void {
    recordMdns({ type: 'setMulticastTTL', value });
    if (process.env['FLEETDECK_MDNS_FAIL_TTL'] === 'multicast')
      throw new Error('mock: IP_MULTICAST_TTL refused');
  }
  setMulticastLoopback(): void {}
  setMulticastInterface(address: string): void {
    recordMdns({ type: 'setiface', id: this.id, address });
  }
  // FLEETDECK_MDNS_JOIN_FAILS=1 models a host with no multicast route: every
  // membership join throws and the responder must degrade to unicast-only.
  addMembership(_group: string, address?: string): void {
    recordMdns({ type: 'join', id: this.id, address: address ?? null });
    if (process.env['FLEETDECK_MDNS_JOIN_FAILS']) throw new Error('no multicast route');
  }
  dropMembership(): void {}
  bind(_options: unknown, callback?: () => void): this {
    if (callback) setImmediate(callback);
    return this;
  }
  send(packet: Uint8Array, _port: number, address: string, callback: () => void = () => {}): void {
    const wire = Buffer.from(packet).toString('base64');
    recordMdns({ type: 'send', id: this.id, wire, address });
    const delay = Number(process.env['FLEETDECK_MDNS_SEND_DELAY_MS'] ?? 150);
    const timer = setTimeout(() => {
      recordMdns({ type: 'callback', id: this.id, wire });
      callback();
    }, delay);
    timer.unref?.();
  }
  close(callback?: () => void): void {
    setImmediate(() => callback?.());
  }
}

/**
 * The dgram module stand-in for createMdns's `inject`, or undefined in
 * production (the flag is unset) so the responder uses the real node:dgram.
 */
export function mdnsDgramInject(): { dgram: DgramModule } | undefined {
  if (!testNetMock()) return undefined;
  const mock = { createSocket: () => new MockSocket() };
  return { dgram: mock as unknown as DgramModule };
}

let consoleTeeInstalled = false;
/**
 * Tee console.log to FLEETDECK_TEST_CONSOLE_RECORD so a spawned daemon's
 * startup/roam banner is readable by the parent test. Idempotent; a no-op when
 * the sink var is unset, so production console output is untouched. console.error
 * (the mDNS-disabled lines) is deliberately left alone — tests read it off stderr.
 */
export function installConsoleRecorder(): void {
  const file = process.env['FLEETDECK_TEST_CONSOLE_RECORD'];
  if (!file || consoleTeeInstalled) return;
  consoleTeeInstalled = true;
  const original = console.log.bind(console);
  console.log = (...args: unknown[]): void => {
    try {
      appendFileSync(file, `${args.map((a) => String(a)).join(' ')}\n`);
    } catch {
      /* recording is best effort */
    }
    original(...args);
  };
}

/**
 * Record a refreshLan() call to the console file (the retired http mock did this
 * from inside its fake createHttp). The roam test reads these lines to prove the
 * share panel followed the interface list to a new address. No-op sink unset.
 */
export function recordRefreshLan(info: unknown): void {
  const file = process.env['FLEETDECK_TEST_CONSOLE_RECORD'];
  if (!file) return;
  try {
    appendFileSync(file, `refreshLan ${JSON.stringify(info)}\n`);
  } catch {
    /* best effort */
  }
}
