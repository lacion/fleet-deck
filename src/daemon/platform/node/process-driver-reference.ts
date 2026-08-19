import { spawn, type ChildProcess } from 'node:child_process';
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

function settledCancellation(): ProcessExecution {
  return {
    decision: Promise.resolve(CANCELLED_RESULT),
    cleanup: Promise.resolve(),
    cancel() {},
  };
}

function settledBoundedCancellation(): ProcessExecution<BoundedProcessResult> {
  return {
    decision: Promise.resolve({
      code: null,
      stdout: Buffer.alloc(0),
      stderr: 'cancelled',
      truncated: true,
      timedOut: false,
    }),
    cleanup: Promise.resolve(),
    cancel() {},
  };
}

function signalChild(child: ChildProcess, signal: NodeJS.Signals, killTree: boolean): void {
  try {
    if (killTree && process.platform !== 'win32' && child.pid != null) {
      process.kill(-child.pid, signal);
    } else {
      child.kill(signal);
    }
  } catch {
    /* already gone */
  }
}

function errorResult(error: unknown): ProcessResult {
  if (!(error instanceof Error)) return { ok: false, err: String(error) };
  return {
    ok: false,
    ...('code' in error ? { code: error.code as string | number | undefined } : {}),
    err: error.message || error.name,
  };
}

function synchronousSpawnErrorResult(error: unknown): ProcessResult {
  return {
    ok: false,
    err: error instanceof Error ? error.message || String(error) : String(error),
  };
}

/**
 * The extracted legacy implementation. It intentionally retains node:child_process
 * so the P3 Bun driver has a separately injectable differential reference.
 */
