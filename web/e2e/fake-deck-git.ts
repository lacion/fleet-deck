// FG-1..FG-4: the scripted Fleet Git routes of the browser tests. The projects list, the PUT of the
// Git settings and the Push now route. `fake-deck.ts` parses no PUT body, so this file answers the
// PUT with its own page route (the last route registered answers first).
import type { Page } from '@playwright/test';
import type {
  DeckSession,
  EventEnvelope,
  GitSettingsPut,
  Project,
  ProjectHome,
  PushInfo,
  StationInfo,
  StationView,
} from '../src/gen';
import { project } from '../src/integrations/fixtures';
import { summary } from '../src/testing/summary';
import { claudeCaps } from '../src/testing/capabilities';
import { CSRF, type FakeDeck, stationView } from './fake-deck';

/** The default project of the tests: station `st-alpha`, sync off. */
export const gitProject = (extra: Partial<Project> = {}): Project =>
  project({
    project_id: 'p1',
    name: 'Atlas',
    station_id: 'st-alpha',
    forge: null,
    trackers: [],
    ...extra,
  });

/** What the scripted deck saw and what it answers. */
export type FakeGit = {
  /** The bodies of the PUTs to `/api/projects/{id}/git`, in order. */
  puts: GitSettingsPut[];
  /** The paths of the POSTs to `/api/sessions/{station}/{session}/push`. */
  pushes: string[];
  /** The next PUT answers this (status, JSON body) instead of saving; cleared after use. */
  refuse: { status: number; body: unknown } | null;
};

/**
 * The routes of the Git settings of `p` and of Push now. The PUT saves the settings in memory and
 * answers `{ git, git_revision }` with the revision one higher.
 */
export async function installFleetGit(
  page: Page,
  deck: FakeDeck,
  projects: Project[],
): Promise<FakeGit> {
  const state: FakeGit = { puts: [], pushes: [], refuse: null };
  deck.on('/api/projects', () => ({ status: 200, body: { projects } }));
  await page.route(/\/api\/projects\/[^/]+\/git$/, async (route) => {
    const request = route.request();
    if (request.method() !== 'PUT') return route.fallback();
    const id = decodeURIComponent(new URL(request.url()).pathname.split('/')[3] ?? '');
    const found = projects.find((p) => p.project_id === id);
    if (request.headers()['x-csrf-token'] !== CSRF) {
      return route.fulfill({
        status: 403,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'csrf', message: 'Missing CSRF token.' }),
      });
    }
    const body = request.postDataJSON() as GitSettingsPut;
    state.puts.push(body);
    const refused = state.refuse;
    state.refuse = null;
    if (refused !== null || found === undefined) {
      return route.fulfill({
        status: refused?.status ?? 404,
        contentType: 'application/json',
        body: JSON.stringify(refused?.body ?? { error: 'unknown_project', message: id }),
      });
    }
    found.git = body.settings;
    found.git_revision += 1;
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ git: found.git, git_revision: found.git_revision }),
    });
  });
  deck.route('POST', /^\/api\/sessions\/([^/]+)\/([^/]+)\/push$/, (match) => {
    state.pushes.push(match[0]);
    return { status: 200, body: { state: 'due' } };
  });
  return state;
}

/** A session card of `st-alpha` with a push state and, when given, a stale base. */
export function gitSession(
  id: string,
  title: string,
  push: PushInfo | undefined,
  extra: Partial<DeckSession['summary']> = {},
): DeckSession {
  return {
    station_id: 'st-alpha',
    status: 'idle',
    summary: summary(id, 'idle', 1_760_000_000_000, {
      title,
      cwd: '/work/atlas',
      branch: 'fd/alpha/ada',
      ...(push === undefined ? {} : { push }),
      ...extra,
    }),
  };
}

/** Timeline events of a session, numbered after `after`. */
export function gitEvents(
  session: string,
  events: EventEnvelope['event'][],
  after = 0,
): EventEnvelope[] {
  return events.map((event, i) => ({
    station_seq: 900 + after + i + 1,
    session_id: session,
    session_seq: after + i + 1,
    at: 1_760_000_000_000 + 1000 * (after + i + 1),
    proto_version: 1,
    raw_ref: null,
    event,
  }));
}

