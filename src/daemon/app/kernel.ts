import * as Effect from 'effect/Effect';
import type { ProcessError } from './errors.ts';
import { AppConfig } from './services/app-config.ts';
import {
  execEffect,
  type ProcessRequest,
  type ProcessSuccess,
  ProcessRunner,
} from './services/process-runner.ts';

export interface KernelProbeResult {
  readonly version: string;
  readonly port: number;
  readonly process: ProcessSuccess;
}

/** Small application-only program used by the P2 kernel and cost probes. */
export function kernelProbe(
  request: ProcessRequest,
): Effect.Effect<KernelProbeResult, ProcessError, AppConfig | ProcessRunner> {
  return Effect.gen(function* () {
    const config = yield* AppConfig;
    const process = yield* execEffect(request);

    return {
      version: config.version,
      port: config.port,
      process,
    };
  });
}
