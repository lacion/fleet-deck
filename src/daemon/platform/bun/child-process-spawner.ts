/**
 * Bun-native comparison implementation of Effect's unstable ChildProcessSpawner.
 *
 * This comparison adapter implements only the RC.110 public process contract and
 * is intentionally not selected by the production Live Layer.
 */
import type * as Arr from 'effect/Array';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Path from 'effect/Path';
import * as PlatformError from 'effect/PlatformError';
import type * as Scope from 'effect/Scope';
import * as Sink from 'effect/Sink';
import * as Stream from 'effect/Stream';
import * as ChildProcess from 'effect/unstable/process/ChildProcess';
import type { ChildProcessHandle } from 'effect/unstable/process/ChildProcessSpawner';
import {
  ChildProcessSpawner,
  ExitCode,
  make as makeSpawner,
  makeHandle,
  ProcessId,
} from 'effect/unstable/process/ChildProcessSpawner';

interface ExitStatus {
  readonly code: number | null;
  readonly signal: NodeJS.Signals | number | null;
}

type BunChildProcess = Bun.Subprocess<
  Bun.SpawnOptions.Writable,
  Bun.SpawnOptions.Readable,
  Bun.SpawnOptions.Readable
>;

type BunStdioInput = 'pipe' | 'inherit' | 'ignore';
type BunStdioOutput = 'pipe' | 'inherit' | 'ignore';

function commandString(command: ChildProcess.Command): string {
  const { commands } = flattenCommand(command);
  return commands.map((item) => `${item.command} ${item.args.join(' ')}`.trim()).join(' | ');
}

function errorProperty(cause: unknown, property: 'code' | 'name'): unknown {
  return typeof cause === 'object' && cause !== null && property in cause
    ? (cause as Record<string, unknown>)[property]
    : undefined;
}

function errorTag(cause: unknown): PlatformError.SystemErrorTag {
  const code = errorProperty(cause, 'code');
  const name = errorProperty(cause, 'name');
  switch (code ?? name) {
    case 'ENOENT':
    case 'NotFound':
      return 'NotFound';
    case 'EACCES':
    case 'EPERM':
    case 'PermissionDenied':
      return 'PermissionDenied';
    case 'EEXIST':
      return 'AlreadyExists';
    case 'EBUSY':
      return 'Busy';
    case 'EAGAIN':
      return 'WouldBlock';
    case 'ETIMEDOUT':
    case 'TimedOut':
      return 'TimedOut';
    case 'EPIPE':
      return 'WriteZero';
    default:
      return 'Unknown';
  }
}

function toPlatformError(
  method: string,
  cause: unknown,
  command: ChildProcess.Command,
): PlatformError.PlatformError {
  const description = commandString(command);
  return PlatformError.systemError({
    _tag: errorTag(cause),
    module: 'ChildProcess',
    method,
    pathOrDescriptor: description,
    syscall: `${method} ${description}`,
    cause,
  });
}

function unsupportedAdditionalFds(): PlatformError.PlatformError {
  return PlatformError.badArgument({
    module: 'ChildProcessSpawner',
    method: 'spawn',
    description:
      'additionalFds and fdN pipelines are unsupported: Bun 1.3.14 does not reliably publish EOF for parent-to-child descriptor pipes',
  });
}

function hasUnsupportedFileDescriptors(command: ChildProcess.Command): boolean {
  const { commands, pipeOptions } = flattenCommand(command);
  return (
    commands.some((item) => item.options.additionalFds !== undefined) ||
    pipeOptions.some(
      (options) => options.from?.startsWith('fd') === true || options.to?.startsWith('fd') === true,
    )
  );
}

function resolveEnvironment(
  options: ChildProcess.CommandOptions,
): Record<string, string | undefined> {
  if (options.extendEnv === true) return { ...process.env, ...options.env };
  // Bun's omitted env is the process-launch snapshot. Passing process.env here
  // preserves mutations made before each spawn, matching the Node adapter.
  return options.env === undefined ? process.env : { ...options.env };
}

function resolveStdinOption(options: ChildProcess.CommandOptions): ChildProcess.StdinConfig {
  const defaults: ChildProcess.StdinConfig = {
    stream: 'pipe',
    encoding: 'utf-8',
    endOnDone: true,
  };
  if (options.stdin === undefined) return defaults;
  if (typeof options.stdin === 'string' || Stream.isStream(options.stdin)) {
    return { ...defaults, stream: options.stdin };
  }
  return {
    stream: options.stdin.stream,
    encoding: options.stdin.encoding ?? defaults.encoding,
    endOnDone: options.stdin.endOnDone ?? defaults.endOnDone,
  };
}

