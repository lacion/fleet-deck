import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import { BranchFields } from './BranchFields';
import { CloneAccess } from './CloneAccess';
import { SpawnForm } from './SpawnForm';
import type { StationInfo, StationView, WorkspaceListing } from '../gen';
import { claudeCaps, codexCaps } from '../testing/capabilities';
import { card, station } from '../testing/fleet';
import {
  baseProblem,
  batchPrompts,
  branchProblem,
  importIdProblem,
  knownModels,
  lastParts,
  listingView,
  providerLabel,
  recentDirs,
  spawnActions,
  spawnModes,
  spawnTargets,
  togglePin,
  versionWarning,
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
        version_status: 'unknown',
        capabilities: null,
      }),
    ).toBe('codex · codex · version unknown');
  });

  test('an older CLI shows a warning under the provider select; an ok one shows none (R8.2)', () => {
    const [claude, codex] = info().providers;
    const older = { ...claude!, version: '2.0.1', version_status: 'older_than_tested' as const };
    const html = form([reported('station-a', { info: info({ providers: [older, codex!] }) })]);
    expect(html).toContain('data-version-warning');
    expect(html).toContain('This claude CLI (2.0.1) is older than the version');
    expect(form([reported()])).not.toContain('data-version-warning');
    const ok = { ...claude!, version_status: 'ok' as const };
    expect(versionWarning(ok)).toBeNull();
    expect(versionWarning(undefined)).toBeNull();
    const broken = { ...claude!, version_status: 'broken' as const };
    expect(versionWarning(broken)).toContain('known to break the protocol');
  });

  test('the form offers station, provider, mode and the picker', () => {
    const html = form([reported()]);
    expect(html).toContain('<h2>New session</h2>');
    expect(html).toContain('<option value="station-a" selected="">Alpha</option>');
    expect(html).toContain('claude · claude · 2.1.999');
    expect(html).toContain('Browse…');
    expect(html).toContain('<option value="" selected="">Station default</option>');
    expect(html).toContain('<option value="plan">plan</option>');
    expect(html).toContain('aria-label="Directory path"');
    // No directory yet: the start button is off and the form says why.
    expect(html).toMatch(
      /<button type="submit" class="fd-primary" disabled="" aria-describedby="fd-spawn-missing">Start session<\/button>/,
    );
    expect(html).toContain(
      'To start: choose a directory (type an absolute path, or Browse… and Use this folder).',
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
    expect(html).toContain('git repository');
    expect(html).toContain('aria-label="Folder path"');
    expect(html).toContain('>Roots</button>');
    expect(html).toContain('Use this folder');
    expect(html).toContain('The station shows only the first entries.');
  });

  test('without the drive scope, stale data or providers the form is off', () => {
    expect(form([reported()], ['read'])).toContain('Starting sessions needs the drive scope.');
    expect(form([reported()], ['read'])).toMatch(/disabled=""><svg[\s\S]*?<\/svg> Browse…/);
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

  test('a start of a batch carries the batch reference (R1.4); an import never does', async () => {
    const posts: { path: string; body: unknown }[] = [];
    const actions = spawnActions(
      async () => listing,
      async (path, body) => {
        posts.push({ path, body });
        return {
          state: 'done',
          receipt: { state: 'accepted', result: { session_id: 's-b' }, reject: null },
        };
      },
      ['read', 'drive'],
      () => 'b-batch',
    );
    const spec = {
      provider_instance: 'claude',
      cwd: '/home/me/projects/app',
      mode: null,
      model: null,
      title: null,
    };
    const batch = { batch_id: '01JABCDEFGHJKMNPQRSTVWXYZ0', index: 2, size: 3 };
    await actions.start('station-a', spec, 'task', null, undefined, undefined, batch);
    await actions.start('station-a', spec, 'task');
    await actions.start('station-a', spec, 'task', null, 'native-1', undefined, batch);
    const commands = posts.map((p) => (p.body as { command: Record<string, unknown> }).command);
    expect(commands[0]).toEqual({ op: 'start', spec, prompt: { text: 'task' }, batch });
    expect(commands[1]).not.toHaveProperty('batch');
    expect(commands[2]).toMatchObject({ op: 'import' });
    expect(commands[2]).not.toHaveProperty('batch');
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

  test('the clone mode asks for a repository and shows the URL that it clones', () => {
    const render = (initialRepo: string) =>
      renderToStaticMarkup(
        <SpawnForm
          stations={[reported('station-a')]}
          freshness="live"
          onOpen={() => {}}
          initialPlace="clone"
          initialRepo={initialRepo}
        />,
      );
    const html = render('acme/app');
    expect(html).toContain('Clone a repository into a new directory here');
    expect(html).not.toContain('Clone URL');
    expect(html).toContain('Repository');
    expect(html).toContain('placeholder="owner/name, host/owner/name or a URL"');
    expect(html).toContain('<p class="fd-note">Clones https://github.com/acme/app.git</p>');
    // The directory name follows the repository.
    expect(html).toContain('New directory name<input value="app"/>');
    // A bare name without a default organization shows the error (no alert before the field was left).
    const bare = render('app');
    expect(bare).toContain(
      '<p class="fd-note">Set a default organization in Settings, or enter owner/name.</p>',
    );
    expect(bare).toMatch(/data-missing="true">To start: [^<]*enter owner\/name\.</);
    expect(render('git@gitlab.com:g/sub/tool.git')).toContain(
      '<p class="fd-note">Clones git@gitlab.com:g/sub/tool.git</p>',
    );
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
      <SpawnForm
        stations={[reported()]}
        freshness="live"
        onOpen={() => {}}
        sessions={fleet}
        initialListing={listing}
        actions={spawnActions(
          async () => listing,
          async () => ({}),
          ['read', 'drive'],
        )}
      />,
    );
    // The recent folders sit in the open panel of the picker.
    expect(html).toContain('aria-label="Recent directories"');
    expect(html).toContain('<span class="fd-label">Recent</span>');
    expect(html).toContain('title="/w/two"');
    expect(html).toContain('>w/two</button>');
    expect(html).not.toContain('/w/other');
  });

  test('the provider options show the limit of each instance (R4.3)', () => {
    const at = 1_000_000 + 60 * 60_000;
    const limit = (instance: string, used: number) =>
      card('s-' + instance, 'working', 'station-a', {
        provider_instance: instance,
        rate_limits: [{ scope: 'five_hour', used_percent: used, resets_at: at }],
      });
    const html = renderToStaticMarkup(
      <SpawnForm
        stations={[reported()]}
        freshness="live"
        onOpen={() => {}}
        sessions={[limit('claude', 42), limit('codex', 90)]}
        now={1_000_000}
      />,
    );
    expect(html).toContain(
      'claude · claude · 2.1.999 · 42% of five_hour, resets in 1 h (most room)',
    );
    expect(html).toContain(
      'codex · codex · version unknown · 90% of five_hour, resets in 1 h</option>',
    );
  });

  describe('the model field (DL-151)', () => {
    const list = (...values: string[]) =>
      values.map((value) => ({ value, label: value.toUpperCase(), description: `The ${value}.` }));
    const withModels = (
      id: string,
      models: ReturnType<typeof list> | undefined,
      updated: number,
      instance = 'claude',
      st = 'station-a',
    ) =>
      card(id, 'idle', st, {
        provider_instance: instance,
        updated_at: updated,
        ...(models === undefined ? {} : { models }),
      });

    test('knownModels takes the newest session of the station and the instance', () => {
      expect(knownModels([], 'station-a', 'claude')).toBeNull();
      const fleet = [
        withModels('a', list('old'), 10),
        withModels('b', list('new'), 30),
        withModels('c', list('codex'), 50, 'codex'),
        withModels('d', list('beta'), 60, 'claude', 'station-b'),
        withModels('e', [], 70),
        withModels('f', undefined, 80),
      ];
      expect(knownModels(fleet, 'station-a', 'claude')?.map((m) => m.value)).toEqual(['new']);
      expect(knownModels(fleet, 'station-a', 'codex')?.map((m) => m.value)).toEqual(['codex']);
      expect(knownModels(fleet, 'station-b', 'codex')).toBeNull();
      expect(knownModels([withModels('e', [], 70)], 'station-a', 'claude')).toBeNull();
    });

    const render = (sessions: ReturnType<typeof card>[]) =>
      renderToStaticMarkup(
        <SpawnForm
          stations={[reported()]}
          freshness="live"
          onOpen={() => {}}
          sessions={sessions}
        />,
      );

    test('a known list makes the field a select with the default, the models and Other…', () => {
      const html = render([withModels('a', list('opus', 'haiku'), 10)]);
      expect(html).toContain('<select aria-label="Model (optional)"');
      expect(html).toContain('<option value="" selected="">Provider default</option>');
      expect(html).toContain('<option value="opus" title="The opus.">OPUS</option>');
      expect(html).toContain('<option value="haiku" title="The haiku.">HAIKU</option>');
      expect(html).toContain('<option value="__other__">Other…</option>');
      expect(html).toContain('From the last session of this provider on this station.');
      expect(html).not.toContain('Model id or alias');
    });

    test('without a known list the field is the text input', () => {
      const html = render([withModels('a', list('opus'), 10, 'codex')]);
      expect(html).toContain('Model (optional)<input value=""/>');
      expect(html).not.toContain('Provider default');
      expect(html).not.toContain('From the last session');
    });
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
});
