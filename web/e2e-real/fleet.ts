// Real processes for the P4-5 browser flows: one `fleetdeck deck serve` and
// two `fleetdeck station serve` processes on temporary nodes, each with its
// own home and data directory. Stations run the offline `fake-claude sim` and
// `fake-codex app-server` (`sim`) agents, so no provider account or network is
// used.
import { type ChildProcess, execFileSync, spawn, spawnSync } from 'node:child_process';
import { type APIRequestContext, request } from '@playwright/test';
import {
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Built by `setup.ts` (`cargo build`), or given in `FD_BIN_DIR` (made
 * absolute: station.toml needs an absolute provider binary). */
export const BIN_DIR = resolve(
  process.env['FD_BIN_DIR'] ?? fileURLToPath(new URL('../../target/debug', import.meta.url)),
);
const FLEETDECK = join(BIN_DIR, 'fleetdeck');
const FAKE_CLAUDE = join(BIN_DIR, 'fake-claude');
const FAKE_CODEX = join(BIN_DIR, 'fake-codex');
/** The fake Claude agent's provider instance id in station.toml. */
export const PROVIDER = 'fake-claude';
/** The fake Codex agent's provider instance id in station.toml (P5-4). */
export const CODEX = 'fake-codex';
const WAIT_MS = 20_000;
/** The branch that each station workspace reports (`.git/HEAD`). */
export const BRANCH = 'fd/e2e';

/** A loopback port that is free now. */
export function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port = typeof address === 'object' && address !== null ? address.port : 0;
      server.close(() => (port === 0 ? reject(new Error('no free port')) : resolve(port)));
    });
  });
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** The numeric names in /proc (Linux only; none on another platform). */
function procPids(): number[] {
  if (process.platform !== 'linux') return [];
  return readdirSync('/proc')
    .filter((name) => /^\d+$/.test(name))
    .map(Number);
}

/** The pid of `fleetdeck shim --id <id>`, from /proc (Linux only). */
function shimPid(id: string): number | null {
  const needle = `shim\0--id\0${id}\0`;
  for (const pid of procPids()) {
    try {
      if (readFileSync(`/proc/${pid}/cmdline`, 'latin1').includes(needle)) return pid;
    } catch {
      // The process ended.
    }
  }
  return null;
}

/** The pids of the processes that run with `HOME=<home>` (Linux only). */
function processesWithHome(home: string): number[] {
  const entry = `HOME=${home}\0`;
  const pids: number[] = [];
  for (const pid of procPids()) {
    try {
      const environ = readFileSync(`/proc/${pid}/environ`, 'latin1');
      if (environ.startsWith(entry) || environ.includes(`\0${entry}`)) pids.push(pid);
    } catch {
      // The process ended, or it is not ours.
    }
  }
  return pids;
}

/** SIGKILL, and no error when the process is gone already. */
function kill9(pid: number): void {
  try {
    process.kill(pid, 'SIGKILL');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error;
  }
}

/** One temporary node: `<root>/home` is `~`, `<root>/node` the data dir. */
export class Node {
  readonly root: string;
  readonly home: string;
  readonly data: string;
  private child: ChildProcess | null = null;

  constructor(
    readonly name: string,
    private readonly serve: string[],
    private readonly ready: string[],
  ) {
    this.root = mkdtempSync(join(tmpdir(), `fleetdeck-e2e-${name}-`));
    this.home = join(this.root, 'home');
    this.data = join(this.root, 'node');
    mkdirSync(this.home, { mode: 0o700 });
    mkdirSync(this.data, { mode: 0o700 });
  }

