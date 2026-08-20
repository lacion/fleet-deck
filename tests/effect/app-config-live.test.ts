import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, test } from 'bun:test';
import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import type * as Layer from 'effect/Layer';
import {
  type AppConfigLiveProbes,
  makeAppConfigEffect,
  makeAppConfigLayer,
  resolveDaemonDirectory,
} from '../../src/daemon/app/app-config-live.ts';
import { DaemonStartupRefusalError } from '../../src/daemon/app/errors.ts';
import { AppConfig, type AppConfigService } from '../../src/daemon/app/services/app-config.ts';
import { runEffectExit } from './helpers.ts';

const APP_CONFIG_MODULE_URL = new URL('../../src/daemon/app/app-config-live.ts', import.meta.url)
  .href;

function baseProbes(): AppConfigLiveProbes {
  return {
    resolvePort: () => 4711,
    resolveHome: () => '/Users/fleet/.fleetdeck',
    readEnvironment: () => undefined,
    moduleUrl: () => APP_CONFIG_MODULE_URL,
    fileURLToPath,
    dirname: path.dirname,
    basename: path.basename,
    resolvePath: (...segments) => path.resolve(...segments),
    readTextFile: () => '{"version":"1.2.3"}',
  };
}

async function buildConfig(
  layer: Layer.Layer<AppConfig, DaemonStartupRefusalError>,
): Promise<AppConfigService> {
  const exit = await runEffectExit(Effect.provide(AppConfig, layer));
  if (Exit.isFailure(exit)) assert.fail(Cause.pretty(exit.cause));
  return exit.value;
}

function firstFailure(
  exit: Exit.Exit<unknown, DaemonStartupRefusalError>,
): DaemonStartupRefusalError {
  assert.ok(Exit.isFailure(exit));
  const reason = exit.cause.reasons[0];
  assert.ok(reason);
  assert.ok(Cause.isFailReason(reason));
  assert.ok(reason.error instanceof DaemonStartupRefusalError);
  return reason.error;
}

