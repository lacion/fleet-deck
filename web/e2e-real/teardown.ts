// A safety net for a run that stops before its `afterAll` (a crash of
// Playwright, a failed `beforeAll`): a shim outlives its station by design
// (ADR-005), so kill every `fleetdeck shim` whose HOME is a temporary node of
// the suite (`fleet.ts` gives each station `<tmp>/fleetdeck-e2e-*/home`; the
// shim inherits it). Linux only.
import { readdirSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

export default function globalTeardown() {
  if (process.platform !== 'linux') return;
  const home = `HOME=${join(tmpdir(), 'fleetdeck-e2e-')}`;
  for (const name of readdirSync('/proc')) {
    if (!/^\d+$/.test(name)) continue;
    try {
      if (!readFileSync(`/proc/${name}/cmdline`, 'latin1').includes('shim\0--id\0')) continue;
      const environ = readFileSync(`/proc/${name}/environ`, 'latin1').split('\0');
      if (environ.some((entry) => entry.startsWith(home))) process.kill(Number(name), 'SIGKILL');
    } catch {
      // The process ended, or it is not ours.
    }
  }
}