  private env(): NodeJS.ProcessEnv {
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: this.home, RUST_LOG: 'warn' };
    for (const key of ['FLEETDECK_DATA_DIR', 'FLEETDECK_FAULT', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME'])
      delete env[key];
    return env;
  }

  writeConfig(file: string, text: string) {
    writeFileSync(join(this.data, file), text, { mode: 0o600 });
  }

  /** One CLI call; it throws with the stderr text when it fails. */
  cli(args: string[], input?: string): string {
    const out = spawnSync(FLEETDECK, ['--data-dir', this.data, ...args], {
      env: this.env(),
      input,
      encoding: 'utf8',
      timeout: WAIT_MS,
    });
    if (out.status !== 0)
      throw new Error(`fleetdeck ${args.join(' ')} failed (${out.status}): ${out.stderr}`);
    return out.stdout.trim();
  }

  private answers(args: string[]): boolean {
    return (
      spawnSync(FLEETDECK, ['--data-dir', this.data, ...args], {
        env: this.env(),
        stdio: 'ignore',
        timeout: WAIT_MS,
      }).status === 0
    );
  }

  /** Starts the serve process and waits until its local socket answers. */
  async start(): Promise<void> {
    if (this.child !== null) throw new Error(`${this.name} runs already`);
    const log = openSync(join(this.root, 'serve.log'), 'a');
    const child = spawn(FLEETDECK, ['--data-dir', this.data, ...this.serve], {
      env: this.env(),
      stdio: ['ignore', log, log],
    });
    closeSync(log);
    this.child = child;
    const deadline = Date.now() + WAIT_MS;
    while (!this.answers(this.ready)) {
      if (child.exitCode !== null) throw new Error(`${this.name} exited: see ${this.root}`);
      if (Date.now() > deadline) throw new Error(`${this.name} did not start`);
      await sleep(100);
    }
  }

  /** SIGTERM, then SIGKILL after the wait. */
  async stop(): Promise<void> {
    const child = this.child;
    this.child = null;
    if (child === null || child.exitCode !== null) return;
    const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()));
    child.kill('SIGTERM');
    const timer = setTimeout(() => child.kill('SIGKILL'), WAIT_MS);
    await exited;
    clearTimeout(timer);
  }

  async dispose(): Promise<void> {
    await this.stop();
    if (process.env['FD_E2E_KEEP'] === undefined)
      rmSync(this.root, { recursive: true, force: true });
  }
}

/** The deck: the board on a fixed loopback port, so that a restarted deck
 * serves the same origin and the phone keeps its login cookie. */
export class Deck extends Node {
  private constructor(
    readonly boardPort: number,
    readonly linkPort: number,
  ) {
    super('deck', ['deck', 'serve'], ['deck', 'stations']);
    this.writeConfig(
      'deck.toml',
      `name = "home-deck"\nlisten = ["127.0.0.1:${boardPort}"]\nlink_listen = ["127.0.0.1:${linkPort}"]\n`,
    );
  }

  static async create(): Promise<Deck> {
    return new Deck(await freePort(), await freePort());
  }

  /** A single-use login link from `fleetdeck open`. */
  loginLink(): string {
    return this.cli(['open']);
  }

  /** The deck's command router rows (`deck commands --json`). */
  commands(): RoutedCommand[] {
    return JSON.parse(this.cli(['deck', 'commands', '--json']));
  }
}

/** The fields of a deck command row that the flows check. */
export type RoutedCommand = {
  command_id: string;
  station_id: string;
  method: string;
  state: string;
  attempts: number;
};

/** What a station's agents read on stdin (every process of the station). */
export type ProviderInput = {
  /** User messages: prompts, steers and follow-ups, in order. */
  messages: string[];
  /** Host control requests by subtype (`interrupt`, `set_model`, ...). */
  controls: Record<string, number>;
  /** Answers to the agent's permission requests. */
  answers: number;
};

/** What a station's Codex agents read on stdin (JSON-RPC from the driver,
 * every process of the station including the sign-in probe). */
export type CodexInput = {
  /** The text of every `turn/start`, in order. */
  turns: string[];
  /** The text of every `turn/steer`, in order. */
  steers: string[];
  /** Requests and notifications by method (`turn/interrupt`, ...). */
  methods: Record<string, number>;
  /** Answers to the agent's approval requests. */
  answers: number;
  /** The `answers` of every reply to a `requestUserInput` question (DL-068). */
  questions: unknown[];
};

/** A station with the `fake-claude sim` and `fake-codex` agents and
 * `~/projects/demo`. */
export class Station extends Node {
  readonly workspace: string;
  /** `fake-claude sim` appends every stdin line here (one file for all of
   * the station's agent processes). */
  readonly stdinLog: string;
  /** The same for the `fake-codex` processes. */
  readonly codexLog: string;

