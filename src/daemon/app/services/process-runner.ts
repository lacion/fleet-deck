import * as Context from 'effect/Context';
import type * as Effect from 'effect/Effect';
import type { ProcessRunnerUnavailableError } from '../errors.ts';

export interface ProcessRequest {
  /** Complete argv vector. No command string or shell interpretation is permitted. */
  readonly argv: readonly [executable: string, ...arguments_: string[]];
  readonly cwd?: string;
  readonly env?: Readonly<Record<string, string | undefined>>;
  readonly timeoutMs?: number;
  readonly signal?: AbortSignal;
  readonly killTree?: boolean;
}

/** Compatibility-shaped result retained while the Promise facade still exists. */
export type ProcessResult =
  | { readonly ok: true; readonly out: string }
  | {
      readonly ok: false;
      readonly code?: string | number | null | undefined;
      readonly err: string;
    };

export interface ProcessRunnerService {
  readonly run: (
    request: ProcessRequest,
  ) => Effect.Effect<ProcessResult, ProcessRunnerUnavailableError>;
}

/** Definition-only process capability; P3 supplies the direct Bun implementation. */
export class ProcessRunner extends Context.Service<ProcessRunner, ProcessRunnerService>()(
  'fleetdeck/daemon/app/ProcessRunner',
) {}