/** A station that reports Fleet Git (and, with `extra`, traits, config and Taildrive). */
export function gitStation(
  id: string,
  name: string,
  extra: Partial<StationInfo> = {},
  view: Partial<StationView> = {},
): StationView {
  return stationView(id, name, {
    info: {
      name,
      incarnation: `inc-${id}`,
      head: 1,
      live_sessions: 0,
      version: '0.3.0',
      max_sessions: 4,
      allow_shell: null,
      providers: [
        {
          instance: 'personal',
          provider: 'claude',
          version: '2.1.5',
          auth: 'signed_in',
          version_status: 'unknown',
          capabilities: claudeCaps(),
        },
      ],
      fleet_git: {
        clone_root: `/srv/${id}/projects`,
        allow_check_cmd: false,
        allow_local_remotes: false,
      },
      ...extra,
    },
    info_at: 1_760_000_000_000,
    ...view,
  });
}

/** A home of a project on `station`. */
export function gitHome(station: string, extra: Partial<ProjectHome> = {}): ProjectHome {
  return {
    station_id: station,
    generation: 1,
    position: station === 'st-alpha' ? 0 : 1,
    state: 'ready',
    path: `/srv/${station}/atlas`,
    reserved_path: null,
    attempt_id: null,
    sync_owner: false,
    code: null,
    message: null,
    ...extra,
  };
}

/** What the scripted deck saw on the home routes. */
export type FakeHomes = {
  /** `METHOD path` of each call, with its body. */
  calls: { call: string; body: unknown }[];
  /** The next home call answers this (status, JSON body) instead of acting; cleared after use. */
  refuse: { status: number; body: unknown } | null;
};

const json = (status: number, body: unknown) => ({
  status,
  contentType: 'application/json',
  body: JSON.stringify(body),
});

/**
 * The home routes of the projects in `projects` (design §8.4): add makes a `cloning` home,
 * retry makes a failed home `cloning`, remove deletes the home, and `PUT /remote` stores the URL
 * (or asks for the rebind when `rebindFrom` is set and the URL differs). `settle(station)` makes a
 * cloning home ready, as the station does when the clone ends.
 */
