// FG-5, FG-6 (design §8.5, §9.3): the Home choice and the placement fields of the spawn form.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Project, ProjectHome, StationInfo, StationView } from '../gen';
import { IntegrationsProvider } from '../integrations/context';
import { fakeActions, project } from '../integrations/fixtures';
import { claudeCaps, codexCaps } from '../testing/capabilities';
import { station } from '../testing/fleet';
import { PlacementFields } from './PlacementFields';
import { SpawnForm } from './SpawnForm';
import {
  AUTO,
  autoOffered,
  type PlacedStart,
  parseNeeds,
  placeStarts,
  projectOfStart,
  providerKinds,
  type SpawnActions,
  startHomes,
} from './spawn';

const providers = (auth = 'signed_in') => [
  {
    instance: 'claude',
    provider: 'claude' as const,
    version: '2.1.0',
    auth,
    version_status: 'unknown' as const,
    capabilities: claudeCaps(),
  },
  {
    instance: 'codex',
    provider: 'codex' as const,
    version: '0.9.0',
    auth: 'signed_in',
    version_status: 'unknown' as const,
    capabilities: codexCaps(),
  },
];

const reported = (id: string, extra: Partial<StationInfo> = {}): StationView =>
  station(id, {
    name: id === 'st-a' ? 'Alpha' : 'Bravo',
    info: { name: id, providers: providers(), ...extra } as unknown as StationInfo,
  });

const traits = { os: 'linux', arch: 'x86_64', labels: [] };
const stations = [reported('st-a', { traits }), reported('st-b', { traits })];

const home = (station_id: string, path: string, extra: Partial<ProjectHome> = {}): ProjectHome => ({
  station_id,
  generation: 1,
  position: station_id === 'st-a' ? 0 : 1,
  state: 'ready',
  path,
  reserved_path: null,
  attempt_id: null,
  sync_owner: false,
  code: null,
  message: null,
  ...extra,
});

const atlas = (): Project =>
  project({
    station_id: 'st-a',
    path: '/work/atlas',
    homes: [home('st-a', '/work/atlas'), home('st-b', '/srv/atlas')],
  });

describe('the helpers', () => {
  test('the Needs text is comma-separated labels without empty ones', () => {
    expect(parseNeeds(' gpu, os:linux ,, arch:aarch64 ')).toEqual([
      'gpu',
      'os:linux',
      'arch:aarch64',
    ]);
    expect(parseNeeds('')).toEqual([]);
  });

  test('Auto is offered when a station reports its traits', () => {
    expect(autoOffered(stations)).toBe(true);
    expect(autoOffered([reported('st-a'), reported('st-b')])).toBe(false);
  });

  test('the provider families of the stations', () => {
    expect(providerKinds(stations)).toEqual(['claude', 'codex']);
    expect(providerKinds([])).toEqual([]);
  });

  test('the project of a start is the one with a home at this station and path', () => {
    const p = atlas();
    expect(projectOfStart([p], 'st-b', '/srv/atlas')).toBe(p);
    expect(projectOfStart([p], 'st-b', '/work/atlas')).toBeUndefined();
    expect(projectOfStart([p], undefined, '/work/atlas')).toBeUndefined();
    const old = { ...p, homes: undefined } as unknown as Project;
    expect(projectOfStart([old], 'st-a', '/work/atlas')).toBeUndefined();
  });

  test('the ready homes of stations that can start now, by position', () => {
    const p = project({
      homes: [
        home('st-b', '/srv/atlas'),
        home('st-a', '/work/atlas'),
        home('st-c', '/c/atlas', { position: 2 }),
        home('st-d', '/d/atlas', { position: 3, state: 'cloning', path: null }),
      ],
    });
    expect(startHomes(p, stations).map((h) => h.path)).toEqual(['/work/atlas', '/srv/atlas']);
  });
});

