import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import type { ProcessRunnerUnavailableError } from '../../src/daemon/app/errors.ts';
import { AppConfig, type AppConfigService } from '../../src/daemon/app/services/app-config.ts';
import {
  type ProcessRequest,
  type ProcessResult,
  ProcessRunner,
  type ProcessRunnerService,
} from '../../src/daemon/app/services/process-runner.ts';

export const DefaultFakeAppConfig: AppConfigService = {
  home: '/fake/fleetdeck-home',
  port: 4711,
  version: '0.0.0-test',
};

export function fakeAppConfigLayer(
  overrides: Partial<AppConfigService> = {},
): Layer.Layer<AppConfig> {
  return Layer.succeed(AppConfig, { ...DefaultFakeAppConfig, ...overrides });
}

export interface FakeProcessRunnerOptions {
  readonly execute?: (
    request: ProcessRequest,
  ) => Effect.Effect<ProcessResult, ProcessRunnerUnavailableError>;
}

export interface FakeProcessRunner {
  readonly requests: readonly ProcessRequest[];
  readonly service: ProcessRunnerService;
  readonly layer: Layer.Layer<ProcessRunner>;
}

export function makeFakeProcessRunner(options: FakeProcessRunnerOptions = {}): FakeProcessRunner {
  const requests: ProcessRequest[] = [];
  const execute =
    options.execute ??
    ((request: ProcessRequest) =>
      Effect.succeed({
        ok: true as const,
        out: request.argv.join('\u0000'),
      }));
  const service: ProcessRunnerService = {
    run(request) {
      requests.push(request);
      return execute(request);
    },
  };

  return {
    requests,
    service,
    layer: Layer.succeed(ProcessRunner, service),
  };
}

export function fakeKernelLayer(
  options: {
    readonly config?: Partial<AppConfigService>;
    readonly processRunner?: FakeProcessRunner;
  } = {},
): {
  readonly layer: Layer.Layer<AppConfig | ProcessRunner>;
  readonly processRunner: FakeProcessRunner;
} {
  const processRunner = options.processRunner ?? makeFakeProcessRunner();
  return {
    layer: Layer.merge(fakeAppConfigLayer(options.config), processRunner.layer),
    processRunner,
  };
}
