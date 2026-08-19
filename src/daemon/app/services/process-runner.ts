import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import type { ProcessError, ProcessRunnerUnavailableError } from '../errors.ts';

export interface ProcessRequest {
  /** Complete argv vector. No command string or shell interpretation is permitted. */
  readonly argv: readonly [executable: string, ...arguments_: string[]];
  readonly cwd?: string;
  readonly env?: Readonly<Record<string, string | undefined>>;
  /** Finite input written once before stdin closes; absent preserves the legacy ignored stdin. */
  readonly stdin?: string | Uint8Array;
  readonly timeoutMs?: number;
  readonly signal?: AbortSignal;
  readonly killTree?: boolean;
}

/** Distinct bounded-search policy retained from files.ts. */
export interface BoundedProcessRequest {
  readonly argv: readonly [executable: string, ...arguments_: string[]];
  readonly cwd?: string;
  readonly env?: Readonly<Record<string, string | undefined>>;
  readonly stdin?: string | Uint8Array;
  readonly timeoutMs: number;
  readonly maxBytes: number;
}

export interface BoundedProcessResult {
  readonly code: number | null;
  readonly stdout: Buffer;
  readonly stderr: string;
  readonly truncated: boolean;
  readonly timedOut: boolean;
}

export interface ProcessSuccess {
  readonly ok: true;
  readonly out: string;
}

export interface ProcessFailure {
  readonly ok: false;
  readonly code?: string | number | null | undefined;
  readonly err: string;
}

/** Compatibility-shaped driver result retained beneath the typed application service. */
export type ProcessResult = ProcessSuccess | ProcessFailure;

export interface ProcessRunnerService {
  readonly run: (request: ProcessRequest) => Effect.Effect<ProcessSuccess, ProcessError>;
  readonly runBounded: (
    request: BoundedProcessRequest,
  ) => Effect.Effect<BoundedProcessResult, ProcessRunnerUnavailableError>;
}

/** Definition-only process capability; P3 supplies the direct Bun implementation. */
export class ProcessRunner extends Context.Service<ProcessRunner, ProcessRunnerService>()(
  'fleetdeck/daemon/app/ProcessRunner',
) {}

/** Application workflow for the single argv/no-shell subprocess capability. */
export function execEffect(
  request: ProcessRequest,
): Effect.Effect<ProcessSuccess, ProcessError, ProcessRunner> {
  return Effect.flatMap(ProcessRunner, (runner) => runner.run(request));
}

/** Application workflow for files.ts's partial-output bounded subprocess policy. */
export function execBoundedEffect(
  request: BoundedProcessRequest,
): Effect.Effect<BoundedProcessResult, ProcessRunnerUnavailableError, ProcessRunner> {
  return Effect.flatMap(ProcessRunner, (runner) => runner.runBounded(request));
}