describe('the placement of the starts', () => {
  const spec = { provider_instance: '', cwd: '/x', title: null };
  const run = (
    over: Partial<Parameters<typeof placeStarts>[0]> = {},
    place: SpawnActions['place'] = async () => ['st-b', 'st-a'],
  ) =>
    placeStarts({
      place,
      count: 2,
      needs: 'gpu',
      prefer: ['idle'],
      spread: true,
      kind: 'claude',
      project: atlas(),
      targets: stations,
      cwd: null,
      clone: null,
      spec,
      ...over,
    });

  test('each start gets its station, its instance and the home path of that station', async () => {
    const asked: unknown[] = [];
    const result = await run({}, async (body) => {
      asked.push(body);
      return ['st-b', 'st-a'];
    });
    expect(asked).toEqual([
      {
        count: 2,
        requires: ['gpu'],
        prefer: ['idle'],
        spread: true,
        provider: 'claude',
        project_id: 'p1',
      },
    ]);
    expect(result).toEqual({
      ok: true,
      starts: [
        { station: 'st-b', spec: { ...spec, provider_instance: 'claude', cwd: '/srv/atlas' } },
        { station: 'st-a', spec: { ...spec, provider_instance: 'claude', cwd: '/work/atlas' } },
      ] satisfies PlacedStart<typeof spec>[],
    });
  });

  test('without a project the typed directory is used and no project id is sent', async () => {
    const asked: Record<string, unknown>[] = [];
    const result = await run(
      { project: undefined, cwd: '/work/repo', count: 1, spread: true },
      async (body) => {
        asked.push({ ...body });
        return ['st-a'];
      },
    );
    expect(asked[0]).not.toHaveProperty('project_id');
    // A batch of one never spreads.
    expect(asked[0]?.['spread']).toBe(false);
    expect(result.ok && result.starts[0]?.spec.cwd).toBe('/work/repo');
  });

  test('a clone goes into a new folder below the directory', async () => {
    const result = await run({ clone: 'copy', count: 1 }, async () => ['st-a']);
    expect(result.ok && result.starts[0]?.spec.cwd).toBe('/work/atlas/copy');
  });

  test('the refusal of the deck is the text of the form', async () => {
    const refused = await run({}, async () => {
      throw new Error('No station matches: gpu.');
    });
    expect(refused).toEqual({ ok: false, text: 'No station matches: gpu.' });
    expect(await run({}, async () => ['st-a'])).toEqual({
      ok: false,
      text: 'The deck placed fewer stations than sessions.',
    });
  });

  test('a placed station without a signed-in instance of the family is refused', async () => {
    const signedOut = [reported('st-a', { traits }), reported('st-b', { traits })];
    const info = signedOut[1]?.info as StationInfo;
    signedOut[1] = {
      ...(signedOut[1] as StationView),
      info: { ...info, providers: providers('signed_out') },
    };
    const result = await run({ targets: signedOut, count: 1 }, async () => ['st-b']);
    expect(result).toEqual({
      ok: false,
      text: 'The station Bravo cannot start a claude session here.',
    });
  });
});

describe('the fields', () => {
  test('Needs, the three preferences and, for a batch, Spread', () => {
    const fields = (batch: boolean) =>
      renderToStaticMarkup(
        <PlacementFields
          needs="gpu"
          onNeeds={() => {}}
          prefer={['idle']}
          onPrefer={() => {}}
          batch={batch}
          spread
          onSpread={() => {}}
        />,
      );
    const one = fields(false);
    expect(one).toContain('Needs');
    expect(one).toContain('value="gpu"');
    for (const text of ['Idle station', 'Not on battery', 'Always-on station']) {
      expect(one).toContain(text);
    }
    expect(one).toContain('role="switch"');
    expect(one).not.toContain('Spread over stations');
    expect(fields(true)).toContain('Spread over stations');
  });
});

describe('the form', () => {
  const actions = (place = true): SpawnActions =>
    ({
      canDrive: true,
      list: async () => ({ path: '/', entries: [] }) as never,
      preflight: async () => ({ ok: false, text: '' }),
      start: async () => ({ ok: true, text: 'ok', sessionId: 's1' }),
      ...(place ? { place: async () => ['st-a'] } : {}),
    }) as SpawnActions;
  const form = (opts: { place?: boolean; project?: boolean; list?: StationView[] } = {}) => {
    const p = atlas();
    return renderToStaticMarkup(
      <IntegrationsProvider actions={fakeActions()} initial={opts.project === false ? [] : [p]}>
        <SpawnForm
          stations={opts.list ?? stations}
          freshness="live"
          actions={actions(opts.place ?? true)}
          onOpen={() => {}}
          initialStation="st-b"
          initialCwd="/srv/atlas"
          initialPlace="worktree"
        />
      </IntegrationsProvider>,
    );
  };

  test('a project start has the Home select with the ready homes and Auto', () => {
    const html = form();
    expect(html).toContain('Home');
    expect(html).toContain('Alpha · /work/atlas');
    expect(html).toContain('Bravo · /srv/atlas');
    expect(html).toContain(`value="${AUTO}"`);
    expect(html).toContain('Auto (best station)');
    // The home is the directory: no directory picker.
    expect(html).not.toContain('aria-label="Directory"');
    expect(html).toMatch(/<option value="st-b" selected="">Bravo · \/srv\/atlas/);
  });

  test('another start has the Station select, with Auto after the stations', () => {
    const html = form({ project: false });
    expect(html).not.toContain('Home');
    expect(html).toContain('Station');
    expect(html).toContain('Auto (best station)');
  });

  test('without a deck that places, or a station with traits, there is no Auto', () => {
    expect(form({ place: false })).not.toContain('Auto (best station)');
    expect(form({ list: [reported('st-a'), reported('st-b')] })).not.toContain(
      'Auto (best station)',
    );
  });
});
