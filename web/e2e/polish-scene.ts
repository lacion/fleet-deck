// The scene of the UI polish tests (polish.spec.ts): the gallery deck plus the routes that the
// polish checks need: the folders of the New session picker, the coordinator grants, a tailnet
// computer to add, and the secret files of a project with a ready home.
import type { GrantView, WorkspaceListing } from '../src/gen';
import { project } from '../src/integrations/fixtures';
import type { FakeDeck, FakeTailnet } from './fake-deck';
import { galleryDeck } from './gallery-scene';
import { DEV1, FD, FD_REPO, LIGHTING, MAC } from './gallery-data';

export { LIGHTING };

const ROOT = '/home/me/projects';
/** The folders of every station: the roots, `projects`, and `projects/app`. */
const LISTINGS: Record<string, WorkspaceListing> = {
  '': {
    path: null,
    parent: null,
    entries: [{ name: 'projects', path: ROOT, repository: false }],
    truncated: false,
  },
  [ROOT]: {
    path: ROOT,
    parent: null,
    entries: [
      { name: 'app', path: `${ROOT}/app`, repository: true },
      { name: 'notes', path: `${ROOT}/notes`, repository: false },
    ],
    truncated: false,
  },
  [`${ROOT}/app`]: { path: `${ROOT}/app`, parent: ROOT, entries: [], truncated: false },
};

/** The coordinator grants in memory: `g-live` is active, `g-idle` expired after idle time. */
export type FakeGrants = { rows: GrantView[] };

function grant(id: string, state: string, reason: string | null): GrantView {
  return {
    grant_id: id,
    station_id: LIGHTING,
    session_id: `coord-${id}`,
    classes: ['read', 'mail'],
    stations: [LIGHTING],
    spawn_cap: 3,
    spawn_used: 0,
    state,
    revoked_reason: reason,
    generation: 1,
    expires_at: Date.now() + 3_600_000,
    last_used_at: null,
  };
}

/** The project of the secret files: one ready home on Lighting. */
export const SECRET_PROJECT = 'p-fleet-deck';

export type PolishDeck = { deck: FakeDeck; grants: FakeGrants; tailnet: FakeTailnet };

export function polishDeck(): PolishDeck {
  const deck = galleryDeck();
  for (const st of [LIGHTING, DEV1, MAC]) {
    deck.on(`/api/stations/${st}/workspaces`, (_body, query) => {
      const listing = LISTINGS[query.get('path') ?? ''];
      return listing === undefined
        ? { status: 400, body: { code: 'outside_roots', message: 'Outside the roots.' } }
        : { status: 200, body: listing };
    });
  }
  const grants: FakeGrants = {
    rows: [grant('g-live', 'active', null), grant('g-idle', 'expired', 'idle')],
  };
  const find = (id: string) => grants.rows.find((g) => g.grant_id === id);
  deck.on('/api/coordinator/grants', () => ({ status: 200, body: grants.rows }));
  deck.on('/api/coordinator/grants/g-live/revoke', () => {
    const g = find('g-live');
    if (g !== undefined) Object.assign(g, { state: 'revoked', revoked_reason: 'owner' });
    return { status: 200, body: g };
  });
  deck.on('/api/coordinator/grants/g-idle/renew', () => {
    const g = find('g-idle');
    if (g !== undefined) Object.assign(g, { state: 'active', revoked_reason: null });
    return { status: 200, body: g };
  });
  // A second `tailnet` call replaces the routes of the gallery's: one computer can be added.
  const tailnet = deck.tailnet({
    devices: [
      {
        host_name: 'lab-box',
        dns_name: 'lab-box.tail1234.ts.net',
        ips: ['100.64.0.9'],
        os: 'linux',
        tags: [],
        online: true,
        state: 'none',
      },
    ],
    requests: [],
  });
  const home = {
    station_id: LIGHTING,
    generation: 1,
    position: 0,
    state: 'ready' as const,
    path: FD,
    reserved_path: null,
    attempt_id: null,
    sync_owner: false,
    code: null,
    message: null,
  };
  deck.on('/api/projects', () => ({
    status: 200,
    body: {
      projects: [
        project({
          project_id: SECRET_PROJECT,
          name: 'fleet-deck',
          station_id: LIGHTING,
          path: FD,
          forge: { kind: 'github', host: 'github.com', repo: 'lacion/fleet-deck' },
          trackers: [],
          repo: FD_REPO,
          remote_url: `https://${FD_REPO}.git`,
          homes: [home],
        }),
      ],
    },
  }));
  const base = `/api/projects/${SECRET_PROJECT}/secret-files`;
  deck.on(base, () => ({ status: 200, body: { paths: [], files: [] } }));
  deck.on(`${base}/browse`, (_body, query) => {
    const dir = query.get('dir') ?? '';
    const entries =
      dir === ''
        ? [{ name: 'config', kind: 'dir', eligible: false }]
        : dir === 'config'
          ? [{ name: 'local', kind: 'dir', eligible: false }]
          : [{ name: '.env', kind: 'file', eligible: true, size: 120 }];
    return { status: 200, body: { dir, entries, truncated: false } };
  });
  return { deck, grants, tailnet };
}
