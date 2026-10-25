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
import type {
  LandRow as Row,
  MoveTarget,
  MoveView,
  OtherBranches,
  RadarAnswer,
} from '../src/integrations/actions';
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

/** One land row that the fake deck may change. */
type LandRow = { -readonly [K in keyof Row]: Row[K] };

/** The lands of a project, as the fake deck keeps them. */
type ProjectLands = { queue: LandRow[]; recent: LandRow[] };

/** What the scripted deck saw and answers on the radar and the land routes (FG-7, FG-8). */
export type FakeLands = {
  /** `GET /api/radar` answers this; `null` answers 404 (an older deck). */
  radar: RadarAnswer | null;
  /** The lands of each project; the routes below change them. `null`: 404 (an older deck). */
  lands: Record<string, ProjectLands | null>;
  /** `METHOD path` of each POST, with its body. */
  calls: { call: string; body: unknown }[];
  /** The next POST answers this (status, JSON body) instead of acting; cleared after use. */
  refuse: { status: number; body: unknown } | null;
};

/**
 * The routes of the radar and of the lands of `projectId`. A land POST adds a queued row; cancel
 * moves a queued row to the recent ends; abandon marks the row; send and forget only answer.
 */
export function installFleetGitLands(
  deck: FakeDeck,
  projectId: string,
  initial: Partial<FakeLands> = {},
): FakeLands {
  const state: FakeLands = {
    radar: null,
    lands: { [projectId]: { queue: [], recent: [] } },
    calls: [],
    refuse: null,
    ...initial,
  };
  const take = () => {
    const refused = state.refuse;
    state.refuse = null;
    return refused;
  };
  const rowOf = (id: string): LandRow | undefined =>
    [...(state.lands[projectId]?.queue ?? []), ...(state.lands[projectId]?.recent ?? [])].find(
      (r) => r.land_id === id,
    );
  deck.on('/api/radar', () =>
    state.radar === null
      ? { status: 404, body: { error: 'not_found', message: 'No such route.' } }
      : { status: 200, body: state.radar },
  );
  deck.on(`/api/projects/${projectId}/lands`, () => {
    const lands = state.lands[projectId];
    return lands === null || lands === undefined
      ? { status: 404, body: { error: 'not_found', message: 'No such route.' } }
      : { status: 200, body: lands };
  });
  deck.route('POST', /^\/api\/sessions\/([^/]+)\/([^/]+)\/land$/, (m, raw) => {
    state.calls.push({ call: `POST ${m[0]}`, body: raw });
    const refused = take();
    if (refused !== null) return refused;
    const lands = state.lands[projectId];
    if (lands === null || lands === undefined) return { status: 404, body: { error: 'x' } };
    lands.queue.push({
      land_id: `land-${lands.queue.length + lands.recent.length + 1}`,
      session_id: decodeURIComponent(m[2] ?? ''),
      station_id: decodeURIComponent(m[1] ?? ''),
      branch: 'fd/alpha/ada',
      requested_by: 'owner',
      state: 'queued',
      reconciling: false,
      attempt: 0,
      files: [],
      position: 0,
      created_at: Date.now(),
    });
    return { status: 202, body: { land_id: `land-${lands.queue.length}`, position: 0 } };
  });
  deck.route('POST', /^\/api\/lands\/([^/]+)\/(cancel|abandon|forget|send)$/, (m, raw) => {
    state.calls.push({ call: `POST ${m[0]}`, body: raw });
    const refused = take();
    if (refused !== null) return refused;
    const id = decodeURIComponent(m[1] ?? '');
    const lands = state.lands[projectId];
    const found = rowOf(id);
    if (lands === null || lands === undefined || found === undefined) {
      return { status: 404, body: { error: 'unknown_land', message: 'Unknown land.' } };
    }
    if (m[2] === 'cancel') {
      lands.queue = lands.queue.filter((r) => r.land_id !== id);
      lands.recent.unshift({ ...found, state: 'cancelled', ended_at: Date.now() });
      return { status: 200, body: { state: 'cancelled' } };
    }
    if (m[2] === 'abandon') {
      const index = lands.queue.findIndex((r) => r.land_id === id);
      if (index >= 0) lands.queue[index] = { ...found, abandon_state: 'requested' };
      return { status: 202, body: { abandon_state: 'requested' } };
    }
    return { status: 200, body: { command_id: 'cmd-e2e', state: 'done' } };
  });
  return state;
}

