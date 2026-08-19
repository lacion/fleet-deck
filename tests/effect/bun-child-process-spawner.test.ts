import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as BunPath from '@effect/platform-bun/BunPath';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as PlatformError from 'effect/PlatformError';
import * as Scope from 'effect/Scope';
import * as Stream from 'effect/Stream';
import * as ChildProcess from 'effect/unstable/process/ChildProcess';
import { type ChildProcessSpawner, ExitCode } from 'effect/unstable/process/ChildProcessSpawner';
import { layer } from '../../src/daemon/platform/bun/child-process-spawner.ts';
import { runEffectExit } from './helpers.ts';

const FIXTURE = fileURLToPath(new URL('./fixtures/bun-child-process.ts', import.meta.url));
const TestLayer = layer.pipe(Layer.provide(BunPath.layer));
const encoder = new TextEncoder();

function decode(stream: Stream.Stream<Uint8Array, PlatformError.PlatformError>) {
  return Stream.mkString(Stream.decodeText(stream));
}

function pidExists(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (
      typeof error === 'object' &&
      error !== null &&
      'code' in error &&
      (error as { readonly code?: unknown }).code === 'EPERM'
    );
  }
}

async function waitForPidExit(pid: number, timeoutMs = 3_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (pidExists(pid)) {
    if (Date.now() >= deadline) throw new Error(`process ${String(pid)} survived cleanup`);
    await Bun.sleep(20);
  }
}

async function runSuccess<A, E>(effect: Effect.Effect<A, E>): Promise<A> {
  const exit = await runEffectExit(effect);
  if (Exit.isFailure(exit)) throw Cause.squash(exit.cause);
  return exit.value;
}

async function runScoped<A>(
  effect: Effect.Effect<A, PlatformError.PlatformError, ChildProcessSpawner | Scope.Scope>,
) {
  return runSuccess(Effect.provide(Effect.scoped(effect), TestLayer));
}