  /** `extraToml`: top-level `station.toml` lines after `listen` (Fleet Git specs: `allow_local_remotes`). */
  constructor(name: string, extraToml: readonly string[] = []) {
    super(name, ['station', 'serve'], ['station', 'sessions']);
    this.stdinLog = join(this.root, 'agent-stdin.jsonl');
    this.codexLog = join(this.root, 'codex-stdin.jsonl');
    this.workspace = join(this.home, 'projects', 'demo');
    mkdirSync(join(this.workspace, '.git'), { recursive: true });
    writeFileSync(join(this.workspace, '.git', 'HEAD'), `ref: refs/heads/${BRANCH}\n`);
    this.writeConfig(
      'station.toml',
      [
        `name = "${name}"`,
        'allowed_roots = ["~/projects"]',
        'max_sessions = 4',
        'listen = []',
        ...extraToml,
        '',
        '[[provider_instance]]',
        `id = "${PROVIDER}"`,
        'driver = "claude"',
        `binary = "${FAKE_CLAUDE}"`,
        `env = { FLEETDECK_FAKE_SCENARIO = "sim", FLEETDECK_FAKE_STDIN_LOG = "${this.stdinLog}" }`,
        '',
        '[[provider_instance]]',
        `id = "${CODEX}"`,
        'driver = "codex"',
        `binary = "${FAKE_CODEX}"`,
        `env = { FLEETDECK_FAKE_SCENARIO = "sim", FLEETDECK_FAKE_STDIN_LOG = "${this.codexLog}" }`,
        '',
      ].join('\n'),
    );
  }

  /** A shim outlives its station by design (ADR-005): kill the shims of this
   * station, so that no shim or fake agent outlives the suite. */
  reapShims(): void {
    const dir = join(this.data, 'station', 'shims');
    if (!existsSync(dir)) return;
    for (const file of readdirSync(dir)) {
      if (!file.endsWith('.journal')) continue;
      const pid = shimPid(file.slice(0, -'.journal'.length));
      if (pid !== null) kill9(pid);
    }
  }

  /** Stops the station, kills its shims (before the folder goes: the journals
   * name them), and fails when a process of the station outlives that. */
  override async dispose(): Promise<void> {
    await this.stop();
    this.reapShims();
    const deadline = Date.now() + 5_000;
    let left = processesWithHome(this.home);
    while (left.length > 0 && Date.now() < deadline) {
      await sleep(100);
      left = processesWithHome(this.home);
    }
    for (const pid of left) kill9(pid);
    await super.dispose();
    if (left.length > 0)
      throw new Error(`processes of the station outlived the suite: ${left.join(', ')}`);
  }

  /** `station run`: a session started on the station itself, without the
   * deck. Returns the new session id. */
  run(prompt: string, provider: string = PROVIDER): string {
    return this.cli(['station', 'run', '--provider', provider, '--cwd', this.workspace, prompt]);
  }

  /** Every JSON-RPC frame that the station wrote to its Codex agents. */
  codexInput(): CodexInput {
    const input: CodexInput = { turns: [], steers: [], methods: {}, answers: 0, questions: [] };
    if (!existsSync(this.codexLog)) return input;
    const text = (params: { input?: { text?: string }[] } | undefined) =>
      String(params?.input?.find((i) => typeof i.text === 'string')?.text);
    for (const line of readFileSync(this.codexLog, 'utf8').split('\n')) {
      if (!line.startsWith('{')) continue;
      const frame = JSON.parse(line);
      if (typeof frame.method === 'string') {
        input.methods[frame.method] = (input.methods[frame.method] ?? 0) + 1;
        if (frame.method === 'turn/start') input.turns.push(text(frame.params));
        if (frame.method === 'turn/steer') input.steers.push(text(frame.params));
      } else if (frame.result?.decision !== undefined) input.answers += 1;
      else if (frame.result?.answers !== undefined) input.questions.push(frame.result.answers);
    }
    return input;
  }

  /** Every frame that the station wrote to its agents, grouped. A command
   * that ran twice shows here as a second message, control or answer. */
  providerInput(): ProviderInput {
    const input: ProviderInput = { messages: [], controls: {}, answers: 0 };
    if (!existsSync(this.stdinLog)) return input;
    for (const line of readFileSync(this.stdinLog, 'utf8').split('\n')) {
      if (!line.startsWith('{')) continue;
      const frame = JSON.parse(line);
      if (frame.type === 'user') input.messages.push(String(frame.message?.content));
      if (frame.type === 'control_request') {
        const subtype = String(frame.request?.subtype);
        // The station's usage poll and its effort read (B18), not board
        // commands: their counts follow the station's own timing.
        if (subtype === 'get_usage' || subtype === 'get_settings') continue;
        input.controls[subtype] = (input.controls[subtype] ?? 0) + 1;
      }
      if (frame.type === 'control_response') input.answers += 1;
    }
    return input;
  }
}

