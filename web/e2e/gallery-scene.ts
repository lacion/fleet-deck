// The UI gallery scene (opt-in: FD_GALLERY=1): three stations, seven sessions and the
// `otter-n1rr` timeline of docs/v2/design/ui-refresh/concepts/BRIEF.md §4, with a small answer
// for every API call of every board page. `galleryDeck` is the busy deck, `{ needsYou: false }`
// the all-clear deck. The data tables keep one row to a line (`biome-ignore format`).
import type {
  ActivityItem,
  DeckArtifact,
  DeckSession,
  Event,
  EventEnvelope,
  FieldMeta,
  OpenRequest,
  PrSummary,
  StationInfo,
  TaskView,
} from '../src/gen';
import { project } from '../src/integrations/fixtures';
import { meta } from '../src/settings/fixtures';
import { claudeCaps, codexCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { FakeDeck, stationView } from './fake-deck';

export const LIGHTING = 'st-lighting';
export const DEV1 = 'st-dev1';
export const MAC = 'st-mac';
/** The open session of the session shots: `otter-n1rr` "Fix flaky shim test". */
export const OTTER = 'ses-otter-n1rr';
const id = (callsign: string) => `ses-${callsign}`;

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
const GIB = 1024 ** 3;
const FD = '/home/me/projects/fleet-deck';
const RA = '/home/me/projects/rannoch';
const REAPER = 'crates/fleetdeck-station/src/shim/reaper.rs';
const NEXTEST = "cargo nextest run -p fleetdeck-station -E 'test(shim)'";
const WHY = 'Run the shim tests 20 times to confirm the fix';
const QUESTION = 'Use the test key or the live key for the webhook secret?';
const TURN = 'turn-1';
const FD_REPO = 'github.com/lacion/fleet-deck';
const RA_REPO = 'github.com/lacion/rannoch';
const TOKENS = 'web/src/styles/tokens.css';

/** The reaper.rs change of the turn: +9 -3 in one file. */
const REAPER_PATCH = [
  `diff --git a/${REAPER} b/${REAPER}`,
  `--- a/${REAPER}`,
  `+++ b/${REAPER}`,
  '@@ -38,7 +38,13 @@ use std::io;',
  ' use std::io;',
  '+use nix::sys::signal::{SigSet, Signal};',
  ' ',
  '+/// Installs the reaper without losing a SIGCHLD.',
  ' pub fn install_reaper() -> io::Result<Reaper> {',
  '-    let reaper = Reaper::new()?;',
  '-    install_handler(&reaper)?;',
  '-    Ok(reaper)',
  '+    let mask = block_sigchld()?;       // block first',
  '+    let reaper = Reaper::new(mask)?;   // then install',
  '+    install_handler(&reaper)?;',
  '+    mask.thread_unblock()?;',
  '+    // A child that exited early left no signal behind.',
  '+    reaper.drain_exited();             // reap what exited before',
  '+    Ok(reaper)',
  ' }',
].join('\n');

const commandRequest = (now: number): OpenRequest => ({
  request_id: 'req-otter-1',
  node_id: 'node-9',
  request_kind: 'command',
  title: 'Bash',
  details: NEXTEST,
  input: { command: NEXTEST, description: WHY },
  // biome-ignore format: one option to a line
  options: [
    { option_id: 'allow', label: 'Allow', kind: 'allow_once' },
    { option_id: 'allow_session', label: 'Allow for this session', kind: 'allow_session' },
    { option_id: 'deny', label: 'Deny', kind: 'deny' },
  ],
  response: 'rpc_reply',
  requested_at: now - 2 * MIN,
});

/** A Codex question: the `requestUserInput` shape that the board turns into an answer form. */
const questionRequest = (now: number): OpenRequest => ({
  request_id: 'req-kite-1',
  node_id: null,
  request_kind: 'question',
  title: QUESTION,
  details: '',
  input: {
    questions: [
      {
        id: 'webhook_secret',
        header: 'Webhook secret',
        question: QUESTION,
        isOther: false,
        isSecret: false,
        // biome-ignore format: one option to a line
        options: [
          { label: 'Test key', description: 'Sign with the Stripe test-mode secret.' },
          { label: 'Live key', description: 'Sign with the live secret. Real events only.' },
        ],
      },
    ],
  },
  options: [{ option_id: 'answer', label: 'Answer' }],
  response: 'rpc_reply',
  requested_at: now - 6 * MIN,
});

type Row = {
  callsign: string;
  title: string;
  branch: string;
  station: string;
  model: string;
  cwd: string;
  status: 'needs_you' | 'working' | 'idle' | 'ended';
  /** Minutes since the last change. */
  age: number;
  /** Input and output tokens in thousands, and the cost in dollars. */
  use: [number, number, number];
  extra: Partial<DeckSession['summary']>;
};

const tool = (title: string, input: string, kind: 'shell' | 'edit' = 'edit') => [
  { node_id: `n-${title}`, tool_kind: kind, title, input_summary: input },
];
const limit = (scope: string, used_percent: number, resets_at: number) => ({
  scope,
  used_percent,
  resets_at,
});
const landed = (extra: object) => ({
  land_id: 'land-1',
  attempt: 1,
  state: 'landed' as const,
  phase: 'done' as const,
  reconciling: false,
  files: [],
  ...extra,
});

/** The seven sessions of the BRIEF table (the usage sums to 4.2 M in, 610 k out, $38.40). */
function rows(now: number, needsYou: boolean): Row[] {
  const ask = needsYou ? 'needs_you' : 'working';
  const claude = [limit('five_hour', 62, now + 108 * MIN), limit('weekly', 31, now + 4 * DAY)];
  // `names` is "callsign|title|branch".
  const row = (
    names: string,
    [station, model, cwd]: [string, string, string],
    [status, age]: [Row['status'], number],
    use: Row['use'],
    extra: Row['extra'] = {},
  ): Row => {
    const [callsign = '', title = '', branch = ''] = names.split('|');
    return { callsign, title, branch, station, model, cwd, status, age, use, extra };
  };
  const [sonnet, opus, haiku, codex] = [
    'claude-sonnet-5-5',
    'claude-opus-5-5',
    'claude-haiku-5-5',
    'gpt-6.1-codex',
  ] as const;
  const bash = needsYou
    ? { open_requests: [commandRequest(now)] }
    : { running_tools: tool('Bash', NEXTEST, 'shell') };
  // biome-ignore format: one session to a line
  return [
    row('otter-n1rr|Fix flaky shim test|fleet-sync', [LIGHTING, sonnet, FD], [ask, needsYou ? 2 : 0.3], [380, 52, 3.1], { artifacts_count: 1, rate_limits: claude, ...bash }),
    row('kite-x81d|Stripe webhooks|rannoch/webhooks', [DEV1, codex, RA], [ask, needsYou ? 6 : 0.5], [410, 60, 2.4], needsYou ? { open_requests: [questionRequest(now)] } : {}),
    row('falcon-vf9n|Fleet Deck UI|ui-refresh', [LIGHTING, opus, FD], ['working', 14], [1100, 150, 12.8], { focus: 'Designing the new board: concept briefs', rate_limits: claude, running_tools: tool('Write', 'concepts/BRIEF.md') }),
    // Two subagents run. The pull request #128 is the `pr_url` of the land.
    row('lynx-4kq2|Rannoch checkout flow|rannoch/checkout', [DEV1, opus, RA], ['working', 38], [1250, 170, 11.2], { background_agents: 2, background_tasks: ['agent-1', 'agent-2'], land: landed({ pr_url: `https://${RA_REPO}/pull/128` }), running_tools: tool('Edit', 'src/checkout/cart.ts') }),
    row('wren-77ta|Journzy iOS build|journzy/ios-ci', [MAC, sonnet, '/Users/me/journzy'], ['working', 9], [520, 70, 3.6], { running_tools: tool('Bash', 'xcodebuild -scheme Journzy test', 'shell') }),
    row('heron-mc2q|Omarchy integration docs|docs/omarchy-integration', [LIGHTING, codex, FD], ['idle', 120], [330, 68, 2.9], { rate_limits: [limit('five_hour', 18, now + 3 * HOUR)] }),
    row('swift-0p3e|Release notes 2.0|release-notes', [MAC, haiku, FD], ['ended', 24 * 60], [210, 40, 2.4], { state: 'exited', land: landed({ default_published_oid: '9f3c2ab7d41e0c58' }) }),
  ];
}

function deckSession(now: number, r: Row): DeckSession {
  const codex = r.model.startsWith('gpt');
  const live = r.status === 'working' || r.status === 'needs_you';
  const origin = { [RA]: RA_REPO, [FD]: FD_REPO }[r.cwd] ?? 'github.com/lacion/journzy';
  return {
    station_id: r.station,
    status: r.status,
    summary: summary(id(r.callsign), r.status, now - r.age * MIN, {
      provider: codex ? 'codex' : 'claude',
      provider_instance: codex ? 'codex' : 'claude',
      provider_version: codex ? '0.163.0' : '2.1.306',
      callsign: r.callsign,
      title: r.title,
      branch: r.branch,
      cwd: r.cwd,
      repo: { root: r.cwd, origin },
      model: r.model,
      mode: codex ? 'on-request' : 'acceptEdits',
      capabilities: codex ? codexCaps() : claudeCaps(),
      current_run: live ? `run-${r.callsign}` : null,
      usage: {
        input_tokens: r.use[0] * 1000,
        output_tokens: r.use[1] * 1000,
        cost_usd: r.use[2],
        cost_inferred: codex,
      },
      session_seq: 40,
      ...r.extra,
    }),
  };
}

function info(
  now: number,
  name: string,
  live: number,
  os: string,
  arch: string,
  [claude, codex]: [string, string],
  extra: Partial<StationInfo> = {},
): StationInfo {
  // biome-ignore format: one provider to a line
  const provider = (instance: 'claude' | 'codex', version: string, caps: typeof claudeCaps) =>
    ({ instance, provider: instance, version, auth: 'signed_in', version_status: 'ok', capabilities: caps() }) as const;
  return {
    name,
    incarnation: `inc-${name}`,
    head: 480,
    live_sessions: live,
    version: '2.0.0',
    max_sessions: 12,
    allow_shell: true,
    allow_setup_cmd: true,
    providers: [provider('claude', claude, claudeCaps), provider('codex', codex, codexCaps)],
    resources: {
      at: now - 20_000,
      disks: [{ label: 'Data', path: '/data', total_bytes: 476 * GIB, free_bytes: 291 * GIB }],
      memory: { total_bytes: 32 * GIB, available_bytes: 19 * GIB },
      cpu: { cores: 12, load1_x100: 185, load5_x100: 142 },
      sessions: [],
    },
    traits: { os, arch, labels: [] },
    ...extra,
  };
}

/** The Omarchy desktop of dev1: one monitor, every tool present. */
// biome-ignore format: one line for the monitor and one for the tools
const desktop: NonNullable<StationInfo['desktop']> = {
  kind: 'omarchy',
  omarchy_version: '3.1.0',
  monitors: [{ name: 'DP-1', description: 'LG UltraFine 5K', width: 2560, height: 1440, scale_pct: 100, refresh_hz: 60, focused: true, dpms_on: true }],
  locked: false,
  dnd: false,
  idle_inhibited: false,
  walls: [],
  allowed: { desktop: true, capture: true, capture_monitors: [], input: false },
  tools: { wtype: true, pointer: true, recorder: true, grim: true, chromium: true, notify_send: true },
  features: [],
};

/** Lighting (Linux), dev1 (Omarchy with a desktop) and mac-m4 (macOS). */
function stations(now: number) {
  const view = (st: string, name: string, seen: number, i: StationInfo) =>
    stationView(st, name, { info: i, info_at: now - 20_000, last_seen: now - seen });
  // biome-ignore format: one station to a line
  return [
    view(LIGHTING, 'Lighting', 2_000, info(now, 'lighting', 3, 'linux', 'x86_64', ['2.1.306', '0.163.0'])),
    view(DEV1, 'dev1', 3_000, info(now, 'dev1', 2, 'omarchy', 'x86_64', ['2.1.306', '0.163.0'], { desktop })),
    view(MAC, 'mac-m4', 4_000, info(now, 'mac-m4', 2, 'macos', 'arm64', ['2.1.305', '0.162.1'])),
  ];
}

const refs = {};
// biome-ignore format: one line
const started = (node: string, kind: 'read' | 'search' | 'edit' | 'shell', title: string, input: string): Event => ({
  kind: 'tool.started',
  node_id: node,
  parent_node_id: null,
  tool_kind: kind,
  title,
  input_summary: input,
  files: kind === 'read' || kind === 'edit' ? [REAPER] : [],
  provider_refs: refs,
});
const done = (node: string): Event => ({
  kind: 'tool.completed',
  node_id: node,
  status: 'ok',
  files: [],
  exit_code: null,
});
const said = (text: string): Event => ({
  kind: 'message.completed',
  turn_id: TURN,
  parent_node_id: null,
  text,
});

const FIX = [
  'The test races the reaper: the child can exit before `install_reaper()` returns, so the `SIGCHLD` is lost. Here is the fix:',
  '',
  '```rust',
  'pub fn install_reaper() -> io::Result<Reaper> {',
  '    let mask = block_sigchld()?;       // block first',
  '    let reaper = Reaper::new(mask)?;   // then install',
  '    reaper.drain_exited();             // reap what exited before',
  '    Ok(reaper)',
  '}',
  '```',
].join('\n');
const TABLE = [
  'Two files change:',
  '',
  '| File | Problem | Fix |',
  '| --- | --- | --- |',
  '| `shim/reaper.rs` | SIGCHLD lost before install | block, install, drain |',
  '| `shim/tests.rs` | test waits 50 ms | wait for the exit event |',
].join('\n');

/** BRIEF §4 "chat transcript" items 1 to 10; the diff of the turn belongs to the Edit. */
function otterTimeline(now: number): EventEnvelope[] {
  const input = 'The shim test fails about 1 in 20 runs on Lighting. Find out why and fix it.';
  const start = { state: 'available', commit: 'otter-start' } as const;
  const end = { state: 'available', commit: 'otter-end' } as const;
  // biome-ignore format: one event to a line
  const events: Event[] = [
    { kind: 'run.started', run_id: 'run-1', input_summary: input, input_text: input, origin: 'board' },
    { kind: 'turn.started', run_id: 'run-1', turn_id: TURN, provider_refs: refs },
    { kind: 'reasoning.delta', turn_id: TURN, text: 'A flaky shim test points at a race. Read the reaper, then look for every waitpid.' },
    started('node-1', 'read', 'Read', REAPER),
    done('node-1'),
    started('node-2', 'search', 'Search', 'waitpid in crates/fleetdeck-station'),
    { kind: 'tool.output', node_id: 'node-2', chunk: '7 matches\n', truncated: false },
    done('node-2'),
    said(FIX),
    said(TABLE),
    started('node-8', 'edit', 'Edit', REAPER),
    { kind: 'file.changed', path: REAPER, change: 'modified', node_id: 'node-8' },
    done('node-8'),
    { kind: 'turn.finalized', turn_id: TURN, checkpoint: { worktree_id: 'wt-otter', scope: 'tracked', start, end }, diff_summary: { files_changed: 1, insertions: 9, deletions: 3 } },
    started('node-9', 'shell', 'Bash', NEXTEST),
    { kind: 'approval.requested', request_id: 'req-otter-1', node_id: 'node-9', request_kind: 'command', title: 'Bash', details: NEXTEST, input: { command: NEXTEST, description: WHY }, options: commandRequest(now).options, response: 'rpc_reply', provider_refs: refs },
  ];
  return events.map((event, i) => ({
    station_seq: 500 + i + 1,
    session_id: OTTER,
    session_seq: i + 1,
    at: now - (events.length - i) * 20_000,
    proto_version: 1,
    raw_ref: null,
    event,
  }));
}

/** The newest rows first: [minutes ago, kind, session callsign, text]. */
// biome-ignore format: one row to a line
const FEED: [number, ActivityItem['kind'], string, string][] = [
  [2, 'approval', 'otter-n1rr', `Permission: Bash ${NEXTEST}`],
  [3, 'tool', 'lynx-4kq2', 'Edit src/checkout/cart.ts'],
  [6, 'approval', 'kite-x81d', `Question: ${QUESTION}`],
  [9, 'message', 'wren-77ta', 'The archive step passes. Running the test scheme next.'],
  [14, 'tool', 'falcon-vf9n', 'Write concepts/BRIEF.md'],
  [25, 'mail', 'heron-mc2q', 'Mail from the owner: rebase on v2 when you can.'],
  [38, 'lifecycle', 'lynx-4kq2', 'Session started'],
  [120, 'message', 'heron-mc2q', 'The docs build passes. Next?'],
  [1440, 'lifecycle', 'swift-0p3e', 'Session ended. Landed into v2.'],
];

function activity(now: number, sessions: DeckSession[]): ActivityItem[] {
  return FEED.map(([ago, kind, callsign, text]) => {
    const card = sessions.find((d) => d.summary.callsign === callsign);
    return {
      station_id: card?.station_id ?? LIGHTING,
      session_id: id(callsign),
      session_title: card?.summary.title ?? null,
      repo: card?.summary.repo?.origin ?? null,
      kind,
      text,
      at: now - ago * MIN,
      station_seq: 400,
    };
  });
}

/** One proposed plan of a session: a title and numbered steps. */
function plan(
  station: string,
  callsign: string,
  n: number,
  title: string,
  steps: string[],
  ago: number,
) {
  const at = Date.now() - ago * MIN;
  const markdown = [`# ${title}`, '', ...steps.map((s, i) => `${i + 1}. ${s}`)].join('\n');
  // biome-ignore format: one line
  return { id: `${station}/${id(callsign)}/plan-${n}`, station_id: station, session_id: id(callsign), plan_id: `plan-${n}`, turn_id: `turn-${n}`, session_title: title, markdown, truncated: false, status: 'proposed' as const, proposed_at: at, updated_at: at };
}

/** One automation; the first one ran last night. */
function schedule(
  n: number,
  name: string,
  cron: string,
  station: string,
  prompt: string,
  now: number,
) {
  const schedule_id = `sched-${n}`;
  const at = now - 21 * HOUR;
  // biome-ignore format: one line for the schedule and one for the run
  const base = { schedule_id, name, enabled: true, cron, timezone: 'UTC', station_id: station, project_id: null, path: FD, provider_instance: 'claude', mode: 'acceptEdits', model: null, prompt, precheck_cmd: null, reuse_session: false, last_session: null, grace_ms: 600_000, created_at: now - 20 * DAY };
  // biome-ignore format: one line
  const last = { run_id: 'run-s1', schedule_id, due_at: at, started_at: at, state: 'started' as const, reason: '', station_id: station, session_id: 'ses-old-1', manual: false };
  return { schedule: base, next_run_at: now + n * 3 * HOUR, last_run: n === 1 ? last : null };
}

/** The artifacts: `otter-n1rr` has one. */
function artifacts(now: number): DeckArtifact[] {
  // biome-ignore format: one line
  const row = (st: string, name: string, callsign: string, art: string, title: string, kind: DeckArtifact['kind'], v: number, ago: number): DeckArtifact =>
    ({ station_id: st, station_name: name, artifact_id: art, session_id: id(callsign), title, kind, latest_version: v, updated_at: now - ago * MIN });
  // biome-ignore format: one artifact to a line
  return [
    row(LIGHTING, 'Lighting', 'otter-n1rr', 'art-shim', 'Shim test run report', 'markdown', 1, 1),
    row(LIGHTING, 'Lighting', 'falcon-vf9n', 'art-concepts', 'Concept briefs', 'markdown', 3, 14),
    row(DEV1, 'dev1', 'lynx-4kq2', 'art-checkout', 'Checkout flow diagram', 'svg', 2, 50),
    row(LIGHTING, 'Lighting', 'heron-mc2q', 'art-docs', 'Omarchy docs preview', 'html', 1, 120),
  ];
}

const pr128 = (now: number): PrSummary => ({
  number: 128,
  title: 'Checkout flow: cart totals',
  url: `https://${RA_REPO}/pull/128`,
  author: 'me',
  head_branch: 'rannoch/checkout',
  base_branch: 'main',
  draft: false,
  state: 'open',
  review_state: 'review_required',
  checks: 'passing',
  mergeable: true,
  ticket_key: null,
  updated_at: now - 10 * MIN,
  review_requested_from_me: false,
  mine: true,
});

/** The folders of the workspace by path; a name that ends with `/` is a folder. */
// biome-ignore format: one line
const ls = (...names: string[]) => names.map((n) => (n.endsWith('/') ? { name: n.slice(0, -1), kind: 'dir', size: null } : { name: n, kind: 'file', size: 3200 }));
const TREE: Record<string, ReturnType<typeof ls>> = {
  '': ls('crates/', 'docs/', 'web/', 'Cargo.toml', 'README.md'),
  crates: ls('fleetdeck-station/', 'fleetdeck-deck/'),
  'crates/fleetdeck-station': ls('src/', 'Cargo.toml'),
  'crates/fleetdeck-station/src': ls('shim/', 'lib.rs'),
  'crates/fleetdeck-station/src/shim': ls('reaper.rs', 'tests.rs'),
};

const stationFields: FieldMeta[] = [
  meta('name', 'Station name', 'text', { help: 'The name that the board shows for this station.' }),
  meta('max_sessions', 'Most sessions', 'int', {
    min: 1,
    max: 256,
    help: 'The station refuses a new session above this number.',
  }),
  meta('allow_shell', 'Allow terminals', 'bool', {
    danger: true,
    help: 'The board can open a shell on this machine.',
  }),
  meta('host', 'Where sessions run', 'enum', {
    options: ['direct', 'shim'],
    apply: 'restart',
    help: 'A shim keeps a session alive when the station restarts.',
  }),
  meta('allowed_roots', 'Allowed folders', 'paths', {
    danger: true,
    help: 'Sessions and shells can work only inside these folders.',
  }),
  meta('model_help.enabled', 'Model help', 'bool', {
    help: 'A small model writes session titles and digests.',
  }),
];
// biome-ignore format: one line
const stationValues = { name: 'Lighting', max_sessions: 12, allow_shell: true, host: 'shim', allowed_roots: ['/home/me/projects'], 'model_help.enabled': true };

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
  // biome-ignore format: one line each
  const task: TaskView = { task_id: 'task-1', title: 'Add webhook retry tests', creator: 'owner', station_id: DEV1, session_id: id('kite-x81d'), state: 'open', result: null, completion_reason: null, question: null, delivery: 'delivered', reason: null, created_at: now - 40 * MIN, updated_at: now - 40 * MIN, completed_at: null };
  // biome-ignore format: one line each
  const touch = (callsign: string, title: string, ago: number) => ({ station_id: LIGHTING, session_id: id(callsign), session_title: title, path: TOKENS, touched_at: now - ago * MIN });
  // biome-ignore format: one line each
  const extras = (callsign: string, mail: number, file: string) => ({ station_id: LIGHTING, session_id: id(callsign), mail_pending: mail, files: [file] });
  // biome-ignore format: one line each
  const share = { share_id: 'share-1', kind: 'session', target: `${LIGHTING}/${id('falcon-vf9n')}`, role: 'viewer', steering: 'open', history: false, expires_at: now + 2 * DAY, revoked_at: null, created_at: now - 3 * HOUR, uses_left: 1, members: [] };
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
  // biome-ignore format: one line each
  deck.on('/api/plans', ok([plan(LIGHTING, 'falcon-vf9n', 1, 'New board: concept briefs', steps1, 45), plan(DEV1, 'lynx-4kq2', 2, 'Checkout: cart totals', steps2, 120)]));
  // biome-ignore format: one line each
  deck.on('/api/schedules', ok([schedule(1, 'Nightly dependency check', '0 3 * * *', LIGHTING, 'Check the lockfiles for outdated dependencies.', now), schedule(2, 'Weekly release notes draft', '0 9 * * 1', MAC, 'Draft the release notes from the merged pull requests.', now)]));
  deck.on('/api/artifacts', (_body, query) => ({
    status: 200,
    body: artifacts(now).filter((a) => (query.get('session') ?? a.session_id) === a.session_id),
  }));
  deck.on('/api/shares', ok([share]));
  // biome-ignore format: one line each
  const repo = (pid: string, name: string, st: string, path: string, origin: string) => project({ project_id: pid, name, station_id: st, path, forge: { kind: 'github', host: 'github.com', repo: origin.replace('github.com/', '') }, trackers: [], repo: origin, remote_url: `https://${origin}.git` });
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
  // biome-ignore format: one line each
  deck.on(`${station}/files/list`, (_body, q) => ({ status: 200, body: { root: FD, path: at(q), entries: TREE[at(q)] ?? TREE['crates'], truncated: false } }));
  // biome-ignore format: one line each
  deck.on(`${station}/files/read`, (_body, q) => ({ status: 200, body: { path: at(q), size: 5230, binary: false, text: `// ${at(q)}\n`, truncated: false } }));
  // The turn diff, the diff between two checkpoints and the diff since the start: the same patch.
  const session = `${station}/sessions/${OTTER}`;
  // biome-ignore format: one line
  const diff = { session_id: OTTER, turn_id: TURN, from_commit: 'otter-start', to_commit: 'otter-end', base_oid: 'a1b2c3d4e5f6', scope: 'worktree', summary: { files_changed: 1, insertions: 9, deletions: 3 }, patch: REAPER_PATCH, truncated: false };
  for (const suffix of [`turns/${TURN}/diff`, 'turn-diff', 'diff'])
    deck.on(`${session}/${suffix}`, ok(diff));
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
