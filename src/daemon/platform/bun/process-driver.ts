import type { ReadableStreamDefaultReader as NodeReadableStreamDefaultReader } from 'node:stream/web';
import {
  PROCESS_DRIVER_DEFAULT_TIMEOUT_MS,
  PROCESS_DRIVER_KILL_GRACE_MS,
  PROCESS_DRIVER_MAX_OUTPUT_BYTES,
  type ProcessDriver,
  type ProcessExecution,
} from '../../app/services/process-driver.ts';
import type {
  BoundedProcessRequest,
  BoundedProcessResult,
  ProcessRequest,
  ProcessResult,
} from '../../app/services/process-runner.ts';

const CANCELLED_RESULT: ProcessResult = {
  ok: false,
  code: 'ECANCELED',
  err: 'cancelled',
};

const OUTPUT_LIMIT_RESULT: ProcessResult = {
  ok: false,
  code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER',
  err: `subprocess output exceeded ${String(PROCESS_DRIVER_MAX_OUTPUT_BYTES)} bytes`,
};

// SIGKILL is uncatchable, but waitpid/process-group visibility can lag briefly.
// Keep the complete owner bounded to one grace interval for TERM plus one for reap.
const PROCESS_REAP_BUDGET_MS = PROCESS_DRIVER_KILL_GRACE_MS;
const PROCESS_GROUP_POLL_MS = 10;
type BunOutputReader = NodeReadableStreamDefaultReader<Uint8Array<ArrayBuffer>>;

export interface BunProcessOutput {
  /** Bytes retained under the selected policy, still undecoded. */
  readonly stdout: Buffer;
  readonly stderr: Buffer;
  /** Combined bytes observed before any policy-specific clipping. */
  readonly observedBytes: number;
  readonly overflowed: boolean;
}

export interface BunProcessExit {
  readonly code: number | null;
  readonly error?: Error;
}

/**
 * Policy-neutral hooks around the Bun spawn/stream/process owner. P3.8 can use
 * this same attempt with retained partial buffers, a caller max, immediate KILL,
 * and a decision delayed until cleanup without inheriting exec's result shape.
 */
export interface BunProcessPolicy<Decision> {
  readonly maxOutputBytes: number;
  readonly retainPartialOutput: boolean;
  /** Zero selects immediate SIGKILL; a positive value selects TERM then KILL. */
  readonly killGraceMs: number;
  readonly terminationDecisionTiming: 'immediate' | 'after-cleanup';
  readonly onSpawnFailure: (error: unknown) => Decision;
  readonly onCancel: (output: BunProcessOutput) => Decision;
  readonly onTimeout: (timeoutMs: number, output: BunProcessOutput) => Decision;
  readonly onOutputLimit: (output: BunProcessOutput) => Decision;
  readonly onExit: (exit: BunProcessExit, output: BunProcessOutput) => Decision;
}

export interface BunProcessAttempt<Decision> {
  readonly decision: Promise<Decision>;
  readonly cleanup: Promise<void>;
  cancel(): void;
}

interface DeferredValue<T> {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
}

function deferred<T>(): DeferredValue<T> {
  let resolveValue: (value: T) => void = () => undefined;
  const promise = new Promise<T>((resolve) => {
    resolveValue = resolve;
  });
  return { promise, resolve: resolveValue };
}

function unrefTimer(callback: () => void, milliseconds: number): ReturnType<typeof setTimeout> {
  const timer = setTimeout(callback, milliseconds);
  timer.unref();
  return timer;
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => {
    // Cleanup is an acquired-resource join, not background scheduling. Once a
    // termination path starts, its bounded poll must keep a standalone Bun
    // process alive long enough to deliver KILL and prove the child/group gone.
    setTimeout(resolve, milliseconds);
  });
}