/** Runs `git args` in `dir` with a fixed identity and returns its trimmed stdout (it throws on an error). */
export function git(dir: string, args: string[]): string {
  return execFileSync(
    'git',
    [
      '-c',
      'user.name=rig',
      '-c',
      'user.email=rig@example.invalid',
      '-c',
      'init.defaultBranch=main',
      '-c',
      'commit.gpgsign=false',
      ...args,
    ],
    {
      cwd: dir,
      encoding: 'utf8',
      env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' },
    },
  ).trim();
}

/** A REST answer: the status and the JSON object of the body (`null` when it has none). */
export type Reply = { status: number; body: Record<string, unknown> | null };

/** The fields of a project that the Fleet Git flows read (`GET /api/projects`). */
export type ProjectRow = {
  project_id: string;
  homes?: { state: string }[];
  git: { sync: boolean; push: string };
  remote_url: string | null;
};

/** The board's REST API as the owner: its own login link, cookie and CSRF token. */
export class BoardApi {
  private constructor(
    private readonly http: APIRequestContext,
    private readonly csrf: string,
  ) {}

  static async login(deck: Deck): Promise<BoardApi> {
    const link = new URL(deck.loginLink());
    const token = new URLSearchParams(link.hash.slice(1)).get('login');
    if (token === null) throw new Error(`no login token in ${link.href}`);
    const http = await request.newContext({ baseURL: link.origin, ignoreHTTPSErrors: true });
    const reply = await http.post('/api/auth/link', { data: { token } });
    if (!reply.ok()) throw new Error(`board login failed (${reply.status()})`);
    const body = (await reply.json()) as { csrf?: string };
    if (typeof body.csrf !== 'string') throw new Error('the login gave no CSRF token');
    return new BoardApi(http, body.csrf);
  }

  async get(path: string): Promise<Reply> {
    const reply = await this.http.get(path);
    return {
      status: reply.status(),
      body: (await reply.json().catch(() => null)) as Reply['body'],
    };
  }

  async post(path: string, data: unknown): Promise<Reply> {
    const reply = await this.http.post(path, { data, headers: { 'X-CSRF-Token': this.csrf } });
    return {
      status: reply.status(),
      body: (await reply.json().catch(() => null)) as Reply['body'],
    };
  }

  async dispose(): Promise<void> {
    await this.http.dispose();
  }
}

/** The fields of a station session summary (`station sessions --json`) that the Fleet Git flows read. */
export type GitSession = {
  session_id: string;
  callsign: string | null;
  branch: string | null;
  worktree: string | null;
  base: unknown;
  current_turn: unknown;
  usage: { input_tokens: number | null };
  push?: { state: string; oid?: string; ref_name?: string };
};

/** Polls `read` until it returns a value (not `null`), or throws after the wait. */
export async function waitFor<T>(
  what: string,
  read: () => T | null | Promise<T | null>,
): Promise<T> {
  const deadline = Date.now() + WAIT_MS;
  for (;;) {
    const value = await read();
    if (value !== null) return value;
    if (Date.now() > deadline) throw new Error(`timed out: ${what}`);
    await sleep(200);
  }
}

/**
 * One deck and one station `alpha` (it dials the deck) with a bare repository `remote.git` below
 * its `~/projects` (an allowed root), and the clone `~/projects/app` as the home of a project.
 * The station allows local remotes (design §16.1), so the `file://` remote can be saved.
 */
export class GitRig {
  private constructor(
    readonly deck: Deck,
    readonly station: Station,
    readonly api: BoardApi,
    /** The bare repository. */
    readonly remote: string,
    /** The clone that is the home of the project. */
    readonly home: string,
  ) {}

