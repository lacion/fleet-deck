// os-net.ts — the network-interface seam.
//
// The Host allowlist and the LAN share URLs refresh from the interface list on
// every checked request of a live daemon, so a roaming host (Wi-Fi change, DHCP
// renewal, VPN up/down) is recognized at its NEW address without a restart
// (BUG-118/129). Tests must drive that list to simulate the roam, but
// os.networkInterfaces() is a native builtin that cannot be monkey-patched after
// import, and Bun ignores the node:module loader hooks the suite used to swap
// `node:os` wholesale. So the daemon reads its interface list through this one
// indirection, resolved in this precedence:
//   1. an in-process override (__setInterfaces) — the network-refresh suite,
//   2. an env-driven set for a SPAWNED daemon (gated on FLEETDECK_TEST_NET_MOCK,
//      the audit-regression suite — a child process the override cannot reach),
//   3. the real os.networkInterfaces().
// Both test paths are inert in production (no override installed, the flag
// unset), so this is byte-identical to calling os.networkInterfaces() directly.

import { readFileSync } from 'node:fs';
import os from 'node:os';

type Interfaces = ReturnType<typeof os.networkInterfaces>;

// The in-process override the network-refresh suite drives. A flat array of
// interface entries, mirroring the old os-facade loader's contract exactly:
// networkInterfaces() then reports them under a single synthetic adapter name,
// so Object.values(...).flat() and Object.entries(...) both see them.
interface TestInterface {
  family: string;
  internal: boolean;
  address: string;
}

let override: TestInterface[] | null = null;

// The env-driven set for a spawned audit-regression daemon (a separate process,
// so __setInterfaces cannot reach it, and Bun ignores the node:os loader hook the
// suite once used). Armed only when FLEETDECK_TEST_NET_MOCK=1:
//   FLEETDECK_TEST_NET_FILE   models a DHCP roam — re-read every call so the file
//                             can be rewritten mid-run (a JSON array of entries);
//   FLEETDECK_TEST_NETIFS     statically pins a multihomed set (a JSON object of
//                             adapter-name → entries), returned as-is;
//   absent both               network A — exactly what the pre-existing suites
//                             relied on as the single-homed default.
// The precedence (NET_FILE > NETIFS > NET_A) matches the retired loader's os stub.
const NET_A: TestInterface[] = [{ family: 'IPv4', internal: false, address: '192.0.2.77' }];

function envInterfaces(): Interfaces | null {
  if (process.env['FLEETDECK_TEST_NET_MOCK'] !== '1') return null;
  const seamFile = process.env['FLEETDECK_TEST_NET_FILE'];
  if (seamFile) {
    try {
      const parsed = JSON.parse(readFileSync(seamFile, 'utf8')) as unknown;
      if (Array.isArray(parsed) && parsed.length > 0)
        return { ethernet: parsed };
    } catch {
      /* mid-write or absent: keep advertising network A below */
    }
    return { ethernet: NET_A } as unknown as Interfaces;
  }
  const staticNetifs = process.env['FLEETDECK_TEST_NETIFS'];
  if (staticNetifs) {
    try {
      return JSON.parse(staticNetifs) as Interfaces;
    } catch {
      /* fall through to network A */
    }
  }
  return { ethernet: NET_A } as unknown as Interfaces;
}

/** The host's network interfaces — the injected set under test, else the real one. */
export function networkInterfaces(): Interfaces {
  if (override) return { test: override } as unknown as Interfaces;
  const env = envInterfaces();
  if (env) return env;
  return os.networkInterfaces();
}

/**
 * Test seam: install (or, with null, clear) the interface list every subsequent
 * networkInterfaces() call returns. In-process only — a spawned daemon is a
 * separate process and never sees this.
 */
export function __setInterfaces(entries: TestInterface[] | null): void {
  override = entries;
}
