import type { ShellChange, ShellInfo } from '../gen';

/** The board side of the human shell (PLAN §6.8, DL-404). Owner-only: admin scope. */
export type ShellActions = {
  list(station: string): Promise<ShellInfo[]>;
  open(station: string, cwd: string, cols: number, rows: number): Promise<string>;
  resize(station: string, shellId: string, cols: number, rows: number): Promise<void>;
  close(station: string, shellId: string): Promise<void>;
  /** Run the provider TUI of an ended session in a station PTY (DL-140); returns the shell id. */
  handover(station: string, sessionId: string, cols: number, rows: number): Promise<string>;
  /** The owner clears a terminal hold whose process the station cannot see (DL-140). */
  release(station: string, shellId: string): Promise<void>;
  /** The WebSocket path of one shell (relative to the board origin). */
  socketPath(station: string, shellId: string): string;
};

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function shellInfo(v: unknown): ShellInfo {
  if (!record(v)) throw new Error('Invalid shell list');
  const state = v['state'];
  const exit = v['exit_code'];
  if (
    typeof v['shell_id'] !== 'string' ||
    typeof v['cwd'] !== 'string' ||
    (state !== 'running' && state !== 'exited') ||
    typeof v['cols'] !== 'number' ||
    typeof v['rows'] !== 'number' ||
    typeof v['created_at'] !== 'number' ||
    !(exit === null || exit === undefined || typeof exit === 'number') ||
    !(v['session_id'] === undefined || typeof v['session_id'] === 'string')
  ) {
    throw new Error('Invalid shell list');
  }
  return v as ShellInfo;
}

export function shellList(v: unknown): ShellInfo[] {
  if (!Array.isArray(v)) throw new Error('Invalid shell list');
  return v.map(shellInfo);
}

/** A command id (and shell id) from [0-9A-Za-z_-], at most 64 characters. */
export function newId(prefix: string, random: () => number = Math.random): string {
  const alphabet = '0123456789abcdefghijklmnopqrstuvwxyz';
  let s = '';
  for (let i = 0; i < 16; i += 1) s += alphabet[Math.floor(random() * alphabet.length)];
  return `${prefix}-${s}`;
}

/** Terminal cells the station accepts (1 to 1000). */
export function cells(n: number): number {
  return Math.min(1000, Math.max(1, Math.floor(Number.isFinite(n) ? n : 1)));
}

export function shellActions(
  get: Get,
  post: Post,
  random: () => number = Math.random,
): ShellActions {
  const base = (station: string) => `/api/stations/${encodeURIComponent(station)}/shells`;
  const send = (station: string, change: ShellChange) =>
    post(base(station), { command_id: newId('sh', random), change });
  return {
    list: async (station) => shellList(await get(base(station))),
    open: async (station, cwd, cols, rows) => {
      const shellId = newId('shell', random);
      await send(station, {
        op: 'open',
        shell_id: shellId,
        cwd,
        cols: cells(cols),
        rows: cells(rows),
      });
      return shellId;
    },
    resize: async (station, shellId, cols, rows) => {
      await send(station, {
        op: 'resize',
        shell_id: shellId,
        cols: cells(cols),
        rows: cells(rows),
      });
    },
    close: async (station, shellId) => {
      await send(station, { op: 'close', shell_id: shellId });
    },
    handover: async (station, sessionId, cols, rows) => {
      const shellId = newId('tui', random);
      await send(station, {
        op: 'handover',
        shell_id: shellId,
        session_id: sessionId,
        cols: cells(cols),
        rows: cells(rows),
      });
      return shellId;
    },
    release: async (station, shellId) => {
      await send(station, { op: 'release', shell_id: shellId });
    },
    socketPath: (station, shellId) => `${base(station)}/${encodeURIComponent(shellId)}/attach`,
  };
}