  static async create(): Promise<GitRig> {
    const deck = await Deck.create();
    const station = new Station('alpha', ['allow_local_remotes = true']);
    // A land commits on the station: its Git needs an identity (else the land refuses).
    writeFileSync(
      join(station.home, '.gitconfig'),
      '[user]\n\tname = Fleet Deck Test\n\temail = test@fleetdeck.invalid\n',
    );
    const projects = join(station.home, 'projects');
    mkdirSync(projects, { recursive: true });
    const real = realpathSync(projects);
    const remote = join(real, 'remote.git');
    git(real, ['init', '-q', '--bare', '-b', 'main', remote]);
    const seed = join(station.root, 'seed');
    mkdirSync(seed);
    git(seed, ['init', '-q', '-b', 'main']);
    writeFileSync(join(seed, 'README.md'), 'one\n');
    git(seed, ['add', '--', 'README.md']);
    git(seed, ['commit', '-q', '-m', 'first']);
    git(seed, ['remote', 'add', 'origin', remote]);
    git(seed, ['push', '-q', 'origin', 'main']);
    const home = join(real, 'app');
    git(real, ['clone', '-q', `file://${remote}`, home]);
    await deck.start();
    await station.start();
    const rig = new GitRig(deck, station, await BoardApi.login(deck), remote, realpathSync(home));
    station.cli(['station', 'join', '--stdin'], deck.cli(['deck', 'invite']));
    // The deck needs `station.info.fleet_git` before it saves a `file://` remote.
    await waitFor('the station is linked', () => {
      const seen = JSON.parse(deck.cli(['deck', 'stations', '--json'])) as {
        reachable: boolean;
        info: { fleet_git?: unknown } | null;
      }[];
      const ready = seen.length === 1 && seen.every((s) => s.reachable && s.info?.fleet_git);
      return ready ? true : null;
    });
    return rig;
  }

  get remoteUrl(): string {
    return `file://${this.remote}`;
  }

  /** `POST /api/projects` for the home; it returns the project id. */
  async createProject(name: string): Promise<string> {
    return waitFor('the project is created', async () => {
      const reply = await this.api.post('/api/projects', {
        name,
        station: this.station.name,
        path: this.home,
        trackers: [],
      });
      if (reply.status === 200) return reply.body?.['project_id'] as string;
      const waiting = reply.status === 404 || reply.body?.['error'] === 'station_unreachable';
      if (!waiting) throw new Error(`project create failed: ${JSON.stringify(reply.body)}`);
      return null;
    });
  }

  /** The project as `GET /api/projects` lists it. */
  async project(id: string): Promise<ProjectRow> {
    const reply = await this.api.get('/api/projects');
    const found = (reply.body?.['projects'] as ProjectRow[] | undefined)?.find(
      (p) => p.project_id === id,
    );
    if (found === undefined) throw new Error(`no project ${id}`);
    return found;
  }

  /** A worktree session in the home (the deck gives it a Fleet Git branch). Returns its id. */
  async startWorktree(): Promise<string> {
    const before = new Set(this.sessions().map((s) => s.session_id));
    const reply = await this.api.post('/api/commands/session.start', {
      station: this.station.name,
      command_id: `rig-${Date.now()}`,
      command: {
        op: 'start',
        spec: { provider_instance: PROVIDER, cwd: this.home },
        prompt: { text: 'hello' },
        workspace: { kind: 'worktree' },
      },
    });
    const receipt = reply.body?.['receipt'] as { state?: string } | undefined;
    if (reply.status !== 200 || receipt?.state !== 'accepted') {
      throw new Error(`session start refused: ${JSON.stringify(reply.body)}`);
    }
    const fresh = await waitFor('the new session', () => {
      const found = this.sessions().find((s) => !before.has(s.session_id));
      return found === undefined ? null : found;
    });
    return fresh.session_id;
  }

  sessions(): GitSession[] {
    return JSON.parse(this.station.cli(['station', 'sessions', '--json']));
  }

  /** Waits until `ok` holds for the session as the station lists it. */
  async waitSession(what: string, id: string, ok: (s: GitSession) => boolean): Promise<GitSession> {
    return waitFor(what, () => {
      const found = this.sessions().find((s) => s.session_id === id);
      return found !== undefined && ok(found) ? found : null;
    });
  }

  /** The session is idle after its first turn. */
  firstTurn(id: string): Promise<GitSession> {
    return this.waitSession(
      'the first turn',
      id,
      (s) => s.base != null && s.current_turn == null && s.usage.input_tokens != null,
    );
  }

  async dispose(): Promise<void> {
    await this.api.dispose();
    await this.station.dispose();
    await this.deck.dispose();
  }
}
