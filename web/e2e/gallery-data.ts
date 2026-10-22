// The data of the UI gallery scene: the three stations, the seven sessions of
// docs/v2/design/ui-refresh/concepts/BRIEF.md §4, the `otter-n1rr` timeline and the rows of the
// pages. gallery-scene.ts builds the fake deck from it.
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
} from '../src/gen';
import { meta } from '../src/settings/fixtures';
import { claudeCaps, codexCaps } from '../src/testing/capabilities';
import { summary } from '../src/testing/summary';
import { stationView } from './fake-deck';

export const LIGHTING = 'st-lighting';
export const DEV1 = 'st-dev1';
export const MAC = 'st-mac';
/** The open session of the session shots: `otter-n1rr` "Fix flaky shim test". */
export const OTTER = 'ses-otter-n1rr';
export const id = (callsign: string) => `ses-${callsign}`;

export const MIN = 60_000;
export const HOUR = 60 * MIN;
export const DAY = 24 * HOUR;
const GIB = 1024 ** 3;
export const FD = '/home/me/projects/fleet-deck';
export const RA = '/home/me/projects/rannoch';
const REAPER = 'crates/fleetdeck-station/src/shim/reaper.rs';
const NEXTEST = "cargo nextest run -p fleetdeck-station -E 'test(shim)'";
const WHY = 'Run the shim tests 20 times to confirm the fix';
const QUESTION = 'Use the test key or the live key for the webhook secret?';
export const TURN = 'turn-1';
export const FD_REPO = 'github.com/lacion/fleet-deck';
export const RA_REPO = 'github.com/lacion/rannoch';
export const TOKENS = 'web/src/styles/tokens.css';