function startNodeExecution(request: ProcessRequest): ProcessExecution {
  let resolveDecision!: (result: ProcessResult) => void;
  let resolveCleanup!: () => void;
  const decision = new Promise<ProcessResult>((resolve) => {
    resolveDecision = resolve;
  });
  const cleanup = new Promise<void>((resolve) => {
    resolveCleanup = resolve;
  });

  let child: ChildProcess | null = null;
  let deadline: NodeJS.Timeout | null = null;
  let killTimer: NodeJS.Timeout | null = null;
  let decisionDone = false;
  let cleanupDone = false;
  let terminationStarted = false;
  let abortListenerInstalled = false;
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];
  let outputBytes = 0;

  const removeAbortListener = () => {
    request.signal?.removeEventListener('abort', cancel);
    abortListenerInstalled = false;
  };

  const settleDecision = (result: ProcessResult): void => {
    if (decisionDone) return;
    decisionDone = true;
    if (deadline) {
      clearTimeout(deadline);
      deadline = null;
    }
    // Match the legacy pre-abort observation: removal is attempted even when
    // an already-aborted signal meant no listener was installed.
    removeAbortListener();
    resolveDecision(result);
  };

  const destroyPipes = (): void => {
    child?.stdin?.destroy();
    child?.stdout?.destroy();
    child?.stderr?.destroy();
  };

  const finishCleanup = (): void => {
    if (cleanupDone) return;
    cleanupDone = true;
    if (deadline) clearTimeout(deadline);
    if (killTimer) clearTimeout(killTimer);
    deadline = null;
    killTimer = null;
    if (abortListenerInstalled) removeAbortListener();
    if (child) {
      child.stdout?.off('data', onStdout);
      child.stderr?.off('data', onStderr);
      child.stdin?.off('error', onStdinError);
      child.off('error', onError);
      child.off('close', onClose);
    }
    destroyPipes();
    resolveCleanup();
  };

  const terminate = (): void => {
    if (!child || cleanupDone || terminationStarted) return;
    terminationStarted = true;
    signalChild(child, 'SIGTERM', request.killTree === true);
    killTimer = setTimeout(() => {
      if (!child) return;
      if (child.pid != null) {
        let alive = true;
        try {
          const probe =
            request.killTree === true && process.platform !== 'win32' ? -child.pid : child.pid;
          process.kill(probe, 0);
        } catch {
          alive = false;
        }
        if (alive) signalChild(child, 'SIGKILL', request.killTree === true);
      }
      // A descendant may outlive a non-killTree leader while retaining its
      // inherited descriptors. Closing our pipe ends prevents that unrelated
      // process from holding this execution's ownership join indefinitely.
      destroyPipes();
    }, PROCESS_DRIVER_KILL_GRACE_MS);
    killTimer.unref();
  };

  function cancel(): void {
    if (!decisionDone) settleDecision(CANCELLED_RESULT);
    terminate();
  }

  const capture = (chunks: Buffer[], chunk: Buffer | string): void => {
    if (decisionDone) return;
    const data = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    outputBytes += data.byteLength;
    if (outputBytes > PROCESS_DRIVER_MAX_OUTPUT_BYTES) {
      settleDecision({
        ok: false,
        code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER',
        err: `subprocess output exceeded ${String(PROCESS_DRIVER_MAX_OUTPUT_BYTES)} bytes`,
      });
      terminate();
      return;
    }
    chunks.push(data);
  };

  function onStdout(chunk: Buffer | string): void {
    capture(stdout, chunk);
  }

  function onStderr(chunk: Buffer | string): void {
    capture(stderr, chunk);
  }

  function onStdinError(error: Error): void {
    if (decisionDone) return;
    settleDecision(errorResult(error));
    terminate();
  }

  function onError(error: Error): void {
    settleDecision(errorResult(error));
    if (child?.pid == null) finishCleanup();
    else terminate();
  }

  function onClose(code: number | null): void {
    const out = Buffer.concat(stdout).toString('utf8');
    const errorText = Buffer.concat(stderr).toString('utf8').trim();
    if (code === 0) settleDecision({ ok: true, out });
    else settleDecision({ ok: false, code, err: errorText || `process exited ${code ?? ''}` });
    finishCleanup();
  }

  try {
    const executable = request.argv[0];
    child = spawn(executable, [...request.argv.slice(1)], {
      cwd: request.cwd,
      shell: false,
      stdio: [request.stdin === undefined ? 'ignore' : 'pipe', 'pipe', 'pipe'],
      windowsHide: true,
      detached: request.killTree === true && process.platform !== 'win32',
      env: request.env ? { ...process.env, ...request.env } : process.env,
    });
    child.stdout?.on('data', onStdout);
    child.stderr?.on('data', onStderr);
    child.stdin?.on('error', onStdinError);
    child.once('error', onError);
    child.once('close', onClose);

    if (request.stdin !== undefined) {
      const input =
        typeof request.stdin === 'string'
          ? request.stdin
          : Buffer.from(request.stdin.buffer, request.stdin.byteOffset, request.stdin.byteLength);
      child.stdin?.end(input);
    }

    const timeout = request.timeoutMs ?? PROCESS_DRIVER_DEFAULT_TIMEOUT_MS;
    deadline = setTimeout(() => {
      settleDecision({
        ok: false,
        code: 'ETIMEDOUT',
        err: `timed out after ${String(timeout)}ms`,
      });
      terminate();
    }, timeout);
    deadline.unref();

    if (request.signal?.aborted) cancel();
    else if (request.signal) {
      abortListenerInstalled = true;
      request.signal.addEventListener('abort', cancel, { once: true });
    }
  } catch (error) {
    // execFileP's synchronous catch predates the async ChildProcess error
    // shape and deliberately omits `code`, even when Node's thrown error has
    // one. Keep that observable distinction in the differential reference.
    settleDecision(synchronousSpawnErrorResult(error));
    // A hostile AbortSignal or a synchronous stdin/setup failure can throw
    // after spawn returned. In that case the decision stays legacy-shaped,
    // but ownership still terminates and reaps the published child.
    if (child?.pid == null) finishCleanup();
    else terminate();
  }

  return { decision, cleanup, cancel };
}