function settlesWithin(promise: Promise<unknown>, milliseconds: number): Promise<boolean> {
  return new Promise((resolve) => {
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const finish = (value: boolean) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      resolve(value);
    };

    // Unlike the ordinary command deadline, a cleanup budget is referenced:
    // natural parent exit before this race settles would orphan owned work.
    timer = setTimeout(() => finish(false), Math.max(0, milliseconds));
    void promise.then(
      () => finish(true),
      () => finish(true),
    );
  });
}

function remaining(deadline: number): number {
  return Math.max(0, deadline - Date.now());
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message || error.name;
  return String(error);
}

function errorCode(error: unknown): string | number | undefined {
  if (typeof error !== 'object' || error === null || !('code' in error)) return undefined;
  const code = (error as { readonly code?: unknown }).code;
  return typeof code === 'string' || typeof code === 'number' ? code : undefined;
}

function spawnFailure(error: unknown): ProcessResult {
  const code = errorCode(error);
  const err = errorMessage(error);

  // Bun throws ENOENT/EACCES synchronously where Node publishes an error event,
  // so those system codes remain observable. Node's synchronous argv validation
  // had no public code, however, and Bun's ERR_INVALID_ARG_VALUE must stay hidden.
  return code === undefined || code === 'ERR_INVALID_ARG_VALUE'
    ? { ok: false, err }
    : { ok: false, code, err };
}

const EXEC_PROCESS_POLICY: BunProcessPolicy<ProcessResult> = {
  maxOutputBytes: PROCESS_DRIVER_MAX_OUTPUT_BYTES,
  retainPartialOutput: false,
  killGraceMs: PROCESS_DRIVER_KILL_GRACE_MS,
  terminationDecisionTiming: 'immediate',
  onSpawnFailure: spawnFailure,
  onCancel: () => CANCELLED_RESULT,
  onTimeout: (timeoutMs) => ({
    ok: false,
    code: 'ETIMEDOUT',
    err: `timed out after ${String(timeoutMs)}ms`,
  }),
  onOutputLimit: () => OUTPUT_LIMIT_RESULT,
  onExit: (exit, output) => {
    if (exit.error) return spawnFailure(exit.error);
    const out = output.stdout.toString('utf8');
    const err = output.stderr.toString('utf8').trim();
    return exit.code === 0
      ? { ok: true, out }
      : { ok: false, code: exit.code, err: err || `process exited ${String(exit.code ?? '')}` };
  },
};

function completedCancellation(): ProcessExecution {
  return {
    decision: Promise.resolve(CANCELLED_RESULT),
    cleanup: Promise.resolve(),
    cancel() {
      // Admission is already closed and there is no process to own.
    },
  };
}

function completedDecision<Decision>(result: Decision): ProcessExecution<Decision> {
  return {
    decision: Promise.resolve(result),
    cleanup: Promise.resolve(),
    cancel() {
      // No process was acquired, so the published decision is already final.
    },
  };
}

function formatInvalidReceived(value: string): string {
  // Node's invalid-argument formatter preserves control bytes verbatim. It
  // prefers single quotes, switching to double quotes (and escaping only
  // embedded double quotes) when the value itself contains a single quote.
  return value.includes("'") ? `"${value.replaceAll('"', '\\"')}"` : `'${value}'`;
}