function resolveOutputOption(
  options: ChildProcess.CommandOptions,
  name: 'stdout' | 'stderr',
): ChildProcess.StdoutConfig {
  const value = options[name];
  if (value === undefined) return { stream: 'pipe' };
  if (typeof value === 'string' || Sink.isSink(value)) return { stream: value };
  return { stream: value.stream };
}

function inputToStdioOption(input: ChildProcess.CommandInput): BunStdioInput {
  if (Stream.isStream(input) || input === 'pipe' || input === 'overlapped') return 'pipe';
  return input === 'ignore' ? 'ignore' : 'inherit';
}

function outputToStdioOption(output: ChildProcess.CommandOutput | undefined): BunStdioOutput {
  if (Sink.isSink(output) || output === undefined || output === 'pipe' || output === 'overlapped') {
    return 'pipe';
  }
  return output === 'ignore' ? 'ignore' : 'inherit';
}

function fileSinkWritable(fileSink: Bun.FileSink, endOnDone: boolean): WritableStream<Uint8Array> {
  return new WritableStream<Uint8Array>({
    async write(chunk) {
      await fileSink.write(chunk);
    },
    async close() {
      if (endOnDone) await fileSink.end();
    },
    async abort(reason) {
      await fileSink.end(reason instanceof Error ? reason : new Error(String(reason)));
    },
  });
}

function setupChildStdin(
  command: ChildProcess.StandardCommand,
  childProcess: BunChildProcess,
  config: ChildProcess.StdinConfig,
) {
  return Effect.suspend(() => {
    if (inputToStdioOption(config.stream) !== 'pipe' || typeof childProcess.stdin === 'number') {
      return Effect.succeed(Sink.drain);
    }
    const fileSink = childProcess.stdin;
    if (fileSink === undefined) return Effect.succeed(Sink.drain);
    const sink = Sink.fromWritableStream({
      evaluate: () => fileSinkWritable(fileSink, config.endOnDone !== false),
      onError: (cause) => toPlatformError('fromWritable(stdin)', cause, command),
      closeOnDone: true,
    });
    return Stream.isStream(config.stream)
      ? Effect.as(Effect.forkScoped(Stream.run(config.stream, sink)), sink)
      : Effect.succeed(sink);
  });
}

function readableOutput(
  command: ChildProcess.StandardCommand,
  method: 'fromReadable(stdout)' | 'fromReadable(stderr)',
  output: ReadableStream<Uint8Array> | number | undefined,
): Stream.Stream<Uint8Array, PlatformError.PlatformError> {
  if (!(output instanceof ReadableStream)) return Stream.empty;
  return Stream.fromReadableStream({
    evaluate: () => output,
    onError: (cause) => toPlatformError(method, cause, command),
  });
}

function setupChildOutputStreams(
  command: ChildProcess.StandardCommand,
  childProcess: BunChildProcess,
  stdoutConfig: ChildProcess.StdoutConfig,
  stderrConfig: ChildProcess.StderrConfig,
) {
  let stdout = readableOutput(command, 'fromReadable(stdout)', childProcess.stdout);
  let stderr = readableOutput(command, 'fromReadable(stderr)', childProcess.stderr);
  if (Sink.isSink(stdoutConfig.stream)) stdout = Stream.transduce(stdout, stdoutConfig.stream);
  if (Sink.isSink(stderrConfig.stream)) stderr = Stream.transduce(stderr, stderrConfig.stream);
  return { stdout, stderr, all: Stream.merge(stdout, stderr) };
}

function bestEffortGroupSignal(pid: number, signal: ChildProcess.Signal): void {
  try {
    process.kill(-pid, signal);
  } catch {
    // The group may already be empty. Exit observation must still settle.
  }
}

