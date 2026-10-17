// FG-5, FG-6 (design §8.5, §9.3): the start dialog picks a home of the project or Auto.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { Project, ProjectHome, StartedSessions, StationInfo, StationView } from '../gen';
import { station } from '../testing/fleet';
import { fakeActions, project } from './fixtures';
import {
  autoStations,
  initialHome,
  readyHomes,
  StartDialog,
  StartResult,
  startBody,
  startProviders,
} from './StartDialog';

const noop = () => {};
const traits = { os: 'linux', arch: 'x86_64', labels: [] };

const reported = (
  id: string,
  instances: readonly string[],
  extra: Partial<StationInfo> = {},
): StationView =>
  station(id, {
    name: id === 'station-a' ? 'Alpha' : 'Beta',
    info: {
      providers: instances.map((instance) => ({
        instance,
        provider: instance.startsWith('codex') ? 'codex' : 'claude',
      })),
      ...extra,
    } as unknown as StationInfo,
  });

const home = (station_id: string, position: number, extra: Partial<ProjectHome> = {}) =>
  ({
    ...project().homes[0],
    station_id,
    position,
    path: `/work/${station_id}`,
    ...extra,
  }) as ProjectHome;

const twoHomes = (extra: Partial<Project> = {}) =>
  project({ homes: [home('station-b', 1), home('station-a', 0)], ...extra });

const stations = [
  reported('station-a', ['claude-1', 'codex-1']),
  reported('station-b', ['codex-1', 'claude-2']),
];

const render = (p: Project, s: StationView[] = stations, issues = ['PROJ-1', 'PROJ-2']) =>
  renderToStaticMarkup(
    <StartDialog project={p} actions={fakeActions()} stations={s} issues={issues} onClose={noop} />,
  );

const body = (extra: Partial<Parameters<typeof startBody>[0]> = {}) =>
  startBody({
    batchId: 'b1',
    issues: ['PROJ-1', 'PROJ-2'],
    review: undefined,
    instance: 'claude-1',
    mode: '',
    model: '',
    count: 1,
    home: 'station-a',
    primary: 'station-a',
    needs: '',
    prefer: [],
    spread: false,
    ...extra,
  });

