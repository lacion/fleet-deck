import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { resolveHome, resolvePort } from '../config.ts';
import { DaemonStartupRefusalError } from './errors.ts';
import { AppConfig, type AppConfigService } from './services/app-config.ts';

export type AppConfigEnvironmentName = 'FLEETDECK_PORT' | 'FLEETDECK_VERSION_OVERRIDE';

export interface AppConfigPathProbes {
  readonly fileURLToPath: (url: string) => string;
  readonly dirname: (filePath: string) => string;
  readonly basename: (filePath: string) => string;
  readonly resolvePath: (...segments: readonly string[]) => string;
}

/**
 * Host probes used while building AppConfig. They are functions rather than
 * captured values so importing this module, constructing its Effect, and
 * constructing its Layer remain definition-only operations.
 */
export interface AppConfigLiveProbes extends AppConfigPathProbes {
  readonly resolvePort: () => number;
  readonly resolveHome: () => string;
  readonly readEnvironment: (name: AppConfigEnvironmentName) => string | undefined;
  readonly moduleUrl: () => string;
  readonly readTextFile: (filePath: string) => string;
}

export type AppConfigLiveProbeOverrides = Partial<AppConfigLiveProbes>;

const defaultProbes: AppConfigLiveProbes = {
  resolvePort,
  resolveHome,
  readEnvironment: (name) => process.env[name],
  moduleUrl: () => import.meta.url,
  fileURLToPath,
  dirname: path.dirname,
  basename: path.basename,
  resolvePath: (...segments) => path.resolve(...segments),
  readTextFile: (filePath) => fs.readFileSync(filePath, 'utf8'),
};

/**
 * program.ts is nested under app/ in source, while Bun places the generated
 * single-file daemon beside fleetd.ts. Normalize both layouts before locating
 * package.json so source and bundle report the same version.
 */
export function resolveDaemonDirectory(
  moduleUrl: string,
  probes: AppConfigPathProbes = defaultProbes,
): string {
  const moduleDirectory = probes.dirname(probes.fileURLToPath(moduleUrl));
  return probes.basename(moduleDirectory) === 'app'
    ? probes.dirname(moduleDirectory)
    : moduleDirectory;
}

function startupRefusal(reason: string): DaemonStartupRefusalError {
  return new DaemonStartupRefusalError({
    reason,
    message: `fleetd refused to start: ${reason}`,
    cleanupCause: null,
  });
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function resolveValidatedPort(
  probes: AppConfigLiveProbes,
): Effect.Effect<number, DaemonStartupRefusalError> {
  return Effect.suspend(() => {
    let port: number;
    try {
      port = probes.resolvePort();
    } catch (cause) {
      return Effect.fail(startupRefusal(errorMessage(cause)));
    }

    // Keep program.ts's defensive validation even though the shared resolver
    // currently enforces the narrower reachable-port contract (1..65535).
    if (!Number.isInteger(port) || port < 0 || port > 65535) {
      const raw = probes.readEnvironment('FLEETDECK_PORT');
      return Effect.fail(
        startupRefusal(
          `FLEETDECK_PORT must be an integer between 0 and 65535 (got '${String(raw)}')`,
        ),
      );
    }

    return Effect.succeed(port);
  });
}

function resolveVersion(probes: AppConfigLiveProbes): string {
  const fallback = '0.0.0';
  const versionOverride = probes.readEnvironment('FLEETDECK_VERSION_OVERRIDE')?.trim();
  if (versionOverride) return versionOverride;

  try {
    const daemonDirectory = resolveDaemonDirectory(probes.moduleUrl(), probes);
    const packageFile = probes.resolvePath(daemonDirectory, '../../package.json');
    return (
      (JSON.parse(probes.readTextFile(packageFile)) as { readonly version?: string }).version ??
      fallback
    );
  } catch {
    // Package metadata is informational. Preserve the historical fail-open
    // version when source paths, generated paths, reads, or JSON are unusable.
    return fallback;
  }
}

/**
 * Cold application configuration selection. Every probe runs only when the
 * Effect is evaluated, with port validation preceding all HOME/version work.
 */
export function makeAppConfigEffect(
  overrides: AppConfigLiveProbeOverrides = {},
): Effect.Effect<AppConfigService, DaemonStartupRefusalError> {
  return Effect.suspend(() => {
    const probes: AppConfigLiveProbes = { ...defaultProbes, ...overrides };
    return Effect.map(resolveValidatedPort(probes), (port) => {
      const home = probes.resolveHome();
      const version = resolveVersion(probes);
      return { home, port, version };
    });
  });
}

/** A fresh AppConfig service is selected each time this Layer is built. */
export function makeAppConfigLayer(
  overrides: AppConfigLiveProbeOverrides = {},
): Layer.Layer<AppConfig, DaemonStartupRefusalError> {
  return Layer.effect(AppConfig, makeAppConfigEffect(overrides));
}

/** Default production definition; construction is cold and performs no probes. */
export const AppConfigLive: Layer.Layer<AppConfig, DaemonStartupRefusalError> =
  makeAppConfigLayer();
