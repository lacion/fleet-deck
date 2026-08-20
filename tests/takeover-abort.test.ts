import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { terminateDaemon } from '../src/daemon/takeover.ts';
import test from './helpers/harness-test.ts';

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

test('takeover wait observes root acquisition cancellation without killing a wedged incumbent', async (t) => {
  const child = spawn(
    process.execPath,
    [
      '-e',
      'process.on("SIGTERM",()=>{});process.stdout.write("ready\\n");setInterval(()=>{},1000)',
    ],
    { stdio: ['ignore', 'pipe', 'inherit'] },
  );
  assert.ok(child.pid);
  t.after(async () => {
    if (pidAlive(child.pid ?? -1)) child.kill('SIGKILL');
    if (child.exitCode === null && child.signalCode === null) await once(child, 'exit');
  });
  await once(child.stdout, 'data');

  const controller = new AbortController();
  const startedAt = performance.now();
  const terminating = terminateDaemon(child.pid, {
    timeoutMs: 10_000,
    signal: controller.signal,
  });
  setTimeout(() => controller.abort(), 25);

  await assert.rejects(terminating, (error: unknown) => {
    assert.equal(error instanceof Error ? error.name : '', 'AbortError');
    return true;
  });
  assert.ok(performance.now() - startedAt < 500, 'abort must not wait for takeover timeout');
  assert.equal(pidAlive(child.pid), true, 'abort never escalates the incumbent to SIGKILL');
});
