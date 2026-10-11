// Real processes for the P4-5 browser flows: one `fleetdeck deck serve` and
// two `fleetdeck station serve` processes on temporary nodes, each with its
// own home and data directory. Stations run the offline `fake-claude sim` and
// `fake-codex app-server` (`sim`) agents, so no provider account or network is
// used.
import { type ChildProcess, spawn, spawnSync } from 'node:child_process';
import {
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
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

  constructor(name: string) {
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

  /** `station run`: a session started on the station itself, without the
   * deck. Returns the new session id. */
  run(prompt: string, provider: string = PROVIDER): string {
    return this.cli(['station', 'run', '--provider', provider, '--cwd', this.workspace, prompt]);
  }

  /** Every JSON-RPC frame that the station wrote to its Codex agents. */
  codexInput(): CodexInput {
    const input: CodexInput = { turns: [], steers: [], methods: {}, answers: 0 };
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
        input.controls[subtype] = (input.controls[subtype] ?? 0) + 1;
      }
      if (frame.type === 'control_response') input.answers += 1;
    }
    return input;
  }
}
