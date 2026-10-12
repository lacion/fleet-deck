import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { App } from '../App';
import type { StationInfo, StationView, WorkspaceListing } from '../gen';
import { claudeCaps, codexCaps } from '../testing/capabilities';
import { station } from '../testing/fleet';
import {
  batchPrompts,
  importIdProblem,
  listingView,
  providerLabel,
  spawnActions,
  spawnModes,
  spawnTargets,
} from './spawn';

const info = (extra: Partial<StationInfo> = {}): StationInfo => ({
  name: 'Alpha',
  incarnation: 'inc',
  head: 0,
  live_sessions: 0,
  version: '0.2.0',
  max_sessions: 4,
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
      /<button type="submit" disabled="" aria-describedby="fd-spawn-missing">Start session<\/button>/,
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
});
