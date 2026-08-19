import type * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import type { StartupError } from './errors.ts';
import { AppConfig, type AppConfigService } from './services/app-config.ts';
import { ProcessRunner, type ProcessRunnerService } from './services/process-runner.ts';

export interface LiveLayerOptions<E extends StartupError> {
  readonly config: AppConfigService;
  /**
   * Adapter construction stays injectable until P3 supplies the Bun implementation.
   * Requiring AppConfig here also proves that Layer dependencies compose in RC.110.
   */
  readonly acquireProcessRunner: Effect.Effect<ProcessRunnerService, E, AppConfig>;
}

/**
 * Definition-only production composition seam. Importing this module acquires nothing and does
 * not create a runtime; the caller decides when and in which Scope the returned Layer is built.
 */
export function makeLiveLayer<E extends StartupError>(
  options: LiveLayerOptions<E>,
): Layer.Layer<AppConfig | ProcessRunner, E> {
  const configLayer = Layer.succeed(AppConfig, options.config);
  const processRunnerLayer = Layer.effect(ProcessRunner, options.acquireProcessRunner).pipe(
    Layer.provide(configLayer),
  );

  return Layer.merge(configLayer, processRunnerLayer);
}