describe('start dialog homes', () => {
  test('an older deck sends no homes: no Home select, the providers of the project station', () => {
    const older = project();
    (older as { homes?: unknown }).homes = undefined;
    const html = render(older);
    expect(html).not.toContain('Home');
    expect(html).toContain('<option value="claude-1"');
    expect(html).not.toContain('Auto (best station)');
    const none = render(project({ homes: [home('station-a', 0, { state: 'unverified' })] }));
    expect(none).not.toContain('Home');
  });

  test('the options in position order, the position-0 home selected, no Auto without traits', () => {
    const html = render(twoHomes());
    expect(html).toContain('Home');
    const a = html.indexOf('Alpha · /work/station-a');
    const b = html.indexOf('Beta · /work/station-b');
    expect(a).toBeGreaterThan(-1);
    expect(b).toBeGreaterThan(a);
    expect(html).toMatch(/<option value="station-a" selected[^>]*>Alpha/);
    expect(html).not.toContain('Auto (best station)');
  });

  test('a home that is not ready is not an option; the first ready home starts selected', () => {
    const p = twoHomes({
      homes: [home('station-a', 0, { state: 'unverified' }), home('station-b', 1)],
    });
    const html = render(p);
    expect(html).not.toContain('Alpha · /work/station-a');
    expect(html).toMatch(/<option value="station-b" selected[^>]*>Beta/);
    expect(initialHome(readyHomes(p))).toBe('station-b');
    expect(initialHome(readyHomes(twoHomes()))).toBe('station-a');
  });

  test('a station that is not in the list shows its id', () => {
    const html = render(twoHomes(), [stations[0] as StationView]);
    expect(html).toContain('station-b · /work/station-b');
  });

  test('Auto is offered when a station with a ready home reports its traits', () => {
    const withTraits = [stations[0] as StationView, reported('station-b', ['codex-1'], { traits })];
    const html = render(twoHomes(), withTraits);
    expect(html).toContain('<option value="auto">Auto (best station)</option>');
    // A station that reports traits but has no ready home of the project does not count.
    const other = [...stations, reported('station-c', ['claude-9'], { traits })];
    expect(render(twoHomes(), other)).not.toContain('Auto (best station)');
    expect(autoStations(readyHomes(twoHomes()), other).map((s) => s.station_id)).toEqual([
      'station-a',
      'station-b',
    ]);
  });

  test('the instances: the station of a home, and under Auto without duplicates', () => {
    const ready = readyHomes(twoHomes());
    const ids = (h: string) =>
      startProviders(twoHomes(), stations, ready, h).map((p) => p.instance);
    expect(ids('station-a')).toEqual(['claude-1', 'codex-1']);
    expect(ids('station-b')).toEqual(['codex-1', 'claude-2']);
    expect(ids('auto')).toEqual(['claude-1', 'codex-1', 'claude-2']);
    expect(ids('unknown-station')).toEqual([]);
    const without = startProviders(project({ homes: [] }), stations, [], 'x');
    expect(without.map((p) => p.instance)).toEqual(['claude-1', 'codex-1']);
  });

  test('the dialog shows the instances of the initial home', () => {
    const html = render(twoHomes({ homes: [home('station-b', 0), home('station-a', 1)] }));
    expect(html).toContain('<option value="claude-2"');
    expect(html).not.toContain('<option value="claude-1"');
    // The chosen instance of the first render is the first one of the list.
    expect(html).toMatch(/<option value="codex-1" selected/);
  });

  test('the body: no home for the position-0 station, a station id for another home', () => {
    expect('home' in body()).toBe(false);
    expect(body({ home: 'station-b' }).home).toBe('station-b');
    expect('home' in body({ home: null, primary: undefined })).toBe(false);
    // The position-0 home is not ready: the first ready home is sent.
    expect(body({ home: 'station-b', primary: undefined }).home).toBe('station-b');
    expect(body({ home: 'station-b' })).not.toHaveProperty('requires');
    expect(body({ home: 'station-b', needs: 'gpu', prefer: ['idle'], spread: true })).toEqual(
      body({ home: 'station-b' }),
    );
  });

  test('under Auto: requires, prefer, and spread for a batch of two or more', () => {
    const auto = body({
      home: 'auto',
      needs: ' gpu, os:linux ,, ',
      prefer: ['idle', 'always_on'],
      spread: true,
    });
    expect(auto.home).toBe('auto');
    expect(auto.requires).toEqual(['gpu', 'os:linux']);
    expect(auto.prefer).toEqual(['idle', 'always_on']);
    expect(auto.spread).toBe(true);
    expect(body({ home: 'auto', spread: false }).spread).toBe(false);
    // One issue and one session: a batch of one sends no spread.
    const one = body({ home: 'auto', issues: ['PROJ-1'], count: 1, spread: true });
    expect(one.requires).toEqual([]);
    expect(one.prefer).toEqual([]);
    expect(one).not.toHaveProperty('spread');
    // Issues times count is the batch.
    expect(body({ home: 'auto', issues: ['PROJ-1'], count: 3, spread: true }).spread).toBe(true);
    // A review is one session.
    const review = body({ home: 'auto', issues: [], review: 7, count: 5, spread: true });
    expect(review).not.toHaveProperty('spread');
    expect(review.review_of_pr).toBe(7);
    expect(review.count_per_issue).toBe(1);
  });

  test('the placement fields show under Auto only (the first render shows the home)', () => {
    const html = render(twoHomes());
    expect(html).not.toContain('Spread over stations');
    expect(html).not.toContain('Needs');
  });
});

describe('start result links', () => {
  const result: StartedSessions = {
    started: [
      {
        key: 'PROJ-1',
        command: {
          station_id: 'station-b',
          receipt: { result: { session_id: 'sess-1' } },
        } as never,
      },
      {
        key: 'PROJ-2',
        command: { receipt: { result: { session_id: 'sess-2' } } } as never,
      },
    ],
    failed: [],
  };

  test('each link uses the station of its command; the prop is the fallback', () => {
    const html = renderToStaticMarkup(<StartResult station="station-a" result={result} />);
    expect(html).toContain('href="#/s/station-b/sess-1"');
    expect(html).toContain('href="#/s/station-a/sess-2"');
  });
});