function legacySpawnValidationMessage(
  request: Pick<ProcessRequest, 'argv' | 'cwd' | 'env'>,
): string | null {
  const [executable, ...arguments_] = request.argv;
  if (executable.length === 0) return "The argument 'file' cannot be empty. Received ''";

  if (executable.includes('\0')) {
    return `The argument 'file' must be a string without null bytes. Received ${formatInvalidReceived(executable)}`;
  }

  for (let index = 0; index < arguments_.length; index += 1) {
    const value = arguments_[index];
    if (value === undefined || !value.includes('\0')) continue;
    return `The argument 'args[${String(index)}]' must be a string without null bytes. Received ${formatInvalidReceived(value)}`;
  }

  if (request.cwd?.includes('\0')) {
    return `The property 'options.cwd' must be a string or Uint8Array without null bytes. Received ${formatInvalidReceived(request.cwd)}`;
  }

  if (request.env) {
    // The legacy path merged request values onto the live environment before
    // child_process validated it. Preserve that insertion order as well as the
    // key-before-value validation order for each entry.
    const environment = { ...process.env, ...request.env };
    for (const [key, value] of Object.entries(environment)) {
      if (key.includes('\0')) {
        return `The property 'options.env['${key}']' must be a string without null bytes. Received ${formatInvalidReceived(key)}`;
      }
      if (typeof value === 'string' && value.includes('\0')) {
        return `The property 'options.env['${key}']' must be a string without null bytes. Received ${formatInvalidReceived(value)}`;
      }
    }
  }

  return null;
}

function boundedOutput(
  output: BunProcessOutput,
  overrides: Pick<BoundedProcessResult, 'code' | 'truncated' | 'timedOut'>,
): BoundedProcessResult {
  return {
    ...overrides,
    stdout: output.stdout,
    stderr: output.stderr.toString('utf8'),
  };
}

function boundedSpawnFailure(error: unknown): BoundedProcessResult {
  const code = errorCode(error);
  const rawErrno =
    typeof error === 'object' &&
    error !== null &&
    'errno' in error &&
    typeof (error as { readonly errno?: unknown }).errno === 'number'
      ? (error as { readonly errno: number }).errno
      : null;
  const closeCode = process.platform === 'win32' && code === 'ENOENT' ? -4058 : (rawErrno ?? null);
  return {
    code: closeCode,
    stdout: Buffer.alloc(0),
    // The retired files.ts path ran node:child_process under Bun, whose
    // asynchronous spawn error uses Bun's posix_spawn message verbatim.
    stderr: errorMessage(error),
    truncated: false,
    timedOut: false,
  };
}

function boundedProcessPolicy(
  request: BoundedProcessRequest,
): BunProcessPolicy<BoundedProcessResult> {
  return {
    maxOutputBytes: request.maxBytes,
    retainPartialOutput: true,
    killGraceMs: 0,
    terminationDecisionTiming: 'after-cleanup',
    onSpawnFailure: boundedSpawnFailure,
    onCancel: (output) => boundedOutput(output, { code: null, truncated: true, timedOut: false }),
    onTimeout: (_timeoutMs, output) =>
      boundedOutput(output, { code: null, truncated: true, timedOut: true }),
    onOutputLimit: (output) =>
      boundedOutput(output, { code: null, truncated: true, timedOut: false }),
    onExit: (exit, output) =>
      exit.error
        ? boundedSpawnFailure(exit.error)
        : boundedOutput(output, {
            code: exit.code,
            truncated: false,
            timedOut: false,
          }),
  };
}

class BunProcessExecution<Decision> implements BunProcessAttempt<Decision> {
  readonly decision: Promise<Decision>;
  readonly cleanup: Promise<void>;

  private readonly resolveDecision: (result: Decision) => void;
  private readonly resolveCleanup: () => void;
  private readonly exited = deferred<BunProcessExit>();
  private readonly signal: AbortSignal | undefined;
  private readonly killTree: boolean;
  private readonly policy: BunProcessPolicy<Decision>;
  private readonly stdoutChunks: Buffer[] = [];
  private readonly stderrChunks: Buffer[] = [];
  private readonly abortListener = () => this.cancel();

  private child: Bun.ReadableSubprocess | null = null;
  private stdoutReader: BunOutputReader | null = null;
  private stderrReader: BunOutputReader | null = null;
  private drains: Promise<void> = Promise.resolve();
  private streamCancellation: Promise<void> | null = null;
  private deadline: ReturnType<typeof setTimeout> | null = null;
  private exitObservation: BunProcessExit | null = null;
  private outputBytes = 0;
  private published = false;
  private admissionStopped = false;
  private terminalSelected = false;
  private decisionSettled = false;
  private cleanupSettled = false;
  private finalizingExit = false;
  private terminating = false;
  private pendingDecision: (() => Decision) | null = null;