export async function installFleetGitHomes(
  page: Page,
  deck: FakeDeck,
  projects: Project[],
  options: { rebindFrom?: string } = {},
): Promise<FakeHomes & { settle: (project: string, station: string) => void }> {
  const state: FakeHomes = { calls: [], refuse: null };
  const find = (id: string) => projects.find((p) => p.project_id === id);
  const take = () => {
    const refused = state.refuse;
    state.refuse = null;
    return refused;
  };
  deck.route('POST', /^\/api\/projects\/([^/]+)\/homes$/, (m, raw) => {
    state.calls.push({ call: `POST ${m[0]}`, body: raw });
    const refused = take();
    if (refused !== null) return refused;
    const body = raw as { station: string };
    const p = find(decodeURIComponent(m[1] ?? ''));
    if (p === undefined) return { status: 404, body: { error: 'unknown_project' } };
    const root = `/srv/${body.station}/projects`;
    p.homes.push(
      gitHome(body.station, {
        state: 'cloning',
        path: null,
        reserved_path: `${root}/${p.name}`,
        position: p.homes.length,
      }),
    );
    return { status: 202, body: { generation: 1, attempt_id: 'ha-e2e' } };
  });
  deck.route('POST', /^\/api\/projects\/([^/]+)\/homes\/([^/]+)\/retry$/, (m, raw) => {
    state.calls.push({ call: `POST ${m[0]}`, body: raw });
    const refused = take();
    if (refused !== null) return refused;
    const home = find(decodeURIComponent(m[1] ?? ''))?.homes.find(
      (h) => h.station_id === decodeURIComponent(m[2] ?? ''),
    );
    if (home === undefined) return { status: 404, body: { error: 'unknown_home' } };
    home.state = 'cloning';
    home.code = null;
    home.message = null;
    home.reserved_path = `/srv/${home.station_id}/projects/retry`;
    return { status: 202, body: { generation: 1, attempt_id: 'ha-e2e-2' } };
  });
  deck.route('DELETE', /^\/api\/projects\/([^/]+)\/homes\/([^/]+)$/, (m) => {
    state.calls.push({ call: `DELETE ${m[0]}`, body: null });
    const refused = take();
    if (refused !== null) return refused;
    const p = find(decodeURIComponent(m[1] ?? ''));
    if (p === undefined) return { status: 404, body: { error: 'unknown_project' } };
    p.homes = p.homes.filter((h) => h.station_id !== decodeURIComponent(m[2] ?? ''));
    return { status: 204, body: null };
  });
  await page.route(/\/api\/projects\/[^/]+\/remote$/, async (route) => {
    const request = route.request();
    if (request.method() !== 'PUT') return route.fallback();
    const id = decodeURIComponent(new URL(request.url()).pathname.split('/')[3] ?? '');
    const body = request.postDataJSON() as { remote_url: string; rebind: boolean };
    state.calls.push({ call: `PUT ${new URL(request.url()).pathname}`, body });
    const refused = take();
    if (refused !== null) return route.fulfill(json(refused.status, refused.body));
    const p = find(id);
    if (p === undefined) return route.fulfill(json(404, { error: 'unknown_project' }));
    if (options.rebindFrom !== undefined && p.repo === options.rebindFrom && !body.rebind) {
      return route.fulfill(
        json(409, {
          error: 'rebind_needed',
          message: `This URL names another repository than ${p.repo}. Confirm the rebind; every home must match the new remote before sync runs again.`,
        }),
      );
    }
    p.remote_url = body.remote_url;
    p.repo = body.remote_url.replace(/^https?:\/\//, '').replace(/\.git$/, '');
    p.git_revision += 1;
    return route.fulfill(
      json(200, {
        remote_url: p.remote_url,
        remote_ssh_user: null,
        repo: p.repo,
        remote_dest: p.remote_url,
        git_revision: p.git_revision,
      }),
    );
  });
  const settle = (project: string, station: string) => {
    const home = find(project)?.homes.find((h) => h.station_id === station);
    if (home === undefined) return;
    home.state = 'ready';
    home.path = home.reserved_path;
    home.reserved_path = null;
  };
  return Object.assign(state, { settle });
}

/** What the scripted deck saw on the placement and the config reference routes. */
export type FakePlacement = {
  /** The bodies of `POST /api/placement`, in order. */
  placements: unknown[];
  /** The stations that the next placement answers; or a refusal. */
  answer: string[] | { status: number; body: unknown };
  /** The config reference station, as `GET /api/fleet-git/prefs` answers it. */
  reference: string | null;
  /** The bodies of the PUTs to the drift reference route. */
  references: unknown[];
};

/** `POST /api/placement`, `GET /api/fleet-git/prefs` and `PUT /api/fleet-git/drift-reference`. */
export async function installFleetGitPlacement(
  page: Page,
  deck: FakeDeck,
  initial: Partial<FakePlacement> = {},
): Promise<FakePlacement> {
  const state: FakePlacement = {
    placements: [],
    answer: [],
    reference: null,
    references: [],
    ...initial,
  };
  deck.on('/api/placement', (raw) => {
    state.placements.push(raw);
    return Array.isArray(state.answer)
      ? { status: 200, body: { stations: state.answer } }
      : state.answer;
  });
  deck.on('/api/fleet-git/prefs', () => ({
    status: 200,
    body: { drift_reference: state.reference },
  }));
  await page.route(/\/api\/fleet-git\/drift-reference$/, async (route) => {
    const request = route.request();
    if (request.method() !== 'PUT') return route.fallback();
    const body = request.postDataJSON() as { station: string | null };
    state.references.push(body);
    state.reference = body.station;
    return route.fulfill(json(200, { drift_reference: state.reference }));
  });
  return state;
}
