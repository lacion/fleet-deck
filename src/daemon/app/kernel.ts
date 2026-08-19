import * as Effect from 'effect/Effect';
import type { ProcessRunnerUnavailableError } from './errors.ts';
import { AppConfig } from './services/app-config.ts';
import {
  type ProcessRequest,
  type ProcessResult,
  ProcessRunner,
} from './services/process-runner.ts';

export interface KernelProbeResult {
  readonly version: string;
  readonly port: number;
  readonly process: ProcessResult;
}

/** Small application-only program used by the P2 kernel and cost probes. */
export function kernelProbe(
  request: ProcessRequest,
): Effect.Effect<KernelProbeResult, ProcessRunnerUnavailableError, AppConfig | ProcessRunner> {
  return Effect.gen(function* () {
    const config = yield* AppConfig;
    const processRunner = yield* ProcessRunner;
    const process = yield* processRunner.run(request);

    return {
      version: config.version,
      port: config.port,
      process,
    };
  });
}