describe('Bun-native ChildProcessSpawner comparison', () => {
  test('uses direct argv, live env, cwd, Web-stream stdin, and concurrent output drains', async () => {
    const key = 'FLEETDECK_EFFECT_CHILD_ENV';
    const previous = process.env[key];
    process.env[key] = 'live-after-launch';
    try {
      const result = await runScoped(
        Effect.gen(function* () {
          const handle = yield* ChildProcess.make(process.execPath, [
            FIXTURE,
            'inspect',
            'argument with spaces',
          ]);
          yield* Stream.run(Stream.make(encoder.encode('finite-input')), handle.stdin);
          const [stdout, stderr, code] = yield* Effect.all(
            [decode(handle.stdout), decode(handle.stderr), handle.exitCode],
            { concurrency: 'unbounded' },
          );
          return { stdout, stderr, code };
        }),
      );

      assert.equal(result.code, ExitCode(0));
      assert.equal(result.stderr, 'fixture-stderr');
      assert.deepEqual(JSON.parse(result.stdout), {
        args: ['argument with spaces'],
        cwd: process.cwd(),
        input: 'finite-input',
        liveEnv: 'live-after-launch',
      });
    } finally {
      if (previous === undefined) delete process.env[key];
      else process.env[key] = previous;
    }
  });

  test('pipes one public ChildProcess command into another', async () => {
    const output = await runScoped(
      Effect.gen(function* () {
        const pipeline = ChildProcess.make(process.execPath, [
          FIXTURE,
          'emit',
          'pipeline payload',
        ]).pipe(ChildProcess.pipeTo(ChildProcess.make(process.execPath, [FIXTURE, 'upper'])));
        const handle = yield* pipeline;
        const [stdout, code] = yield* Effect.all([decode(handle.stdout), handle.exitCode], {
          concurrency: 'unbounded',
        });
        return { stdout, code };
      }),
    );

    assert.equal(output.stdout, 'PIPELINE PAYLOAD');
    assert.equal(output.code, ExitCode(0));
  });

  test('maps spawn failures and unsupported additional descriptors into PlatformError', async () => {
    const missing = await runEffectExit(
      Effect.provide(
        Effect.scoped(ChildProcess.make('/definitely/missing/fleetdeck-command')),
        TestLayer,
      ),
    );
    assert.ok(Exit.isFailure(missing));
    const missingError = Cause.squash(missing.cause);
    assert.ok(missingError instanceof PlatformError.PlatformError);
    assert.equal(missingError.reason._tag, 'NotFound');

    const additionalFd = await runEffectExit(
      Effect.provide(
        Effect.scoped(
          ChildProcess.make(process.execPath, [FIXTURE, 'exit', '0'], {
            additionalFds: { fd3: { type: 'output' } },
          }),
        ),
        TestLayer,
      ),
    );
    assert.ok(Exit.isFailure(additionalFd));
    const fdError = Cause.squash(additionalFd.cause);
    assert.ok(fdError instanceof PlatformError.PlatformError);
    assert.equal(fdError.reason._tag, 'BadArgument');

    const fdPipeline = ChildProcess.make(process.execPath, [FIXTURE, 'emit', 'data']).pipe(
      ChildProcess.pipeTo(ChildProcess.make(process.execPath, [FIXTURE, 'upper']), {
        from: 'fd3',
      }),
    );
    const pipelineExit = await runEffectExit(Effect.provide(Effect.scoped(fdPipeline), TestLayer));
    assert.ok(Exit.isFailure(pipelineExit));
    const pipelineError = Cause.squash(pipelineExit.cause);
    assert.ok(pipelineError instanceof PlatformError.PlatformError);
    assert.equal(pipelineError.reason._tag, 'BadArgument');
  });

  test('scope finalization reaps a referenced process', async () => {
    const state = await runSuccess(
      Effect.provide(
        Effect.gen(function* () {
          const scope = yield* Scope.make();
          const handle = yield* Scope.provide(scope)(
            ChildProcess.make(process.execPath, [FIXTURE, 'hold'], {
              stdin: 'ignore',
              stdout: 'ignore',
              stderr: 'ignore',
            }),
          );
          const before = yield* handle.isRunning;
          yield* Scope.close(scope, Exit.void);
          const after = yield* handle.isRunning;
          return { before, after, pid: handle.pid };
        }),
        TestLayer,
      ),
    );

    assert.equal(state.before, true);
    assert.equal(state.after, false);
    await waitForPidExit(state.pid);
  });

  test('unref skips scope termination and reref restores it', async () => {
    const state = await runSuccess(
      Effect.provide(
        Effect.gen(function* () {
          const unrefScope = yield* Scope.make();
          const unrefed = yield* Scope.provide(unrefScope)(
            ChildProcess.make(process.execPath, [FIXTURE, 'hold'], {
              stdin: 'ignore',
              stdout: 'ignore',
              stderr: 'ignore',
            }),
          );
          yield* Scope.provide(unrefScope)(unrefed.unref);
          yield* Scope.close(unrefScope, Exit.void);
          const survived = yield* unrefed.isRunning;
          yield* unrefed.kill({ killSignal: 'SIGKILL' });

          const rerefScope = yield* Scope.make();
          const rerefed = yield* Scope.provide(rerefScope)(
            ChildProcess.make(process.execPath, [FIXTURE, 'hold'], {
              stdin: 'ignore',
              stdout: 'ignore',
              stderr: 'ignore',
            }),
          );
          const reref = yield* Scope.provide(rerefScope)(rerefed.unref);
          yield* reref;
          yield* Scope.close(rerefScope, Exit.void);
          const reapedAfterReref = !(yield* rerefed.isRunning);
          return { survived, reapedAfterReref, pids: [unrefed.pid, rerefed.pid] };
        }),
        TestLayer,
      ),
    );

    assert.equal(state.survived, true);
    assert.equal(state.reapedAfterReref, true);
    await Promise.all(state.pids.map((pid) => waitForPidExit(pid)));
  });

  test('forceKillAfter escalates a TERM-resistant child and settles exit observation', async () => {
    const started = performance.now();
    const exit = await runScoped(
      Effect.gen(function* () {
        const handle = yield* ChildProcess.make(process.execPath, [FIXTURE, 'stubborn'], {
          stdin: 'ignore',
          stdout: 'ignore',
          stderr: 'ignore',
        });
        yield* Effect.promise(() => Bun.sleep(100));
        yield* handle.kill({ forceKillAfter: 50 });
        return yield* Effect.exit(handle.exitCode);
      }),
    );

    assert.ok(Exit.isFailure(exit));
    assert.ok(performance.now() - started < 2_000);
  });

  test.skipIf(process.platform === 'win32')(
    'cleans the detached POSIX group when the leader exits from a signal',
    async () => {
      const observation = await runScoped(
        Effect.gen(function* () {
          const handle = yield* ChildProcess.make(process.execPath, [FIXTURE, 'signal-tree']);
          const [stdout, exit] = yield* Effect.all(
            [decode(handle.stdout), Effect.exit(handle.exitCode)],
            { concurrency: 'unbounded' },
          );
          return { descendantPid: Number.parseInt(stdout.trim(), 10), exit };
        }),
      );

      assert.ok(Number.isSafeInteger(observation.descendantPid));
      assert.ok(Exit.isFailure(observation.exit));
      await waitForPidExit(observation.descendantPid);
    },
  );
});
