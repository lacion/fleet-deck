// FG-1..FG-4: the scripted Fleet Git routes of the browser tests. The projects list, the PUT of the
// Git settings and the Push now route. `fake-deck.ts` parses no PUT body, so this file answers the
// PUT with its own page route (the last route registered answers first).
import type { Page } from '@playwright/test';
import type { DeckSession, EventEnvelope, GitSettingsPut, Project, PushInfo } from '../src/gen';
import { project } from '../src/integrations/fixtures';
import { summary } from '../src/testing/summary';
import { CSRF, type FakeDeck } from './fake-deck';

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