/** What the scripted deck answers on the branches and fetch routes of a project (FG-10). */
export type FakeBranches = {
  /** `GET .../branches` answers this (status, JSON body). */
  list: { status: number; body: unknown };
  /** `POST .../fetch` answers this (status, JSON body). */
  fetch: { status: number; body: unknown };
  /** `METHOD path` of each call, in order. */
  calls: string[];
};

/** A 200 answer of the branches route. */
export const branchesAnswer = (
  branches: OtherBranches['branches'],
  extra: Partial<OtherBranches> = {},
): { status: number; body: OtherBranches } => ({
  status: 200,
  body: { remote: 'origin', default_branch: 'main', truncated: false, branches, ...extra },
});

/** The branches and fetch routes of `projectId`; a test changes `list` and `fetch` at any time. */
export function installFleetGitBranches(
  deck: FakeDeck,
  projectId: string,
  initial: Partial<FakeBranches> = {},
): FakeBranches {
  const state: FakeBranches = {
    list: branchesAnswer([]),
    fetch: { status: 200, body: {} },
    calls: [],
    ...initial,
  };
  deck.on(`/api/projects/${projectId}/branches`, () => {
    state.calls.push(`GET /api/projects/${projectId}/branches`);
    return state.list;
  });
  deck.route('POST', new RegExp(`^/api/projects/${projectId}/fetch$`), (m) => {
    state.calls.push(`POST ${m[0]}`);
    return state.fetch;
  });
  return state;
}

/** What the scripted deck answers on the move routes of a session (FG-9). */
export type FakeMoves = {
  /** `GET .../move` answers this body (the newest move and the stations of the dialog). */
  read: { move: MoveView | null; targets: readonly MoveTarget[] };
  /** `METHOD path` of each call, with its body. */
  calls: { call: string; body: unknown }[];
  /** The next POST answers this (status, JSON body) instead of acting; cleared after use. */
  refuse: { status: number; body: unknown } | null;
};

/**
 * The move routes of one session: GET reads `read`; POST move adds a `holding` move for the chosen station; POST
 * undo sets the move `undoing`. A test changes `read.move` to script the progress.
 */
export function installFleetGitMoves(
  deck: FakeDeck,
  session: { readonly station: string; readonly session: string },
  initial: Partial<FakeMoves> = {},
): FakeMoves {
  const state: FakeMoves = {
    read: { move: null, targets: [] },
    calls: [],
    refuse: null,
    ...initial,
  };
  const take = () => {
    const refused = state.refuse;
    state.refuse = null;
    return refused;
  };
  deck.on(`/api/sessions/${session.station}/${session.session}/move`, () => ({
    status: 200,
    body: state.read,
  }));
  deck.route('POST', /^\/api\/sessions\/([^/]+)\/([^/]+)\/move$/, (m, raw) => {
    state.calls.push({ call: `POST ${m[0]}`, body: raw });
    const refused = take();
    if (refused !== null) return refused;
    const to = (raw as { to_station?: string }).to_station ?? '';
    state.read.move = {
      move_id: 'mv-1',
      state: 'holding',
      kind: state.read.targets.find((t) => t.station_id === to)?.kind ?? 'native',
      src_station: decodeURIComponent(m[1] ?? ''),
      src_session: decodeURIComponent(m[2] ?? ''),
      dst_station: to,
      created_at: Date.now(),
    };
    return { status: 202, body: { move_id: 'mv-1' } };
  });
  deck.route('POST', /^\/api\/moves\/([^/]+)\/undo$/, (m, raw) => {
    state.calls.push({ call: `POST ${m[0]}`, body: raw });
    const refused = take();
    if (refused !== null) return refused;
    if (state.read.move !== null) state.read.move = { ...state.read.move, state: 'undoing' };
    return { status: 202, body: { move_id: decodeURIComponent(m[1] ?? ''), state: 'undoing' } };
  });
  return state;
}