/** Extracted files.ts policy retained only as the Node differential reference. */
function startNodeBoundedExecution(
  request: BoundedProcessRequest,
): ProcessExecution<BoundedProcessResult> {
  let resolveDecision!: (result: BoundedProcessResult) => void;
  let resolveCleanup!: () => void;
  const decision = new Promise<BoundedProcessResult>((resolve) => {
    resolveDecision = resolve;
  });
  const cleanup = new Promise<void>((resolve) => {
    resolveCleanup = resolve;
  });

  let child: ChildProcess | null = null;
  let timer: NodeJS.Timeout | null = null;
  let finished = false;
  let truncated = false;
  let timedOut = false;
  let spawnError: Error | null = null;
  let bytes = 0;
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];

  const take = (chunks: Buffer[], chunk: Buffer | string): void => {
    const data = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    const remaining = Math.max(0, request.maxBytes - bytes);
    if (remaining > 0) chunks.push(data.subarray(0, remaining));
    bytes += data.length;
    if (bytes > request.maxBytes && !truncated) {
      truncated = true;
      try {
        child?.kill('SIGKILL');
      } catch {
        /* already gone */
      }
    }
  };

  const finish = (code: number | null): void => {
    if (finished) return;
    finished = true;
    if (timer) clearTimeout(timer);
    child?.stdout?.removeAllListeners();
    child?.stderr?.removeAllListeners();
    child?.stdin?.removeAllListeners();
    child?.removeAllListeners();
    resolveDecision({
      code,
      stdout: Buffer.concat(stdout),
      stderr: spawnError
        ? String(spawnError.message || spawnError)
        : Buffer.concat(stderr).toString('utf8'),
      truncated,
      timedOut,
    });
    resolveCleanup();
  };

  const cancel = (): void => {
    if (finished) return;
    truncated = true;
    try {
      child?.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  };

  try {
    child = spawn(request.argv[0], [...request.argv.slice(1)], {
      cwd: request.cwd,
      shell: false,
      windowsHide: true,
      stdio: [request.stdin === undefined ? 'ignore' : 'pipe', 'pipe', 'pipe'],
      env: request.env ? { ...process.env, ...request.env } : process.env,
    });
    child.stdout?.on('data', (chunk: Buffer | string) => take(stdout, chunk));
    child.stderr?.on('data', (chunk: Buffer | string) => take(stderr, chunk));
    child.on('error', (error: Error) => {
      spawnError = error;
    });
    child.on('close', finish);
    if (request.stdin !== undefined) {
      child.stdin?.on('error', () => {
        /* early child exit */
      });
      const input =
        typeof request.stdin === 'string'
          ? request.stdin
          : Buffer.from(request.stdin.buffer, request.stdin.byteOffset, request.stdin.byteLength);
      child.stdin?.end(input);
    }
    timer = setTimeout(() => {
      timedOut = true;
      cancel();
    }, request.timeoutMs);
    timer.unref();
  } catch (error) {
    spawnError = error instanceof Error ? error : new Error(String(error));
    finish(null);
  }

  return { decision, cleanup, cancel };
}

class NodeProcessDriverReference implements ProcessDriver {
  readonly #active = new Set<Pick<ProcessExecution<unknown>, 'cleanup' | 'cancel'>>();
  #closed = false;
  #closePromise: Promise<void> | null = null;

  start(request: ProcessRequest): ProcessExecution {
    if (this.#closed) return settledCancellation();
    const execution = startNodeExecution(request);
    return this.#own(execution);
  }

  startBounded(request: BoundedProcessRequest): ProcessExecution<BoundedProcessResult> {
    if (this.#closed) return settledBoundedCancellation();
    return this.#own(startNodeBoundedExecution(request));
  }

  #own<Decision>(execution: ProcessExecution<Decision>): ProcessExecution<Decision> {
    this.#active.add(execution);
    void execution.cleanup.then(() => {
      this.#active.delete(execution);
    });
    return execution;
  }

  close(): Promise<void> {
    if (this.#closePromise) return this.#closePromise;
    this.#closed = true;
    let resolveClose!: () => void;
    this.#closePromise = new Promise<void>((resolve) => {
      resolveClose = resolve;
    });

    const active = [...this.#active];
    for (const execution of active) execution.cancel();
    void Promise.all(active.map((execution) => execution.cleanup)).then(() => resolveClose());
    return this.#closePromise;
  }
}

/** A fresh reference owner per Layer/test scope; importing this module acquires nothing. */
export function makeNodeProcessDriverReference(): ProcessDriver {
  return new NodeProcessDriverReference();
}
