// The UI gallery scene (opt-in: FD_GALLERY=1): the fake deck of every board page. `galleryDeck` is
// the busy deck of docs/v2/design/ui-refresh/concepts/BRIEF.md §4, `{ needsYou: false }` the
// all-clear deck. The data is in gallery-data.ts.
import type { PrSummary, TaskView } from '../src/gen';
import { project } from '../src/integrations/fixtures';
import { FakeDeck } from './fake-deck';
import {
  activity,
  artifactDetail,
  artifacts,
  DAY,
  deckSession,
  DEV1,
  FD,
  FD_REPO,
  HOUR,
  id,
  LIGHTING,
  MAC,
  MIN,
  OTTER,
  otterTimeline,
  plan,
  pr128,
  RA,
  RA_REPO,
  REAPER_PATCH,
  rows,
  schedule,
  stationFields,
  stations,
  stationValues,
  TOKENS,
  TREE,
  TURN,
} from './gallery-data';

export { LIGHTING, OTTER };

/** The deck of the gallery: every page has its answer. `needsYou: false`: the requests are answered. */
export function galleryDeck(options: { needsYou?: boolean } = {}): FakeDeck {
  const now = Date.now();
  const needsYou = options.needsYou !== false;
  const deck = new FakeDeck();
  deck.stations = stations(now);
  deck.sessions = rows(now, needsYou).map((r) => deckSession(now, r));
  deck.tailnet();
  if (needsYou) deck.emit(LIGHTING, OTTER, otterTimeline(now));
  const shell = {
    shell_id: 'sh-1',
    cwd: FD,
    state: 'running',
    cols: 120,
    rows: 32,
    created_at: now - 12 * MIN,
    exit_code: null,
  } as const;
  deck.shells(LIGHTING).rows.push(shell);
  const ok = (body: unknown) => () => ({ status: 200, body });
  // The stations page asks for the shells and the recording of each station by its name.
  for (const name of ['Lighting', 'dev1', 'mac-m4']) {
    deck.shells(name);
    deck.on(`/api/stations/${name}/desktop/recording`, ok({ state: 'idle' }));
  }
  const task: TaskView = {
    task_id: 'task-1',
    title: 'Add webhook retry tests',
    creator: 'owner',
    station_id: DEV1,
    session_id: id('kite-x81d'),
    state: 'open',
    result: null,
    completion_reason: null,
    question: null,
    delivery: 'delivered',
    reason: null,
    created_at: now - 40 * MIN,
    updated_at: now - 40 * MIN,
    completed_at: null,
  };
  const touch = (callsign: string, title: string, ago: number) => ({
    station_id: LIGHTING,
    session_id: id(callsign),
    session_title: title,
    path: TOKENS,
    touched_at: now - ago * MIN,
  });
  const extras = (callsign: string, mail: number, file: string) => ({
    station_id: LIGHTING,
    session_id: id(callsign),
    mail_pending: mail,
    files: [file],
  });
  const share = {
    share_id: 'share-1',
    kind: 'session',
    target: `${LIGHTING}/${id('falcon-vf9n')}`,
    role: 'viewer',
    steering: 'open',
    history: false,
    expires_at: now + 2 * DAY,
    revoked_at: null,
    created_at: now - 3 * HOUR,
    uses_left: 1,
    members: [],
  };
  const steps1 = [
    'Write one brief for three concepts.',
    'Render the six artboards of each concept.',
    'Compare the concepts and pick one.',
  ];
  const steps2 = [
    'Move the totals into `cart.ts`.',
    'Add a test for a cart with a discount.',
    'Open the pull request.',
  ];
  const conflict = {
    repo: FD_REPO,
    file: TOKENS,
    touches: [
      touch('falcon-vf9n', 'Fleet Deck UI', 20),
      touch('heron-mc2q', 'Omarchy integration docs', 50),
    ],
  };
  deck.on('/api/tasks', ok([task]));
  deck.on('/api/mail', ok({}));
  deck.on('/api/radar', ok({ groups: [], notices: [] }));
  deck.on('/api/archive', ok({ archived: [] }));
  deck.on('/api/spaces', ok([]));
  deck.on(
    '/api/features',
    ok({ features: ['desktop_capture.v1', 'desktop_recording.v1', 'artifacts_video.v1'] }),
  );
  deck.on(
    '/api/fleet/extras',
    ok({
      sessions: [extras('heron-mc2q', 1, 'docs/omarchy.md'), extras('falcon-vf9n', 0, TOKENS)],
    }),
  );
  deck.on('/api/conflicts', ok([conflict]));
  deck.on('/api/activity', () => ({ status: 200, body: { items: activity(now, deck.sessions) } }));
  deck.on(
    '/api/plans',
    ok([
      plan(LIGHTING, 'falcon-vf9n', 1, 'New board: concept briefs', steps1, 45),
      plan(DEV1, 'lynx-4kq2', 2, 'Checkout: cart totals', steps2, 120),
    ]),
  );
  deck.on(
    '/api/schedules',
    ok([
      schedule(
        1,
        'Nightly dependency check',
        '0 3 * * *',
        LIGHTING,
        'Check the lockfiles for outdated dependencies.',
        now,
      ),
      schedule(
        2,
        'Weekly release notes draft',
        '0 9 * * 1',
        MAC,
        'Draft the release notes from the merged pull requests.',
        now,
      ),
    ]),
  );
  deck.on('/api/artifacts', (_body, query) => ({
    status: 200,
    body: artifacts(now).filter((a) => (query.get('session') ?? a.session_id) === a.session_id),
  }));
  deck.on('/api/shares', ok([share]));
  // The page of one artifact (gallery 27): its detail and the view link of its frame.
  deck.on(`/api/artifacts/${LIGHTING}/art-docs`, ok(artifactDetail(now)));
  deck.on(
    `/api/artifacts/${LIGHTING}/art-docs/view`,
    ok({ url: '/a/gallery-docs/', expires_at: now + HOUR }),
  );
  const repo = (pid: string, name: string, st: string, path: string, origin: string) =>
    project({
      project_id: pid,
      name,
      station_id: st,
      path,
      forge: { kind: 'github', host: 'github.com', repo: origin.replace('github.com/', '') },
      trackers: [],
      repo: origin,
      remote_url: `https://${origin}.git`,
    });
  deck.on(
    '/api/projects',
    ok({
      projects: [
        repo('p-fleet-deck', 'fleet-deck', LIGHTING, FD, FD_REPO),
        repo('p-rannoch', 'rannoch', DEV1, RA, RA_REPO),
      ],
    }),
  );
  const prs = (mine: PrSummary[]) =>
    ok({ groups: { review_requested: [], mine, pending: [], recent_closed: [] } });
  deck.on('/api/projects/p-fleet-deck/prs', prs([]));
  deck.on('/api/projects/p-rannoch/prs', prs([pr128(now)]));
  const station = `/api/stations/${LIGHTING}`;
  deck.on(
    `${station}/settings`,
    ok({ values: stationValues, fields: stationFields, revision: 'rev-1', overrides: [] }),
  );
  const at = (query: URLSearchParams) => query.get('path') ?? '';
  deck.on(`${station}/files/list`, (_body, q) => ({
    status: 200,
    body: { root: FD, path: at(q), entries: TREE[at(q)] ?? TREE['crates'], truncated: false },
  }));
  deck.on(`${station}/files/read`, (_body, q) => ({
    status: 200,
    body: { path: at(q), size: 5230, binary: false, text: `// ${at(q)}\n`, truncated: false },
  }));
  // The turn diff, the diff between two checkpoints and the diff since the start: the same patch.
  const session = `${station}/sessions/${OTTER}`;
  const diff = {
    session_id: OTTER,
    turn_id: TURN,
    from_commit: 'otter-start',
    to_commit: 'otter-end',
    base_oid: 'a1b2c3d4e5f6',
    scope: 'worktree',
    summary: { files_changed: 1, insertions: 9, deletions: 3 },
    patch: REAPER_PATCH,
    truncated: false,
  };
  for (const suffix of [`turns/${TURN}/diff`, 'turn-diff', 'diff'])
    deck.on(`${session}/${suffix}`, ok(diff));
  // Since the start, a second file with a long name (60 characters): a phone Files tab must not
  // pan sideways (gallery 05 checks it).
  const LONG = 'web/src/session/session-files-tab-overflow-check-with-a-sixty-char-names.tsx';
  const longPatch = `${REAPER_PATCH}\ndiff --git a/${LONG} b/${LONG}\n--- a/${LONG}\n+++ b/${LONG}\n@@ -1 +1,2 @@\n x\n+y`;
  deck.on(
    `${session}/diff`,
    ok({ ...diff, summary: { files_changed: 2, insertions: 10, deletions: 3 }, patch: longPatch }),
  );
  deck.review(LIGHTING, OTTER, REAPER_PATCH);
  const noAnswers = {
    fleet_control: false,
    answer_cap: null,
    answer_used: 0,
    last_hour: 0,
    answers: [],
  };
  deck.on(`/api/sessions/${LIGHTING}/${OTTER}/fleet-answers`, ok(noAnswers));
  const targets = [DEV1, MAC].map((st) => ({
    station_id: st,
    name: st.slice(3),
    kind: 'native',
    ready: true,
  }));
  deck.on(`/api/sessions/${LIGHTING}/${OTTER}/move`, ok({ move: null, targets }));
  deck.on('/api/displays', ok({ displays: [], pending: [] }));
  deck.on('/api/fleet-git/prefs', ok({ drift_reference: null }));
  return deck;
}
