// Builds the `fleetdeck` CLI and the fake agents once before the real-process
// flows. With `FD_BIN_DIR`, the binaries come from there and nothing builds.
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export default function setup() {
  if (process.env['FD_BIN_DIR'] !== undefined) return;
  const root = fileURLToPath(new URL('../..', import.meta.url));
  const out = spawnSync(
    'cargo',
    ['build', '-q', '-p', 'fleetdeck-cli', '-p', 'fleetdeck-testkit', '--bins'],
    { cwd: root, stdio: 'inherit', timeout: 900_000 },
  );
  if (out.status !== 0) throw new Error('cargo build of fleetdeck and the fake agents failed');
}