  constructor(request: ProcessRequest, policy: BunProcessPolicy<Decision>) {
    const decision = deferred<Decision>();
    const cleanup = deferred<void>();
    this.decision = decision.promise;
    this.cleanup = cleanup.promise;
    this.resolveDecision = decision.resolve;
    this.resolveCleanup = cleanup.resolve;
    this.signal = request.signal;
    this.killTree = request.killTree === true && process.platform !== 'win32';
    this.policy = policy;

    this.spawn(request);
  }

  cancel(): void {
    this.selectTermination(() => this.policy.onCancel(this.output()));
  }

  private spawn(request: ProcessRequest): void {
    const timeout = request.timeoutMs ?? PROCESS_DRIVER_DEFAULT_TIMEOUT_MS;
    const stdin: 'ignore' | Uint8Array =
      request.stdin === undefined
        ? 'ignore'
        : typeof request.stdin === 'string'
          ? Buffer.from(request.stdin)
          : request.stdin;

    try {
      const child = Bun.spawn({
        cmd: [...request.argv],
        ...(request.cwd === undefined ? {} : { cwd: request.cwd }),
        stdin,
        stdout: 'pipe',
        stderr: 'pipe',
        detached: this.killTree,
        // Bun's implicit environment is a process-start snapshot. Passing the
        // live object (or a live merge) preserves runtime process.env mutations.
        env: request.env ? { ...process.env, ...request.env } : process.env,
        onExit: (_child, code, _signal, error) => {
          this.observeExit({
            code,
            ...(error === undefined ? {} : { error }),
          });
        },
      });

      this.child = child;
      const stdoutReader = child.stdout.getReader();
      const stderrReader = child.stderr.getReader();
      this.stdoutReader = stdoutReader;
      this.stderrReader = stderrReader;
      const stdoutDrain = this.drain(stdoutReader, this.stdoutChunks);
      const stderrDrain = this.drain(stderrReader, this.stderrChunks);
      this.drains = Promise.all([stdoutDrain, stderrDrain]).then(() => undefined);

      this.deadline = unrefTimer(() => {
        this.selectTermination(() => this.policy.onTimeout(timeout, this.output()));
      }, timeout);

      if (this.signal?.aborted) this.cancel();
      else this.signal?.addEventListener('abort', this.abortListener, { once: true });

      // Bun documents that onExit may run before spawn() returns. Publication is
      // deliberately last so an early callback cannot finalize before streams,
      // the deadline, and the abort listener have owners.
      this.published = true;
      if (this.exitObservation && !this.terminating) void this.finalizeObservedExit();
    } catch (error) {
      this.selectTerminalDecision(this.policy.onSpawnFailure(error));
      if (this.child) this.beginTermination();
      else this.finishCleanup();
    }
  }

  private async drain(reader: BunOutputReader, chunks: Buffer[]): Promise<void> {
    try {
      while (true) {
        const next = await reader.read();
        if (next.done) return;
        this.capture(chunks, next.value);
      }
    } catch (error) {
      if (!this.terminating) this.selectTermination(() => this.policy.onSpawnFailure(error));
    } finally {
      try {
        reader.releaseLock();
      } catch {
        // A competing cancellation may already have released the reader.
      }
    }
  }

  private capture(chunks: Buffer[], chunk: Uint8Array): void {
    if (this.terminalSelected) return;
    const data = Buffer.from(chunk);
    const remainingBytes = Math.max(0, this.policy.maxOutputBytes - this.outputBytes);
    this.outputBytes += data.byteLength;
    if (this.outputBytes > this.policy.maxOutputBytes) {
      if (this.policy.retainPartialOutput && remainingBytes > 0) {
        chunks.push(data.subarray(0, remainingBytes));
      }
      this.selectTermination(() => this.policy.onOutputLimit(this.output()));
      return;
    }
    chunks.push(data);
  }

