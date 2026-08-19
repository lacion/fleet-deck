import assert from 'node:assert/strict';
import {
  bindExecFileDelegate,
  execFileP,
  type ExecBoundedRequest,
  type ExecRequest,
} from '../../src/daemon/exec.ts';

await assert.rejects(execFileP('before-bind', []), /process runtime is not bound/);

const normalRequests: ExecRequest[] = [];
const boundedRequests: ExecBoundedRequest[] = [];
const delegate = {
  run(request: ExecRequest) {
    normalRequests.push(request);
    return Promise.resolve({ ok: true as const, out: request.argv.join('\0') });
  },
  runBounded(request: ExecBoundedRequest) {
    boundedRequests.push(request);
    return Promise.resolve({
      code: 7,
      stdout: Buffer.from('partial'),
      stderr: 'raw stderr\n',
      truncated: true,
      timedOut: false,
    });
  },
};
const unbind = bindExecFileDelegate(delegate);
assert.throws(() => bindExecFileDelegate(delegate), /already bound/);

assert.deepEqual(
  await execFileP('fixture', ['value with spaces'], {
    timeout: 321,
    cwd: '/tmp',
    env: { FLEETDECK_FACADE: 'normal' },
    stdin: 'input',
    killTree: true,
  }),
  { ok: true, out: 'fixture\0value with spaces' },
);
assert.deepEqual(normalRequests, [
  {
    argv: ['fixture', 'value with spaces'],
    timeoutMs: 321,
    cwd: '/tmp',
    env: { FLEETDECK_FACADE: 'normal' },
    stdin: 'input',
    killTree: true,
  },
]);

assert.deepEqual(
  await execFileP('bounded', ['arg'], {
    timeout: 123,
    maxBytes: 64,
    stdin: new Uint8Array([0, 1]),
  }),
  {
    code: 7,
    stdout: Buffer.from('partial'),
    stderr: 'raw stderr\n',
    truncated: true,
    timedOut: false,
  },
);
assert.deepEqual(boundedRequests, [
  {
    argv: ['bounded', 'arg'],
    timeoutMs: 123,
    maxBytes: 64,
    stdin: new Uint8Array([0, 1]),
  },
]);

unbind();
unbind();
await assert.rejects(execFileP('after-unbind', []), /process runtime is not bound/);
process.stdout.write('exec runtime facade ok\n');
