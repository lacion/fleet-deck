import { afterEach, describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import { BranchFields } from './BranchFields';
import { CloneAccess } from './CloneAccess';
import { SetupField } from './SetupField';
import { SpawnForm } from './SpawnForm';
import type { StationInfo, StationView, WorkspaceListing } from '../gen';
import { claudeCaps, codexCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import {
  baseProblem,
  batchPrompts,
  branchProblem,
  importIdProblem,
  lastParts,
  listingView,
  providerLabel,
  readSetupCmds,
  recentDirs,
  rememberSetupCmd,
  setupKey,
  setupProblem,
  spawnActions,
  spawnModes,
  spawnTargets,
  togglePin,
  workspaceRequest,
} from './spawn';

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
      capabilities: claudeCaps(),
    },
    {
      instance: 'codex',
      provider: 'codex',
      version: null,
      auth: 'unknown',
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

describe('spawn form (P4-4)', () => {
  test('only linked stations that reported providers are targets', () => {
    const targets = spawnTargets([
      reported('station-a'),
      reported('station-b', { reachable: false }),
      station('station-c'),
      reported('station-d', { info: info({ providers: [] }) }),
    ]);
    expect(targets.map((s) => s.station_id)).toEqual(['station-a']);
    expect(
      providerLabel({
        instance: 'codex',
        provider: 'codex',
        version: null,
        auth: 'x',
        capabilities: null,
      }),
    ).toBe('codex · codex · version unknown');
  });

  test('the form offers station, provider, mode and the picker', () => {
    const html = form([reported()]);
    expect(html).toContain('<h2>New session</h2>');
    expect(html).toContain('<option value="station-a" selected="">Alpha</option>');
    expect(html).toContain('claude · claude · 2.1.999');
    expect(html).toContain('Browse the station');
    expect(html).toContain('<option value="" selected="">Station default</option>');
    expect(html).toContain('<option value="plan">plan</option>');
    expect(html).toContain('No directory chosen.');
    // No directory yet: the start button is off and the form says why.
    expect(html).toMatch(
      /<button type="submit" class="fd-primary" disabled="" aria-describedby="fd-spawn-missing">Start session<\/button>/,
    );
    expect(html).toContain(
      'To start: choose a directory (Browse the station, open a folder, then Use this directory).',
    );
  });

  test('a form that is off lists every missing step', () => {
    const html = form([reported()], ['read'], 'stale');
    expect(html).toContain(
      'To start: the board must be live; your login needs the drive scope; choose a directory',
    );
  });

  test('the owner can make a coordinator session; it starts off (ADR-006 §4, DL-217)', () => {
    const html = form([reported()]);
    expect(html).toContain('Coordinator: this session may read the fleet');
    expect(html).not.toMatch(/<input type="checkbox" checked=""\/>Coordinator/);
  });

  test('the modes come from the capabilities that the station reports (P5-3)', () => {
    const [claude, codex] = info().providers;
    expect(spawnModes(claude)).toEqual(['default', 'plan', 'acceptEdits', 'dontAsk', 'auto']);
    expect(spawnModes(codex)).toEqual(codexCaps().modes);
    expect(spawnModes(codex && { ...codex, capabilities: null })).toEqual([]);
    expect(spawnModes(undefined)).toEqual([]);
    // The first provider is Codex: the form offers its modes only.
    const html = form([
      reported('station-a', { info: info({ providers: [...info().providers].reverse() }) }),
    ]);
    expect(html).toContain('<option value="untrusted/read-only">untrusted/read-only</option>');
    expect(html).not.toContain('<option value="plan">plan</option>');
  });

  test('the picker shows entries, git repositories and truncation', () => {
    const html = form([reported()], ['read', 'drive'], 'live', listing);
    expect(html).toContain('/home/me/projects');
    expect(html).toContain('aria-label="Open app"');
    expect(html).toContain('<span class="fd-tag">git</span>');
    expect(html).toContain('Back to the roots');
    expect(html).toContain('Use this directory');
    expect(html).toContain('The station shows only the first entries.');
  });

  test('without the drive scope, stale data or providers the form is off', () => {
    expect(form([reported()], ['read'])).toContain('Starting sessions needs the drive scope.');
    expect(form([reported()], ['read'])).toMatch(/disabled="">Browse the station/);
    expect(form([reported()], ['read', 'drive'], 'stale')).toContain(
      'The form is off until the board is live.',
    );
    expect(form([station()])).toContain('No linked station has reported its providers yet.');
  });

  test('a start sends one session.start command and returns the session id', async () => {
    const posts: { path: string; body: unknown }[] = [];
    const gets: string[] = [];
    const actions = spawnActions(
      async (path) => {
        gets.push(path);
        return listing;
      },
      async (path, body) => {
        posts.push({ path, body });
        return {
          state: 'done',
          receipt: {
            state: 'accepted',
            result: { session_id: 's-new', input_id: 'i-1' },
            reject: null,
          },
        };
      },
      ['read', 'drive'],
      () => 'b-fixed',
    );
    await actions.list('station a', '/home/me/projects/app & more');
    expect(gets).toEqual([
      '/api/stations/station%20a/workspaces?path=%2Fhome%2Fme%2Fprojects%2Fapp%20%26%20more',
    ]);
    const spec = {
      provider_instance: 'claude',
      cwd: '/home/me/projects/app',
      mode: 'plan',
      model: null,
      title: 'Fix',
    };
    const out = await actions.start('station-a', spec, 'hello');
    expect(out).toEqual({ ok: true, text: 'Done.', sessionId: 's-new' });
    expect(posts).toEqual([
      {
        path: '/api/commands/session.start',
        body: {
          station: 'station-a',
          command_id: 'b-fixed',
          command: { op: 'start', spec, prompt: { text: 'hello' } },
        },
      },
    ]);
    // An empty prompt starts the session without a turn.
    await actions.start('station-a', spec, '  ');
    const second = posts[1];
    expect(second).toBeDefined();
    expect(JSON.stringify(second?.body)).toContain('"prompt":null');
  });

  test('an import sends one session.import with the native id; a flag-like id is refused', async () => {
    const posts: { path: string; body: unknown }[] = [];
    const actions = spawnActions(
      async () => listing,
      async (path, body) => {
        posts.push({ path, body });
        return {
          state: 'done',
          receipt: { state: 'accepted', result: { session_id: 's-imp' }, reject: null },
        };
      },
      ['read', 'drive'],
      () => 'b-imp',
    );
    const spec = {
      provider_instance: 'claude',
      cwd: '/home/me/projects/app',
      mode: null,
      model: null,
      title: null,
    };
    const out = await actions.start('station-a', spec, '', null, ' 0b6c-aa_1.x ');
    expect(out.sessionId).toBe('s-imp');
    expect(posts).toEqual([
      {
        path: '/api/commands/session.import',
        body: {
          station: 'station-a',
          command_id: 'b-imp',
          command: { op: 'import', spec, native_session_id: '0b6c-aa_1.x', prompt: null },
        },
      },
    ]);
    expect(importIdProblem('0b6c-aa_1.x')).toBeNull();
    for (const bad of ['', '--resume', 'a b', 'a/b', 'x'.repeat(129)]) {
      expect(importIdProblem(bad)).not.toBeNull();
    }
    expect(form([reported()])).toContain('Import CLI session id (optional)');
  });

  test('a refused or failed start shows the reason and no session', async () => {
    const refused = spawnActions(
      async () => listing,
      async () => ({
        state: 'done',
        receipt: { state: 'rejected', result: null, reject: { message: 'outside the roots' } },
      }),
      ['drive'],
    );
    const spec = { provider_instance: 'claude', cwd: '/x', mode: null, model: null, title: null };
    expect(await refused.start('a', spec, '')).toEqual({
      ok: false,
      text: 'Refused: outside the roots',
      sessionId: null,
    });
    const broken = spawnActions(async () => ({ path: 1 }), async () => {
      throw new Error('Request failed (502)');
    }, ['drive']);
    expect(await broken.start('a', spec, '')).toEqual({
      ok: false,
      text: 'Request failed (502)',
      sessionId: null,
    });
    expect(broken.list('a', null)).rejects.toThrow('Invalid directory listing');
    expect(() => listingView({ ...listing, entries: [{ name: 1 }] })).toThrow();
  });

  test('a batch starts one session per line and Nx repeats a task (M3 6-1)', () => {
    expect(batchPrompts('3x fix the tests\n\n  write docs  \n2X lint')).toEqual([
      'fix the tests',
      'fix the tests',
      'fix the tests',
      'write docs',
      'lint',
      'lint',
    ]);
    // Out of range counts stay one literal prompt.
    expect(batchPrompts('0x a\n11x b')).toEqual(['0x a', '11x b']);
    expect(batchPrompts('  ')).toEqual([]);
  });

  test('a worktree or clone start sends the workspace request (M3 6-1)', async () => {
    const posts: unknown[] = [];
    const actions = spawnActions(
      async () => listing,
      async (_path, body) => {
        posts.push(body);
        return { state: 'done', receipt: { state: 'accepted', result: null, reject: null } };
      },
      ['drive'],
      () => 'c1',
    );
    const spec = {
      provider_instance: 'claude',
      cwd: '/r/app',
      mode: null,
      model: null,
      title: null,
    };
    await actions.start('a', spec, 'go', { kind: 'worktree' });
    await actions.start('a', spec, 'go', { kind: 'clone', url: 'https://h/o/r.git' });
    await actions.start('a', spec, 'go');
    expect(JSON.stringify(posts[0])).toContain('"workspace":{"kind":"worktree"}');
    expect(JSON.stringify(posts[1])).toContain(
      '"workspace":{"kind":"clone","url":"https://h/o/r.git"}',
    );
    expect(JSON.stringify(posts[2])).not.toContain('workspace');
  });

  test('the form is a column of panels with a head each and a primary submit button (DL-141)', () => {
    const html = form([reported()]);
    for (const head of ['Where', 'Agent', 'Task', 'Options']) {
      expect(html).toContain(`<h3 class="fd-label">${head}</h3>`);
    }
    expect(html).toContain('<textarea rows="6">');
    expect(html).toContain('<label class="fd-spawn-opt"><input type="checkbox"/>Batch:');
    expect(html).not.toContain('fd-batch-preview');
    expect(html).not.toContain('fd-recent');
  });

  test('the initial station is chosen at the first render (DL-141)', () => {
    const stations = [reported('station-a'), reported('station-b')];
    const render = (initialStation?: string) =>
      renderToStaticMarkup(
        <SpawnForm
          stations={stations}
          freshness="live"
          onOpen={() => {}}
          initialStation={initialStation}
        />,
      );
    expect(render('station-b')).toContain('<option value="station-b" selected="">Beta</option>');
    expect(render('station-a')).toContain('<option value="station-a" selected="">Alpha</option>');
    expect(render('nowhere')).toContain('<option value="station-a" selected="">Alpha</option>');
    expect(render()).toContain('<option value="station-a" selected="">Alpha</option>');
  });

  test('the recent directories are the newest cwd of the station, once each, not pinned', () => {
    const at = (id: string, cwd: string | null, updated: number, st = 'station-a') =>
      card(id, 'idle', st, { cwd, updated_at: updated });
    const fleet = [
      at('a', '/w/one', 10),
      at('b', '/w/two', 30),
      at('c', '/w/one', 20),
      at('d', null, 99),
      at('e', '/w/other', 50, 'station-b'),
      at('f', '/w/pinned', 40),
    ];
    expect(recentDirs(fleet, 'station-a', [])).toEqual(['/w/pinned', '/w/two', '/w/one']);
    expect(recentDirs(fleet, 'station-a', ['/w/pinned'])).toEqual(['/w/two', '/w/one']);
    const many = Array.from({ length: 9 }, (_, i) => at(`m${i}`, `/m/${i}`, i));
    expect(recentDirs(many, 'station-a', [])).toEqual([
      '/m/8',
      '/m/7',
      '/m/6',
      '/m/5',
      '/m/4',
      '/m/3',
    ]);
    const html = renderToStaticMarkup(
      <SpawnForm stations={[reported()]} freshness="live" onOpen={() => {}} sessions={fleet} />,
    );
    expect(html).toContain('aria-label="Recent directories"');
    expect(html).toContain('<span class="fd-label">Recent</span>');
    expect(html).toContain('title="/w/two"');
    expect(html).toContain('>w/two</button>');
    expect(html).not.toContain('/w/other');
  });

  test('a path shows its last two parts', () => {
    expect(lastParts('/home/me/projects/app')).toBe('projects/app');
    expect(lastParts('/home')).toBe('home');
    expect(lastParts('/')).toBe('/');
    expect(lastParts('/a/b/')).toBe('a/b');
  });

  test('a pin goes to the front, a second toggle removes it, 20 at most, by station', () => {
    let pins = togglePin({}, 'a', '/x');
    pins = togglePin(pins, 'a', '/y');
    expect(pins['a']).toEqual(['/y', '/x']);
    expect(togglePin(pins, 'a', '/x')['a']).toEqual(['/y']);
    expect(togglePin(pins, 'b', '/z')['b']).toEqual(['/z']);
    let full = {};
    for (let i = 0; i < 25; i += 1) full = togglePin(full, 'a', `/p/${i}`);
    const list = (full as Record<string, string[]>)['a'] ?? [];
    expect(list).toHaveLength(20);
    expect(list[0]).toBe('/p/24');
  });

  test('branchProblem and baseProblem mirror the station rules (DL-144)', () => {
    expect(branchProblem('feature/PROJ-1')).toBeNull();
    expect(branchProblem('a'.repeat(200))).toBeNull();
    for (const bad of ['-x', 'a b', 'a..b', 'a~1', 'x.lock', 'a/', 'a\tb', 'a:b', 'a[b', '']) {
      expect(branchProblem(bad)).toBe('the branch name is not valid');
    }
    expect(branchProblem('a'.repeat(201))).toBe('the branch name is not valid');
    expect(baseProblem('HEAD~1')).toBeNull();
    expect(baseProblem('origin/main')).toBeNull();
    expect(baseProblem('-x')).toBe('the base is not valid');
    expect(baseProblem('a b')).toBe('the base is not valid');
    expect(baseProblem('')).toBe('the base is not valid');
  });

  test('workspaceRequest builds the workspace of a start (DL-144)', () => {
    expect(workspaceRequest('here', false, '', '', '')).toBeNull();
    expect(workspaceRequest('here', true, '', 'x', 'y')).toEqual({ kind: 'worktree' });
    expect(workspaceRequest('worktree', false, '', '', '')).toEqual({ kind: 'worktree' });
    expect(workspaceRequest('worktree', false, '', ' feat/x ', ' main ')).toEqual({
      kind: 'worktree',
      branch: 'feat/x',
      base: 'main',
    });
    expect(workspaceRequest('worktree', false, '', 'feat/x', '  ')).toEqual({
      kind: 'worktree',
      branch: 'feat/x',
    });
    // A base needs a branch.
    expect(workspaceRequest('worktree', false, '', '  ', 'main')).toEqual({ kind: 'worktree' });
    // A batch never carries a branch.
    expect(workspaceRequest('worktree', true, '', 'feat/x', 'main')).toEqual({ kind: 'worktree' });
    expect(workspaceRequest('clone', false, ' https://h/o/r.git ', 'x', 'y')).toEqual({
      kind: 'clone',
      url: 'https://h/o/r.git',
    });
  });

  test('the branch fields show Base only after a branch, and the problem of a bad name', () => {
    const render = (branch: string, base = '') =>
      renderToStaticMarkup(
        <BranchFields branch={branch} base={base} onBranch={() => {}} onBase={() => {}} />,
      );
    const empty = render('');
    expect(empty).toContain('placeholder="fd/&lt;callsign&gt;, new"');
    expect(empty).toContain('Empty: a new branch fd/&lt;callsign&gt;.');
    expect(empty).not.toContain('Base');
    expect(empty).not.toContain('role="alert"');
    expect(render('   ')).not.toContain('Base');
    const named = render('feature/PROJ-1');
    expect(named).toContain('Base');
    expect(named).toContain('placeholder="HEAD"');
    expect(named).not.toContain('role="alert"');
    expect(render('a..b')).toContain(
      '<span id="fd-spawn-branch-error" role="alert">The branch name is not valid.</span>',
    );
    expect(render('a..b')).toContain('aria-describedby="fd-spawn-branch-error"');
    expect(render('feat', '-x')).toContain(
      '<span id="fd-spawn-base-error" role="alert">The base is not valid.</span>',
    );
  });

  test('the clone access check: idle, checking, ok, failed and error states (DL-144)', () => {
    const render = (props: Partial<Parameters<typeof CloneAccess>[0]>) =>
      renderToStaticMarkup(
        <CloneAccess
          url="https://h/o/r.git"
          checking={false}
          result={null}
          onCheck={() => {}}
          {...props}
        />,
      );
    expect(render({})).toContain('>Check access</button>');
    expect(render({})).not.toContain('disabled');
    expect(render({ url: '  ' })).toContain('disabled=""');
    const busy = render({ checking: true });
    expect(busy).toContain('disabled=""');
    expect(busy).toContain('>Checking…</button>');
    const ok = render({ result: { ok: true, preflight: { ok: true, message: '', hint: null } } });
    expect(ok).toContain('<span class="fd-badge" data-tone="ok">Access OK</span>');
    expect(ok).toContain('The station can read this repository.');
    const failed = render({
      result: {
        ok: true,
        preflight: {
          ok: false,
          message: 'fatal: Authentication failed',
          hint: 'Add a deploy key.',
        },
      },
    });
    expect(failed).toContain('<p role="alert">Add a deploy key.</p>');
    expect(failed).toContain('<summary>Git said</summary>');
    expect(failed).toContain('<pre class="fd-code">fatal: Authentication failed</pre>');
    const bare = render({
      result: { ok: true, preflight: { ok: false, message: '', hint: null } },
    });
    expect(bare).toContain('<p role="alert">The station cannot read this repository.</p>');
    expect(bare).not.toContain('Git said');
    const error = render({ result: { ok: false, text: 'The station did not answer.' } });
    expect(error).toContain('role="alert">The station did not answer.</p>');
  });

  test('spawnActions preflight posts the URL and maps a bad reply and an error (DL-144)', async () => {
    const calls: { path: string; body: unknown }[] = [];
    const make = (reply: () => unknown) =>
      spawnActions(
        async () => listing,
        async (path, body) => {
          calls.push({ path, body });
          return reply();
        },
        ['drive'],
      );
    const pre = { ok: false, message: 'denied', hint: null };
    const good = await make(() => pre).preflight('st/a', 'https://h/o/r.git');
    expect(good).toEqual({ ok: true, preflight: pre });
    expect(calls[0]).toEqual({
      path: '/api/stations/st%2Fa/workspace/preflight',
      body: { url: 'https://h/o/r.git' },
    });
    expect(await make(() => ({ ok: 'yes' })).preflight('a', 'u')).toEqual({
      ok: false,
      text: 'The station gave an answer that the board cannot read.',
    });
    expect(await make(() => null).preflight('a', 'u')).toEqual({
      ok: false,
      text: 'The station gave an answer that the board cannot read.',
    });
    const failing = make(() => {
      throw new Error('station_unreachable');
    });
    expect(await failing.preflight('a', 'u')).toEqual({ ok: false, text: 'station_unreachable' });
  });

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