describe('P5 cold AppConfig Layer', () => {
  test('module import and Effect/Layer construction are cold', async () => {
    const child = Bun.spawn(
      [
        process.execPath,
        '--no-env-file',
        '--eval',
        `await import(${JSON.stringify(APP_CONFIG_MODULE_URL)}); process.stdout.write('cold-import\\n');`,
      ],
      {
        cwd: process.cwd(),
        env: {
          ...process.env,
          FLEETDECK_HOME: '/must-not-be-resolved',
          // Importing must not evaluate the rejecting production resolver.
          FLEETDECK_PORT: '0',
        },
        stdin: 'ignore',
        stdout: 'pipe',
        stderr: 'pipe',
      },
    );
    const [exitCode, stdout, stderr] = await Promise.all([
      child.exited,
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
    ]);
    assert.equal(exitCode, 0, stderr);
    assert.equal(stdout, 'cold-import\n');
    assert.equal(stderr, '');

    const events: string[] = [];
    const probes: AppConfigLiveProbes = {
      ...baseProbes(),
      resolvePort: () => {
        events.push('port');
        return 4711;
      },
    };
    const effect = makeAppConfigEffect(probes);
    const layer = makeAppConfigLayer(probes);
    assert.ok(effect);
    assert.ok(layer);
    assert.deepEqual(events, []);
  });

  test('one Layer definition reads fresh values on every build', async () => {
    let selected = {
      home: '/fleet/one',
      port: 4801,
      version: '1.0.0',
    };
    const layer = makeAppConfigLayer({
      ...baseProbes(),
      resolvePort: () => selected.port,
      resolveHome: () => selected.home,
      readEnvironment: (name) =>
        name === 'FLEETDECK_VERSION_OVERRIDE' ? selected.version : String(selected.port),
    });

    assert.deepEqual(await buildConfig(layer), selected);
    selected = {
      home: '/fleet/two',
      port: 4802,
      version: '2.0.0',
    };
    assert.deepEqual(await buildConfig(layer), selected);
  });

  test('resolvePort refusal is exact and touches no HOME or version probe', async () => {
    const events: string[] = [];
    const reason =
      'invalid FLEETDECK_PORT "0" — expected an integer port in 1..65535 (port 0 is not supported)';
    const probes: AppConfigLiveProbes = {
      ...baseProbes(),
      resolvePort: () => {
        events.push('port');
        throw new Error(reason);
      },
      resolveHome: () => {
        events.push('home');
        return '/unreachable';
      },
      readEnvironment: (name) => {
        events.push(`environment:${name}`);
        return undefined;
      },
      moduleUrl: () => {
        events.push('module-url');
        return APP_CONFIG_MODULE_URL;
      },
      readTextFile: () => {
        events.push('package-read');
        return '{}';
      },
    };

    const exit = await runEffectExit(Effect.provide(AppConfig, makeAppConfigLayer(probes)));
    const error = firstFailure(exit);
    assert.equal(error.reason, reason);
    assert.equal(error.message, `fleetd refused to start: ${reason}`);
    assert.equal(error.cleanupCause, null);
    assert.deepEqual(events, ['port']);
  });

  test('defensive port validation preserves the historical refusal text and ordering', async () => {
    const events: string[] = [];
    const probes: AppConfigLiveProbes = {
      ...baseProbes(),
      resolvePort: () => {
        events.push('port');
        return 70_000;
      },
      resolveHome: () => {
        events.push('home');
        return '/unreachable';
      },
      readEnvironment: (name) => {
        events.push(`environment:${name}`);
        return name === 'FLEETDECK_PORT' ? '70000' : 'unreachable';
      },
    };

    const exit = await runEffectExit(Effect.provide(AppConfig, makeAppConfigLayer(probes)));
    const error = firstFailure(exit);
    const reason = "FLEETDECK_PORT must be an integer between 0 and 65535 (got '70000')";
    assert.equal(error.reason, reason);
    assert.equal(error.message, `fleetd refused to start: ${reason}`);
    assert.deepEqual(events, ['port', 'environment:FLEETDECK_PORT']);
  });

  test('valid selection evaluates port, HOME, override, and package probes in order', async () => {
    const events: string[] = [];
    const probes: AppConfigLiveProbes = {
      resolvePort: () => {
        events.push('port');
        return 4922;
      },
      resolveHome: () => {
        events.push('home');
        return '/fleet/home';
      },
      readEnvironment: (name) => {
        events.push(`environment:${name}`);
        return undefined;
      },
      moduleUrl: () => {
        events.push('module-url');
        return 'file:///repo/src/daemon/app/app-config-live.ts';
      },
      fileURLToPath: (url) => {
        events.push(`file-url:${url}`);
        return '/repo/src/daemon/app/app-config-live.ts';
      },
      dirname: (filePath) => {
        events.push(`dirname:${filePath}`);
        return path.posix.dirname(filePath);
      },
      basename: (filePath) => {
        events.push(`basename:${filePath}`);
        return path.posix.basename(filePath);
      },
      resolvePath: (...segments) => {
        events.push(`resolve:${segments.join('|')}`);
        return path.posix.resolve(...segments);
      },
      readTextFile: (filePath) => {
        events.push(`package-read:${filePath}`);
        return '{"version":"3.4.5"}';
      },
    };

    assert.deepEqual(await buildConfig(makeAppConfigLayer(probes)), {
      home: '/fleet/home',
      port: 4922,
      version: '3.4.5',
    });
    assert.deepEqual(events, [
      'port',
      'home',
      'environment:FLEETDECK_VERSION_OVERRIDE',
      'module-url',
      'file-url:file:///repo/src/daemon/app/app-config-live.ts',
      'dirname:/repo/src/daemon/app/app-config-live.ts',
      'basename:/repo/src/daemon/app',
      'dirname:/repo/src/daemon/app',
      'resolve:/repo/src/daemon|../../package.json',
      'package-read:/repo/package.json',
    ]);
  });

  test('the injected HOME seam can preserve resolveHome relative normalization', async () => {
    const configured = ' workspace/../fleet-home ';
    const fallbackBase = '/Users/fleet';
    const config = await buildConfig(
      makeAppConfigLayer({
        ...baseProbes(),
        resolveHome: () => path.resolve(fallbackBase, configured.trim()),
        readEnvironment: (name) =>
          name === 'FLEETDECK_VERSION_OVERRIDE' ? 'test-version' : undefined,
      }),
    );

    assert.equal(config.home, '/Users/fleet/fleet-home');
    assert.equal(path.isAbsolute(config.home), true);
  });

  test('version override is trimmed and package failures fail open to 0.0.0', async () => {
    let packageReads = 0;
    const override = await buildConfig(
      makeAppConfigLayer({
        ...baseProbes(),
        readEnvironment: (name) =>
          name === 'FLEETDECK_VERSION_OVERRIDE' ? '  9.8.7-beta  ' : undefined,
        readTextFile: () => {
          packageReads += 1;
          throw new Error('override must bypass package metadata');
        },
      }),
    );
    assert.equal(override.version, '9.8.7-beta');
    assert.equal(packageReads, 0);

    const packageFallback = await buildConfig(
      makeAppConfigLayer({
        ...baseProbes(),
        readEnvironment: (name) => (name === 'FLEETDECK_VERSION_OVERRIDE' ? '   ' : undefined),
        readTextFile: () => '{"version":"4.5.6"}',
      }),
    );
    assert.equal(packageFallback.version, '4.5.6');

    const failingPackages: ReadonlyArray<() => string> = [
      () => {
        throw new Error('unreadable package');
      },
      () => '{not-json',
      () => '{}',
    ];
    for (const readTextFile of failingPackages) {
      const config = await buildConfig(makeAppConfigLayer({ ...baseProbes(), readTextFile }));
      assert.equal(config.version, '0.0.0');
    }
  });

  test('daemon directory normalization matches source and Bun bundle layouts', () => {
    const sourceUrl = new URL('../../src/daemon/app/app-config-live.ts', import.meta.url).href;
    const bundleUrl = new URL('../../src/daemon/fleetd.bundle.mjs', import.meta.url).href;
    const daemonDirectory = path.resolve(
      path.dirname(fileURLToPath(import.meta.url)),
      '../../src/daemon',
    );

    assert.equal(resolveDaemonDirectory(sourceUrl), daemonDirectory);
    assert.equal(resolveDaemonDirectory(bundleUrl), daemonDirectory);
  });
});