/** The reaper.rs change of the turn: +9 -3 in one file. */
export const REAPER_PATCH = [
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
export function rows(now: number, needsYou: boolean): Row[] {
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
  return [
    row(
      'otter-n1rr|Fix flaky shim test|fleet-sync',
      [LIGHTING, sonnet, FD],
      [ask, needsYou ? 2 : 0.3],
      [380, 52, 3.1],
      { artifacts_count: 1, rate_limits: claude, ...bash },
    ),
    row(
      'kite-x81d|Stripe webhooks|rannoch/webhooks',
      [DEV1, codex, RA],
      [ask, needsYou ? 6 : 0.5],
      [410, 60, 2.4],
      needsYou ? { open_requests: [questionRequest(now)] } : {},
    ),
    row(
      'falcon-vf9n|Fleet Deck UI|ui-refresh',
      [LIGHTING, opus, FD],
      ['working', 14],
      [1100, 150, 12.8],
      {
        focus: 'Designing the new board: concept briefs',
        rate_limits: claude,
        running_tools: tool('Write', 'concepts/BRIEF.md'),
      },
    ),
    // Two subagents run. The pull request #128 is the `pr_url` of the land.
    row(
      'lynx-4kq2|Rannoch checkout flow|rannoch/checkout',
      [DEV1, opus, RA],
      ['working', 38],
      [1250, 170, 11.2],
      {
        background_agents: 2,
        background_tasks: ['agent-1', 'agent-2'],
        land: landed({ pr_url: `https://${RA_REPO}/pull/128` }),
        running_tools: tool('Edit', 'src/checkout/cart.ts'),
      },
    ),
    row(
      'wren-77ta|Journzy iOS build|journzy/ios-ci',
      [MAC, sonnet, '/Users/me/journzy'],
      ['working', 9],
      [520, 70, 3.6],
      { running_tools: tool('Bash', 'xcodebuild -scheme Journzy test', 'shell') },
    ),
    row(
      'heron-mc2q|Omarchy integration docs|docs/omarchy-integration',
      [LIGHTING, codex, FD],
      ['idle', 120],
      [330, 68, 2.9],
      { rate_limits: [limit('five_hour', 18, now + 3 * HOUR)] },
    ),
    row(
      'swift-0p3e|Release notes 2.0|release-notes',
      [MAC, haiku, FD],
      ['ended', 24 * 60],
      [210, 40, 2.4],
      { state: 'exited', land: landed({ default_published_oid: '9f3c2ab7d41e0c58' }) },
    ),
  ];
}

export function deckSession(now: number, r: Row): DeckSession {
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
  const provider = (instance: 'claude' | 'codex', version: string, caps: typeof claudeCaps) =>
    ({
      instance,
      provider: instance,
      version,
      auth: 'signed_in',
      version_status: 'ok',
      capabilities: caps(),
    }) as const;
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
const desktop: NonNullable<StationInfo['desktop']> = {
  kind: 'omarchy',
  omarchy_version: '3.1.0',
  monitors: [
    {
      name: 'DP-1',
      description: 'LG UltraFine 5K',
      width: 2560,
      height: 1440,
      scale_pct: 100,
      refresh_hz: 60,
      focused: true,
      dpms_on: true,
    },
  ],
  locked: false,
  dnd: false,
  idle_inhibited: false,
  walls: [],
  allowed: { desktop: true, capture: true, capture_monitors: [], input: false },
  tools: {
    wtype: true,
    pointer: true,
    recorder: true,
    grim: true,
    chromium: true,
    notify_send: true,
  },
  features: [],
};

/** Lighting (Linux), dev1 (Omarchy with a desktop) and mac-m4 (macOS). */
export function stations(now: number) {
  const view = (st: string, name: string, seen: number, i: StationInfo) =>
    stationView(st, name, { info: i, info_at: now - 20_000, last_seen: now - seen });
  return [
    view(
      LIGHTING,
      'Lighting',
      2_000,
      info(now, 'lighting', 3, 'linux', 'x86_64', ['2.1.306', '0.163.0']),
    ),
    view(
      DEV1,
      'dev1',
      3_000,
      info(now, 'dev1', 2, 'omarchy', 'x86_64', ['2.1.306', '0.163.0'], { desktop }),
    ),
    view(MAC, 'mac-m4', 4_000, info(now, 'mac-m4', 2, 'macos', 'arm64', ['2.1.305', '0.162.1'])),
  ];
}

const refs = {};
const started = (
  node: string,
  kind: 'read' | 'search' | 'edit' | 'shell',
  title: string,
  input: string,
): Event => ({
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
export function otterTimeline(now: number): EventEnvelope[] {
  const input = 'The shim test fails about 1 in 20 runs on Lighting. Find out why and fix it.';
  const start = { state: 'available', commit: 'otter-start' } as const;
  const end = { state: 'available', commit: 'otter-end' } as const;
  const events: Event[] = [
    {
      kind: 'run.started',
      run_id: 'run-1',
      input_summary: input,
      input_text: input,
      origin: 'board',
    },
    { kind: 'turn.started', run_id: 'run-1', turn_id: TURN, provider_refs: refs },
    {
      kind: 'reasoning.delta',
      turn_id: TURN,
      text: 'A flaky shim test points at a race. Read the reaper, then look for every waitpid.',
    },
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
    {
      kind: 'turn.finalized',
      turn_id: TURN,
      checkpoint: { worktree_id: 'wt-otter', scope: 'tracked', start, end },
      diff_summary: { files_changed: 1, insertions: 9, deletions: 3 },
    },
    started('node-9', 'shell', 'Bash', NEXTEST),
    {
      kind: 'approval.requested',
      request_id: 'req-otter-1',
      node_id: 'node-9',
      request_kind: 'command',
      title: 'Bash',
      details: NEXTEST,
      input: { command: NEXTEST, description: WHY },
      options: commandRequest(now).options,
      response: 'rpc_reply',
      provider_refs: refs,
    },
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

export function activity(now: number, sessions: DeckSession[]): ActivityItem[] {
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
export function plan(
  station: string,
  callsign: string,
  n: number,
  title: string,
  steps: string[],
  ago: number,
) {
  const at = Date.now() - ago * MIN;
  const markdown = [`# ${title}`, '', ...steps.map((s, i) => `${i + 1}. ${s}`)].join('\n');
  return {
    id: `${station}/${id(callsign)}/plan-${n}`,
    station_id: station,
    session_id: id(callsign),
    plan_id: `plan-${n}`,
    turn_id: `turn-${n}`,
    session_title: title,
    markdown,
    truncated: false,
    status: 'proposed' as const,
    proposed_at: at,
    updated_at: at,
  };
}

/** One automation; the first one ran last night. */
export function schedule(
  n: number,
  name: string,
  cron: string,
  station: string,
  prompt: string,
  now: number,
) {
  const schedule_id = `sched-${n}`;
  const at = now - 21 * HOUR;
  const base = {
    schedule_id,
    name,
    enabled: true,
    cron,
    timezone: 'UTC',
    station_id: station,
    project_id: null,
    path: FD,
    provider_instance: 'claude',
    mode: 'acceptEdits',
    model: null,
    prompt,
    precheck_cmd: null,
    reuse_session: false,
    last_session: null,
    grace_ms: 600_000,
    created_at: now - 20 * DAY,
  };
  const last = {
    run_id: 'run-s1',
    schedule_id,
    due_at: at,
    started_at: at,
    state: 'started' as const,
    reason: '',
    station_id: station,
    session_id: 'ses-old-1',
    manual: false,
  };
  return { schedule: base, next_run_at: now + n * 3 * HOUR, last_run: n === 1 ? last : null };
}

/** The artifacts: `otter-n1rr` has one. */
export function artifacts(now: number): DeckArtifact[] {
  const row = (
    st: string,
    name: string,
    callsign: string,
    art: string,
    title: string,
    kind: DeckArtifact['kind'],
    v: number,
    ago: number,
  ): DeckArtifact => ({
    station_id: st,
    station_name: name,
    artifact_id: art,
    session_id: id(callsign),
    title,
    kind,
    latest_version: v,
    updated_at: now - ago * MIN,
  });
  return [
    row(LIGHTING, 'Lighting', 'otter-n1rr', 'art-shim', 'Shim test run report', 'markdown', 1, 1),
    row(LIGHTING, 'Lighting', 'falcon-vf9n', 'art-concepts', 'Concept briefs', 'markdown', 3, 14),
    row(DEV1, 'dev1', 'lynx-4kq2', 'art-checkout', 'Checkout flow diagram', 'svg', 2, 50),
    row(LIGHTING, 'Lighting', 'heron-mc2q', 'art-docs', 'Omarchy docs preview', 'html', 1, 120),
  ];
}

export const pr128 = (now: number): PrSummary => ({
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
export const ls = (...names: string[]) =>
  names.map((n) =>
    n.endsWith('/')
      ? { name: n.slice(0, -1), kind: 'dir', size: null }
      : { name: n, kind: 'file', size: 3200 },
  );
export const TREE: Record<string, ReturnType<typeof ls>> = {
  '': ls('crates/', 'docs/', 'web/', 'Cargo.toml', 'README.md'),
  crates: ls('fleetdeck-station/', 'fleetdeck-deck/'),
  'crates/fleetdeck-station': ls('src/', 'Cargo.toml'),
  'crates/fleetdeck-station/src': ls('shim/', 'lib.rs'),
  'crates/fleetdeck-station/src/shim': ls('reaper.rs', 'tests.rs'),
};

export const stationFields: FieldMeta[] = [
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
  meta('model_help.enabled', 'Write titles and digests', 'bool', {
    help: 'A small model writes session titles and digests.',
  }),
];
export const stationValues = {
  name: 'Lighting',
  max_sessions: 12,
  allow_shell: true,
  host: 'shim',
  allowed_roots: ['/home/me/projects'],
  'model_help.enabled': true,
};