  private observeExit(observation: BunProcessExit): void {
    if (this.exitObservation) return;
    this.exitObservation = observation;
    this.exited.resolve(observation);
    if (this.published && !this.terminating) void this.finalizeObservedExit();
  }

  private async finalizeObservedExit(): Promise<void> {
    if (this.finalizingExit || this.terminating) return;
    this.finalizingExit = true;
    await this.drains;
    if (this.terminating) return;

    const observation = this.exitObservation;
    if (!observation) return;
    this.selectTerminalDecision(this.policy.onExit(observation, this.output()));
    this.finishCleanup();
  }

  private selectTerminalDecision(result: Decision): boolean {
    if (this.terminalSelected) return false;
    this.terminalSelected = true;
    this.publishDecision(result);
    return true;
  }

  private selectTermination(decision: () => Decision): boolean {
    if (this.terminalSelected) return false;
    this.terminalSelected = true;
    this.stopAdmission();
    if (this.policy.terminationDecisionTiming === 'immediate') this.publishDecision(decision());
    else this.pendingDecision = decision;
    this.beginTermination();
    return true;
  }

  private publishDecision(result: Decision): boolean {
    if (this.decisionSettled) return false;
    this.decisionSettled = true;
    this.stopAdmission();
    this.resolveDecision(result);
    return true;
  }

  private stopAdmission(): void {
    if (this.admissionStopped) return;
    this.admissionStopped = true;
    if (this.deadline) {
      clearTimeout(this.deadline);
      this.deadline = null;
    }
    // Preserve the legacy pre-abort cleanup shape: removal happens exactly once
    // even when the signal was already aborted and no listener was installed.
    this.signal?.removeEventListener('abort', this.abortListener);
  }

  private beginTermination(): void {
    if (this.terminating) return;
    this.terminating = true;
    const child = this.child;
    if (!child) {
      this.finishCleanup();
      return;
    }

    this.signalChild(child, this.policy.killGraceMs === 0 ? 'SIGKILL' : 'SIGTERM');
    void this.terminateAndReap(child);
  }

  private async terminateAndReap(child: Bun.ReadableSubprocess): Promise<void> {
    const cleanupDeadline = Date.now() + this.policy.killGraceMs + PROCESS_REAP_BUDGET_MS;
    const streams = this.cancelStreams();

    try {
      if (this.killTree) {
        const groupGone =
          this.policy.killGraceMs === 0
            ? false
            : await this.waitForProcessGroupGone(child.pid, this.policy.killGraceMs);
        if (!groupGone && this.policy.killGraceMs > 0) {
          this.signalChild(child, 'SIGKILL');
        }
        await this.waitForProcessGroupGone(child.pid, remaining(cleanupDeadline));
        await settlesWithin(this.exited.promise, remaining(cleanupDeadline));
      } else {
        const exitedAfterTerm =
          this.exitObservation !== null ||
          (this.policy.killGraceMs > 0 &&
            (await settlesWithin(this.exited.promise, this.policy.killGraceMs)));
        if (!exitedAfterTerm && this.policy.killGraceMs > 0) {
          this.signalChild(child, 'SIGKILL');
        }
        await settlesWithin(this.exited.promise, remaining(cleanupDeadline));
      }

      await settlesWithin(streams, remaining(cleanupDeadline));
    } finally {
      this.finishCleanup();
    }
  }

  private signalChild(child: Bun.ReadableSubprocess, signal: NodeJS.Signals): void {
    try {
      if (this.killTree) process.kill(-child.pid, signal);
      else child.kill(signal);
    } catch {
      // The process or process group already exited.
    }
  }

