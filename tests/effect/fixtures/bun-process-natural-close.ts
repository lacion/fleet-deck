import { existsSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { makeBunProcessDriver } from '../../../src/daemon/platform/bun/process-driver.ts';

function required(index: number, name: string): string {
  const value = process.argv[index];
  if (!value) throw new Error(`missing ${name}`);
  return value;
}

const fixture = fileURLToPath(import.meta.url);
const mode = required(2, 'mode');

if (mode === 'term-immune') {
  const pidFile = required(3, 'descendant pid file');
  process.on('SIGTERM', () => undefined);
  writeFileSync(pidFile, `${String(process.pid)}\n`);
  setInterval(() => undefined, 1_000);
} else if (mode === 'group-root') {
  const rootPidFile = required(3, 'root pid file');
  const descendantPidFile = required(4, 'descendant pid file');
  writeFileSync(rootPidFile, `${String(process.pid)}\n`);
  Bun.spawn([process.execPath, fixture, 'term-immune', descendantPidFile], {
    stdin: 'ignore',
    stdout: 'inherit',
    stderr: 'inherit',
  });
  setInterval(() => undefined, 1_000);
} else if (mode === 'owner') {
  const rootPidFile = required(3, 'root pid file');
  const descendantPidFile = required(4, 'descendant pid file');
  const closeMarker = required(5, 'close marker');
  const driver = makeBunProcessDriver();
  const execution = driver.start({
    argv: [process.execPath, fixture, 'group-root', rootPidFile, descendantPidFile],
    timeoutMs: 10_000,
    killTree: true,
  });

  const deadline = Date.now() + 5_000;
  const poll = setInterval(() => {
    if (existsSync(rootPidFile) && existsSync(descendantPidFile)) {
      clearInterval(poll);
      execution.cancel();
      void driver.close().then(
        () => writeFileSync(closeMarker, 'closed\n'),
        (error: unknown) => {
          console.error(error);
          process.exitCode = 1;
        },
      );
    } else if (Date.now() >= deadline) {
      clearInterval(poll);
      execution.cancel();
      console.error('process group did not publish both pids');
      process.exitCode = 1;
    }
  }, 5);
} else {
  throw new Error(`unknown mode: ${mode}`);
}
