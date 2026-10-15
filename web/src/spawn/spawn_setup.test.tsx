import { afterEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import { SetupField } from './SetupField';
import type { StationInfo, StationView, WorkspaceListing } from '../gen';
import { claudeCaps, codexCaps } from '../testing/capabilities';
import { station } from '../testing/fleet';
import { readSetupCmds, rememberSetupCmd, setupKey, setupProblem, spawnActions } from './spawn';

const info = (extra: Partial<StationInfo> = {}): StationInfo => ({
  name: 'Alpha',
  incarnation: 'inc',
  head: 0,
  live_sessions: 0,
  version: '0.2.0',
  max_sessions: 4,
  allow_shell: null,
  providers: [
    {
      instance: 'claude',
      provider: 'claude',
      version: '2.1.999',
      auth: 'signed_in',
      version_status: 'unknown',
      capabilities: claudeCaps(),
    },
    {
      instance: 'codex',
      provider: 'codex',
      version: null,
      auth: 'unknown',
      version_status: 'unknown',
      capabilities: codexCaps(),
    },
  ],
  ...extra,
});

const reported = (id = 'station-a', extra: Partial<StationView> = {}) =>
  station(id, { info: info(), info_at: 1, ...extra });

const listing: WorkspaceListing = {
  path: '/home/me/projects',
  parent: null,
  entries: [
    { name: 'app', path: '/home/me/projects/app', repository: true },
    { name: 'notes', path: '/home/me/projects/notes', repository: false },
  ],
  truncated: true,
};

const form = (
  stations: readonly StationView[],
  scopes = ['read', 'drive'],
  freshness: 'live' | 'stale' = 'live',
  initial: WorkspaceListing | null = null,
) =>
  renderToStaticMarkup(
    <App
      stations={stations}
      freshness={freshness}
      initialRoute={{ name: 'new' }}
      spawn={spawnActions(
        async () => listing,
        async () => ({}),
        scopes,
      )}
      initialListing={initial}
    />,
  );

describe('spawn form: the setup command (DL-148)', () => {
  test('the setup command problems mirror the station (DL-148)', () => {
    expect(setupProblem('')).toBeNull();
    expect(setupProblem('  \n ')).toBeNull();
    expect(setupProblem('bun install')).toBeNull();
    expect(setupProblem('a'.repeat(4096))).toBeNull();
    expect(setupProblem('a'.repeat(4097))).toBe(
      'The setup command is longer than 4096 characters.',
    );
    expect(setupProblem('ls\0')).toBe('The setup command contains a NUL character.');
  });

  test('the setup memory key joins the station and the directory', () => {
    expect(setupKey('st-a', '/w/app')).toBe('st-a\n/w/app');
  });

  describe('the setup memory', () => {
    const real = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    const fake = (data: Record<string, string>, fail = false) => {
      Object.defineProperty(globalThis, 'localStorage', {
        configurable: true,
        value: {
          getItem: (k: string) => {
            if (fail) throw new Error('blocked');
            return data[k] ?? null;
          },
          setItem: (k: string, v: string) => {
            if (fail) throw new Error('blocked');
            data[k] = v;
          },
        },
      });
    };
    afterEach(() => {
      if (real === undefined) delete (globalThis as { localStorage?: unknown }).localStorage;
      else Object.defineProperty(globalThis, 'localStorage', real);
    });

    test('stores, replaces and deletes a command', () => {
      fake({});
      expect(readSetupCmds()).toEqual({});
      rememberSetupCmd('k1', 'bun install');
      rememberSetupCmd('k2', 'make');
      expect(readSetupCmds()).toEqual({ k1: 'bun install', k2: 'make' });
      rememberSetupCmd('k1', 'bun i');
      expect(readSetupCmds()['k1']).toBe('bun i');
      rememberSetupCmd('k1', null);
      expect(readSetupCmds()).toEqual({ k2: 'make' });
    });

    test('keeps 50 keys at most and drops the oldest', () => {
      fake({});
      for (let i = 0; i < 51; i += 1) rememberSetupCmd(`k${i}`, `c${i}`);
      const all = readSetupCmds();
      expect(Object.keys(all).length).toBe(50);
      expect(all['k0']).toBeUndefined();
      expect(all['k1']).toBe('c1');
      expect(all['k50']).toBe('c50');
    });

    test('a storage that throws or holds bad data does not throw', () => {
      fake({}, true);
      expect(readSetupCmds()).toEqual({});
      expect(() => rememberSetupCmd('k', 'x')).not.toThrow();
      expect(() => rememberSetupCmd('k', null)).not.toThrow();
      fake({ 'fd-setup-cmds': '[1]' });
      expect(readSetupCmds()).toEqual({});
      fake({ 'fd-setup-cmds': '{"a":"x","b":3}' });
      expect(readSetupCmds()).toEqual({ a: 'x' });
    });
  });

  test('a start sends setup_cmd only when it is not blank, and never for an import (DL-148)', async () => {
    const bodies: { command: Record<string, unknown> }[] = [];
    const actions = spawnActions(
      async () => listing,
      async (_path, body) => {
        bodies.push(body as { command: Record<string, unknown> });
        return {};
      },
      ['drive'],
    );
    const spec = {
      provider_instance: 'claude',
      cwd: '/w',
      mode: null,
      model: null,
      title: null,
    };
    await actions.start('a', spec, 'go', null, undefined, '  bun install \n');
    await actions.start('a', spec, 'go', null, undefined, '   ');
    await actions.start('a', spec, 'go', null, undefined, undefined);
    await actions.start('a', spec, 'go', null, 'native-1', 'bun install');
    expect(bodies[0]?.command['setup_cmd']).toBe('  bun install \n');
    expect('setup_cmd' in (bodies[1]?.command ?? {})).toBe(false);
    expect('setup_cmd' in (bodies[2]?.command ?? {})).toBe(false);
    expect(bodies[3]?.command['op']).toBe('import');
    expect('setup_cmd' in (bodies[3]?.command ?? {})).toBe(false);
  });

  test('the setup field is on the form only when the station allows it (DL-148)', () => {
    const withFlag = (flag: boolean | null | undefined) =>
      form([
        reported('station-a', {
          info: info(flag === undefined ? {} : { allow_setup_cmd: flag }),
        }),
      ]);
    expect(withFlag(undefined)).not.toContain('Setup command');
    expect(withFlag(null)).not.toContain('Setup command');
    expect(withFlag(false)).not.toContain('Setup command');
    const on = withFlag(true);
    expect(on).toContain('Setup command (optional)');
    expect(on).toContain('Remember for this directory');
    expect(on).toContain('class="fd-spawn-setup"');
  });

  test('the setup field shows the problem of a 4097-character command (DL-148)', () => {
    const render = (value: string) =>
      renderToStaticMarkup(
        <SetupField value={value} remember={false} onValue={() => {}} onRemember={() => {}} />,
      );
    expect(render('bun install')).not.toContain('role="alert"');
    expect(render('a'.repeat(4097))).toContain(
      '<p role="alert">The setup command is longer than 4096 characters.</p>',
    );
    expect(render('a\0b')).toContain('The setup command contains a NUL character.');
    expect(render('x')).toContain(
      'Runs with /bin/sh in the new workspace before the agent starts.',
    );
  });
});

describe('spawn form: the station resources (DL-193)', () => {
  const busy = {
    at: 1,
    disks: [],
    memory: null,
    cpu: { cores: 4, load1_x100: 0, load5_x100: 1250 },
  };
  const submit = (html: string) => html.match(/<button type="submit"[^>]*>/)?.[0];

  test('a chosen station with a busy CPU shows the warning and does not disable the submit button', () => {
    const html = form([reported('station-a', { info: info({ resources: busy }) })]);
    expect(html).toContain('data-resource-warning');
    expect(html).toContain(
      'This station is low on resources: Busy CPU: load 12.5 on 4 cores. You can still start the session.',
    );
    // the missing directory disables the button on both forms; the warning adds nothing
    expect(submit(html)).toBe(submit(form([reported()])));
  });

  test('a station without a warning shows no line', () => {
    expect(form([reported()])).not.toContain('data-resource-warning');
    const calm = { ...busy, cpu: { cores: 4, load1_x100: 0, load5_x100: 100 } };
    expect(form([reported('station-a', { info: info({ resources: calm }) })])).not.toContain(
      'data-resource-warning',
    );
  });
});