  private processGroupAlive(pid: number): boolean {
    try {
      process.kill(-pid, 0);
      return true;
    } catch {
      return false;
    }
  }

  private async waitForProcessGroupGone(pid: number, milliseconds: number): Promise<boolean> {
    const deadline = Date.now() + milliseconds;
    while (this.processGroupAlive(pid)) {
      const wait = remaining(deadline);
      if (wait === 0) return false;
      await delay(Math.min(PROCESS_GROUP_POLL_MS, wait));
    }
    return true;
  }

  private cancelStreams(): Promise<void> {
    if (this.streamCancellation) return this.streamCancellation;
    const readers = [this.stdoutReader, this.stderrReader].filter(
      (reader): reader is BunOutputReader => reader !== null,
    );
    this.streamCancellation = Promise.allSettled(
      readers.map(async (reader) => {
        try {
          await reader.cancel();
        } catch {
          // EOF or another terminal path may already own the reader.
        }
      }),
    ).then(() => undefined);
    return this.streamCancellation;
  }

  private finishCleanup(): void {
    if (this.cleanupSettled) return;
    const pendingDecision = this.pendingDecision;
    this.pendingDecision = null;
    if (pendingDecision && !this.decisionSettled) this.publishDecision(pendingDecision());
    this.cleanupSettled = true;
    this.resolveCleanup();
  }

  private output(): BunProcessOutput {
    return {
      stdout: Buffer.concat(this.stdoutChunks),
      stderr: Buffer.concat(this.stderrChunks),
      observedBytes: this.outputBytes,
      overflowed: this.outputBytes > this.policy.maxOutputBytes,
    };
  }
}

export function startBunProcessAttempt<Decision>(
  request: ProcessRequest,
  policy: BunProcessPolicy<Decision>,
): BunProcessAttempt<Decision> {
  return new BunProcessExecution(request, policy);
}

/** Direct Bun.spawn implementation of Fleet Deck's compatibility process policy. */
export class BunProcessDriver implements ProcessDriver {
  private readonly active = new Set<Pick<ProcessExecution<unknown>, 'cleanup' | 'cancel'>>();
  private closed = false;
  private closePromise: Promise<void> | null = null;

  start(request: ProcessRequest): ProcessExecution {
    if (this.closed) return completedCancellation();

    const validationFailure = legacySpawnValidationMessage(request);
    if (validationFailure) return completedDecision({ ok: false, err: validationFailure });

    const execution = startBunProcessAttempt(request, EXEC_PROCESS_POLICY);
    return this.own(execution);
  }

  startBounded(request: BoundedProcessRequest): ProcessExecution<BoundedProcessResult> {
    if (this.closed) {
      return completedDecision({
        code: null,
        stdout: Buffer.alloc(0),
        stderr: 'cancelled',
        truncated: true,
        timedOut: false,
      });
    }

    const validationFailure = legacySpawnValidationMessage(request);
    if (validationFailure) {
      return completedDecision({
        code: null,
        stdout: Buffer.alloc(0),
        stderr: validationFailure,
        truncated: false,
        timedOut: false,
      });
    }

    return this.own(startBunProcessAttempt(request, boundedProcessPolicy(request)));
  }

  private own<Decision>(execution: ProcessExecution<Decision>): ProcessExecution<Decision> {
    this.active.add(execution);
    void execution.cleanup.then(
      () => this.active.delete(execution),
      () => this.active.delete(execution),
    );
    return execution;
  }

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    this.closed = true;
    // Publish one Promise before cancellation can synchronously wake consumers.
    this.closePromise = Promise.resolve().then(async () => {
      const executions = [...this.active];
      for (const execution of executions) execution.cancel();
      await Promise.allSettled(executions.map((execution) => execution.cleanup));
    });
    return this.closePromise;
  }
}

export function makeBunProcessDriver(): ProcessDriver {
  return new BunProcessDriver();
}