const make = Effect.gen(function* () {
  const path = yield* Path.Path;

  const spawn = (
    command: ChildProcess.StandardCommand,
    executable: string,
    args: ReadonlyArray<string>,
    options: {
      readonly cwd?: string;
      readonly env: Record<string, string | undefined>;
      readonly stdin: BunStdioInput;
      readonly stdout: BunStdioOutput;
      readonly stderr: BunStdioOutput;
      readonly detached: boolean;
      readonly windowsHide: boolean;
      readonly windowsVerbatimArguments: boolean;
    },
  ) =>
    Effect.try({
      try: () => {
        const exitSignal = Deferred.makeUnsafe<ExitStatus, PlatformError.PlatformError>();
        const usesProcessGroup = options.detached && process.platform !== 'win32';
        let published = false;
        let pending: Exit.Exit<ExitStatus, PlatformError.PlatformError> | undefined;
        const complete = (exit: Exit.Exit<ExitStatus, PlatformError.PlatformError>) => {
          if (published) Deferred.doneUnsafe(exitSignal, exit);
          else pending = exit;
        };
        const childProcess = Bun.spawn({
          cmd: [executable, ...args],
          ...options,
          onExit: (child, code, signal, error) => {
            const normalizedSignal = child.signalCode ?? signal;
            if (
              usesProcessGroup &&
              ((code !== null && code !== 0) || normalizedSignal !== null || error !== undefined)
            ) {
              bestEffortGroupSignal(child.pid, command.options.killSignal ?? 'SIGTERM');
            }
            complete(
              error === undefined
                ? Exit.succeed({ code, signal: normalizedSignal })
                : Exit.fail(toPlatformError('exit', error, command)),
            );
          },
        }) as BunChildProcess;
        published = true;
        if (pending !== undefined) Deferred.doneUnsafe(exitSignal, pending);
        return [childProcess, exitSignal, usesProcessGroup] as const;
      },
      catch: (cause) => toPlatformError('spawn', cause, command),
    });

  const signalProcess = (
    command: ChildProcess.StandardCommand,
    childProcess: BunChildProcess,
    usesProcessGroup: boolean,
    signal: ChildProcess.Signal,
  ) =>
    Effect.try({
      try: () => {
        if (usesProcessGroup) process.kill(-childProcess.pid, signal);
        else childProcess.kill(signal as NodeJS.Signals);
      },
      catch: (cause) => toPlatformError('kill', cause, command),
    });

  const terminate = (
    command: ChildProcess.StandardCommand,
    childProcess: BunChildProcess,
    exitSignal: Deferred.Deferred<ExitStatus, PlatformError.PlatformError>,
    usesProcessGroup: boolean,
    options: ChildProcess.KillOptions | undefined,
  ) => {
    const signal = options?.killSignal ?? 'SIGTERM';
    const killAndJoin = signalProcess(command, childProcess, usesProcessGroup, signal).pipe(
      Effect.andThen(Deferred.await(exitSignal)),
    );
    if (options?.forceKillAfter === undefined) return killAndJoin;
    return Effect.timeoutOrElse(killAndJoin, {
      duration: options.forceKillAfter,
      orElse: () =>
        signalProcess(command, childProcess, usesProcessGroup, 'SIGKILL').pipe(
          Effect.andThen(Deferred.await(exitSignal)),
        ),
    });
  };

  const spawnCommand: (
    command: ChildProcess.Command,
  ) => Effect.Effect<ChildProcessHandle, PlatformError.PlatformError, Scope.Scope> =
    Effect.fnUntraced(function* (command) {
      if (hasUnsupportedFileDescriptors(command)) {
        return yield* Effect.fail(unsupportedAdditionalFds());
      }

      switch (command._tag) {
        case 'StandardCommand': {
          const stdinConfig = resolveStdinOption(command.options);
          const stdoutConfig = resolveOutputOption(command.options, 'stdout');
          const stderrConfig = resolveOutputOption(command.options, 'stderr');
          let isReferenced = true;
          let executable = command.command;
          let args = command.args;
          let windowsVerbatimArguments = false;
          if (command.options.shell) {
            const shellCommand = `${command.command} ${command.args.join(' ')}`;
            if (process.platform === 'win32') {
              executable =
                typeof command.options.shell === 'string' ? command.options.shell : 'cmd.exe';
              args = ['/d', '/s', '/c', shellCommand];
              windowsVerbatimArguments = true;
            } else {
              executable =
                typeof command.options.shell === 'string' ? command.options.shell : '/bin/sh';
              args = ['-c', shellCommand];
            }
          }

          const detached = command.options.detached ?? process.platform !== 'win32';
          const [childProcess, exitSignal, usesProcessGroup] = yield* Effect.acquireRelease(
            spawn(command, executable, args, {
              ...(command.options.cwd === undefined
                ? {}
                : { cwd: path.resolve(command.options.cwd) }),
              env: resolveEnvironment(command.options),
              stdin: inputToStdioOption(stdinConfig.stream),
              stdout: outputToStdioOption(stdoutConfig.stream),
              stderr: outputToStdioOption(stderrConfig.stream),
              detached,
              windowsHide: command.options.windowsHide ?? !detached,
              windowsVerbatimArguments,
            }),
            Effect.fnUntraced(function* ([childProcess, exitSignal, usesProcessGroup]) {
              const exited = yield* Deferred.isDone(exitSignal);
              if (exited || !isReferenced) return;
              yield* terminate(
                command,
                childProcess,
                exitSignal,
                usesProcessGroup,
                command.options,
              ).pipe(Effect.ignore);
            }),
          );

          const reref = Effect.sync(() => {
            if (!isReferenced) {
              childProcess.ref();
              isReferenced = true;
            }
          });
          const unref = Effect.sync(() => {
            if (isReferenced) {
              childProcess.unref();
              isReferenced = false;
            }
            return reref;
          });
          const stdin = yield* setupChildStdin(command, childProcess, stdinConfig);
          const { all, stderr, stdout } = setupChildOutputStreams(
            command,
            childProcess,
            stdoutConfig,
            stderrConfig,
          );
          const isRunning = Effect.map(Deferred.isDone(exitSignal), (done) => !done);
          const exitCode = Effect.flatMap(Deferred.await(exitSignal), (status) => {
            if (status.signal === null && status.code !== null) {
              return Effect.succeed(ExitCode(status.code));
            }
            return Effect.fail(
              toPlatformError(
                'exitCode',
                new Error(
                  `Process interrupted due to receipt of signal: '${String(status.signal)}'`,
                ),
                command,
              ),
            );
          });
          const kill = (options?: ChildProcess.KillOptions) =>
            terminate(command, childProcess, exitSignal, usesProcessGroup, options).pipe(
              Effect.asVoid,
            );

          return makeHandle({
            pid: ProcessId(childProcess.pid),
            exitCode,
            isRunning,
            kill,
            stdin,
            stdout,
            stderr,
            all,
            getInputFd: () => Sink.drain,
            getOutputFd: () => Stream.empty,
            unref,
          });
        }
        case 'PipedCommand': {
          const { commands, pipeOptions } = flattenCommand(command);
          const [root, ...tail] = commands;
          const handles = [yield* spawnCommand(root)];
          for (let index = 0; index < tail.length; index += 1) {
            const next = tail[index];
            if (next === undefined) continue;
            const pipe = pipeOptions[index] ?? {};
            const previous = handles[handles.length - 1];
            if (previous === undefined) throw new Error('pipeline lost its previous handle');
            const source =
              pipe.from === 'stderr'
                ? previous.stderr
                : pipe.from === 'all'
                  ? previous.all
                  : previous.stdout;
            const stdinConfig = resolveStdinOption(next.options);
            handles.push(
              yield* spawnCommand(
                ChildProcess.make(next.command, next.args, {
                  ...next.options,
                  stdin: { ...stdinConfig, stream: source },
                }),
              ),
            );
          }

          const handle = handles[handles.length - 1];
          if (handle === undefined) throw new Error('pipeline produced no handles');
          const kill = (options?: ChildProcess.KillOptions) =>
            Effect.forEach(
              [...handles].reverse(),
              (item) => item.kill(options).pipe(Effect.ignore),
              { discard: true },
            );
          const unref = Effect.gen(function* () {
            const rerefs: Array<Effect.Effect<void, PlatformError.PlatformError>> = [];
            for (const item of handles) rerefs.push(yield* item.unref);
            return Effect.forEach([...rerefs].reverse(), (reref) => reref, { discard: true });
          });
          return makeHandle({
            pid: handle.pid,
            exitCode: handle.exitCode,
            isRunning: handle.isRunning,
            kill,
            stdin: handle.stdin,
            stdout: handle.stdout,
            stderr: handle.stderr,
            all: handle.all,
            getInputFd: handle.getInputFd,
            getOutputFd: handle.getOutputFd,
            unref,
          });
        }
      }
    });

  return makeSpawner(spawnCommand);
});

/** Comparison-only Layer; deliberately not part of production composition. */
export const layer: Layer.Layer<ChildProcessSpawner, never, Path.Path> = Layer.effect(
  ChildProcessSpawner,
  make,
);

export interface FlattenedPipeline {
  readonly commands: Arr.NonEmptyReadonlyArray<ChildProcess.StandardCommand>;
  readonly pipeOptions: ReadonlyArray<ChildProcess.PipeOptions>;
}

export function flattenCommand(command: ChildProcess.Command): FlattenedPipeline {
  const commands: Array<ChildProcess.StandardCommand> = [];
  const pipeOptions: Array<ChildProcess.PipeOptions> = [];
  const flatten = (item: ChildProcess.Command): void => {
    switch (item._tag) {
      case 'StandardCommand':
        commands.push(item);
        break;
      case 'PipedCommand':
        flatten(item.left);
        pipeOptions.push(item.options);
        flatten(item.right);
        break;
    }
  };
  flatten(command);
  const [first, ...rest] = commands;
  if (first === undefined) throw new Error('flattenCommand produced no commands');
  return { commands: [first, ...rest], pipeOptions };
}
