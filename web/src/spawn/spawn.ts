// M2 P4-4: the spawn form's calls (PLAN §11 "Spawn form", Phase 4 exit).
//
// The directory picker reads `GET /api/stations/<id>/workspaces?path=` (the
// station lists its allowed roots through the link; drive scope). A start is
// one `session.start` command with a fresh command id; the board never
// resends it (A9). M3 6-1 (DL-200, DL-202): a start can ask for a new
// worktree or a clone, and a batch starts one session for each line
// (`3x task` starts three), each in its own worktree.
import type {
  DeckSession,
  ModelChoice,
  Preflight,
  SessionSpec,
  StationProvider,
  StationView,
  WorkspaceListing,
  WorkspaceRequest,
} from '../gen';
import { limitSuffix, type RateMeter } from '../board/usage';
import { commandId, type Outcome, outcomeOf } from '../session/commands';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

export type SpawnOutcome = Outcome & { readonly sessionId: string | null };

/** The answer of a clone access check (DL-144): the station's verdict, or why there is none. */
export type PreflightResult =
  | { readonly ok: true; readonly preflight: Preflight }
  | { readonly ok: false; readonly text: string };

export type SpawnActions = {
  readonly canDrive: boolean;
  list(stationId: string, path: string | null): Promise<WorkspaceListing>;
  /** Asks the station whether it can read the clone URL with its own Git credentials (DL-144). */
  preflight(stationId: string, url: string): Promise<PreflightResult>;
  /** With `importId`: a `session.import` of that native CLI session (DL-308). */
  start(
    stationId: string,
    spec: SessionSpec,
    prompt: string,
    workspace?: WorkspaceRequest | null,
    importId?: string,
    /** DL-148: a shell command that the station runs in the workspace before the agent starts. */
    setupCmd?: string,
  ): Promise<SpawnOutcome>;
};

/** The longest native session id that `session.import` accepts (Rust mirror). */
export const MAX_NATIVE_SESSION_ID = 128;

/**
 * Why a native CLI session id cannot be imported, or `null` (mirrors
 * `check_native_session_id`: letters, digits, `-`, `_`, `.`; never a flag).
 */
export function importIdProblem(id: string): string | null {
  if (id.length === 0 || id.length > MAX_NATIVE_SESSION_ID) {
    return 'The session id must have 1 to 128 characters.';
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(id)) {
    return 'The session id may hold only letters, digits, "-", "_" and ".", and starts with a letter or a digit.';
  }
  return null;
}

/** The longest branch or base name that the station accepts. */
export const MAX_REF = 200;

/**
 * Why a base cannot be used, or `null` (mirrors the station: 1 to 200
 * characters, no leading `-`, no whitespace or control characters).
 */
export function baseProblem(name: string): string | null {
  if (
    name.length === 0 ||
    name.length > MAX_REF ||
    name.startsWith('-') ||
    /[\s\u0000-\u001f\u007f]/.test(name)
  ) {
    return 'the base is not valid';
  }
  return null;
}

/**
 * Why a branch name cannot be used, or `null` (mirrors the station: the
 * base rule, no `..`, none of `~^:?*[\`, not ending in `/` or `.lock`).
 */
export function branchProblem(name: string): string | null {
  if (
    baseProblem(name) !== null ||
    name.includes('..') ||
    /[~^:?*[\\]/.test(name) ||
    name.endsWith('/') ||
    name.endsWith('.lock')
  ) {
    return 'the branch name is not valid';
  }
  return null;
}

/**
 * The workspace request of a start (DL-144): `null` works in the chosen
 * directory. A batch forces a plain worktree per session, so it never
 * carries a branch or a base; a base needs a branch.
 */
export function workspaceRequest(
  place: 'here' | 'worktree' | 'clone',
  batch: boolean,
  cloneUrl: string,
  branch: string,
  base: string,
): WorkspaceRequest | null {
  if (place === 'clone') return { kind: 'clone', url: cloneUrl.trim() };
  if (place === 'here' && !batch) return null;
  const b = batch ? '' : branch.trim();
  const from = base.trim();
  return { kind: 'worktree', ...(b ? { branch: b } : {}), ...(b && from ? { base: from } : {}) };
}

/** The longest setup command that the station accepts (characters). */
export const MAX_SETUP_CMD = 4096;

/**
 * Why a setup command cannot be sent, or `null` (mirrors the station: 1 to
 * 4096 characters, no NUL). An empty or blank command means no setup.
 */
export function setupProblem(cmd: string): string | null {
  if (cmd.trim() === '') return null;
  if (Array.from(cmd).length > MAX_SETUP_CMD) {
    return 'The setup command is longer than 4096 characters.';
  }
  if (cmd.includes('\0')) return 'The setup command contains a NUL character.';
  return null;
}

/** The most sessions that one batch line can ask for (`10x`). */
export const MAX_BATCH = 10;

/**
 * The prompts of a batch (v0 batch spawn, PLAN §16): one session for each
 * non-empty line; `Nx task` (N from 1 to 10) repeats the task N times.
 */
export function batchPrompts(text: string): string[] {
  const prompts: string[] = [];
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (line === '') continue;
    const m = /^(\d{1,2})\s*[xX×]\s+(.+)$/.exec(line);
    const n = m === null ? 1 : Number(m[1]);
    const task = m === null ? line : (m[2] ?? '').trim();
    if (n < 1 || n > MAX_BATCH || task === '') {
      prompts.push(line);
      continue;
    }
    for (let i = 0; i < n; i += 1) prompts.push(task);
  }
  return prompts;
}

/**
 * Permission modes that the form offers for a provider instance: the modes
 * of a new session that the station reports (M2 P5-3, DL-065), most
 * restrictive first. An instance without reported capabilities offers none:
 * the start then uses the station's default mode.
 */
export function spawnModes(p: StationProvider | undefined): readonly string[] {
  return p?.capabilities?.modes ?? [];
}

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** Validates a `workspace.list` answer. */
export function listingView(v: unknown): WorkspaceListing {
  if (
    !record(v) ||
    !(v['path'] === null || typeof v['path'] === 'string') ||
    !(v['parent'] === null || typeof v['parent'] === 'string') ||
    typeof v['truncated'] !== 'boolean' ||
    !Array.isArray(v['entries']) ||
    !v['entries'].every(
      (e) =>
        record(e) &&
        typeof e['name'] === 'string' &&
        typeof e['path'] === 'string' &&
        typeof e['repository'] === 'boolean',
    )
  ) {
    throw new Error('Invalid directory listing');
  }
  return v as WorkspaceListing;
}

/** Stations that can start a session now: linked, with reported providers. */
export function spawnTargets(stations: readonly StationView[]): StationView[] {
  return stations.filter((s) => s.reachable && (s.info?.providers.length ?? 0) > 0);
}

/** The most pinned directories that one station keeps. */
export const MAX_PINS = 20;
/** The most unpinned recent directories that the form offers. */
export const MAX_RECENT = 6;
const PINS_KEY = 'fd-dir-pins';

export type DirPins = Readonly<Record<string, readonly string[]>>;

/** The pinned directories of this browser, by station (DL-141); empty when storage fails. */
export function readPins(): DirPins {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(PINS_KEY) ?? '{}');
    if (!record(raw)) return {};
    const out: Record<string, string[]> = {};
    for (const [id, list] of Object.entries(raw)) {
      if (Array.isArray(list)) out[id] = list.filter((p): p is string => typeof p === 'string');
    }
    return out;
  } catch {
    return {};
  }
}

export function writePins(pins: DirPins): void {
  try {
    localStorage.setItem(PINS_KEY, JSON.stringify(pins));
  } catch {
    // Storage is blocked or full: the pins live for this page only.
  }
}

/** Pins `path` for the station (newest first, at most 20), or unpins it when it is pinned. */
export function togglePin(pins: DirPins, stationId: string, path: string): DirPins {
  const list = pins[stationId] ?? [];
  const next = list.includes(path)
    ? list.filter((p) => p !== path)
    : [path, ...list].slice(0, MAX_PINS);
  return { ...pins, [stationId]: next };
}

/** The most remembered setup commands. */
export const MAX_SETUP_CMDS = 50;
const SETUP_KEY = 'fd-setup-cmds';

/** The memory key of a setup command: one for each station and directory. */
export function setupKey(stationId: string, cwd: string): string {
  return `${stationId}\n${cwd}`;
}

/** The remembered setup commands of this browser (DL-148); empty when storage fails. */
export function readSetupCmds(): Record<string, string> {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(SETUP_KEY) ?? '{}');
    if (!record(raw)) return {};
    const out: Record<string, string> = {};
    for (const [key, cmd] of Object.entries(raw)) {
      if (typeof cmd === 'string') out[key] = cmd;
    }
    return out;
  } catch {
    return {};
  }
}

/** Remembers `cmd` for `key`, or forgets the key when `cmd` is `null` (50 keys at most, oldest dropped). */
export function rememberSetupCmd(key: string, cmd: string | null): void {
  try {
    const all = readSetupCmds();
    if (cmd === null) {
      delete all[key];
    } else {
      all[key] = cmd;
      const keys = Object.keys(all);
      for (const old of keys.slice(0, Math.max(0, keys.length - MAX_SETUP_CMDS))) delete all[old];
    }
    localStorage.setItem(SETUP_KEY, JSON.stringify(all));
  } catch {
    // Storage is blocked or full: nothing is remembered.
  }
}

/** The last two parts of a path (`/home/me/app` gives `me/app`); `/` stays `/`. */
export function lastParts(path: string): string {
  const parts = path.split('/').filter((p) => p !== '');
  return parts.length === 0 ? '/' : parts.slice(-2).join('/');
}

/**
 * The recent directories of a station: the `cwd` of its sessions, newest
 * `updated_at` first, each once, without the pinned ones, at most 6.
 */
export function recentDirs(
  sessions: readonly DeckSession[],
  stationId: string,
  pinned: readonly string[],
): string[] {
  const seen = new Set(pinned);
  const out: string[] = [];
  const mine = sessions
    .filter((s) => s.station_id === stationId && s.summary.cwd !== null && s.summary.cwd !== '')
    .sort((a, b) => b.summary.updated_at - a.summary.updated_at);
  for (const s of mine) {
    const cwd = s.summary.cwd;
    if (cwd === null || seen.has(cwd)) continue;
    seen.add(cwd);
    out.push(cwd);
    if (out.length === MAX_RECENT) break;
  }
  return out;
}

/**
 * The models that the provider reported (DL-151): the list of the newest
 * session of the station and the provider instance that has one, or `null`.
 */
export function knownModels(
  sessions: readonly DeckSession[],
  stationId: string,
  instance: string,
): readonly ModelChoice[] | null {
  let best: DeckSession | null = null;
  for (const s of sessions) {
    if (s.station_id !== stationId || s.summary.provider_instance !== instance) continue;
    if ((s.summary.models?.length ?? 0) === 0) continue;
    if (best === null || s.summary.updated_at > best.summary.updated_at) best = s;
  }
  return best?.summary.models ?? null;
}

export function providerLabel(p: StationProvider): string {
  const version = p.version === null ? 'version unknown' : p.version;
  return `${p.instance} · ${p.provider} · ${version}`;
}

/**
 * The option text of the provider select (R4.3): the label, the tightest
 * limit of the instance ("42% of five_hour, resets in 1 h") and "(most room)".
 */
export function providerChoiceLabel(
  p: StationProvider,
  limit: RateMeter | null,
  now: number,
  mostRoom: boolean,
): string {
  const suffix = limitSuffix(limit, now);
  return `${providerLabel(p)}${suffix === '' ? '' : ` · ${suffix}`}${mostRoom ? ' (most room)' : ''}`;
}

/** The warning under the provider select for a CLI version that the station
 * marked (R8.2, DL-162); `null` for an `ok` or `unknown` version. */
export function versionWarning(p: StationProvider | undefined): string | null {
  if (p === undefined) return null;
  const version = p.version ?? 'unknown';
  if (p.version_status === 'older_than_tested') {
    return `This ${p.provider} CLI (${version}) is older than the version that Fleet Deck is tested with. It still starts, but a feature may not work.`;
  }
  if (p.version_status === 'broken') {
    return `This ${p.provider} version (${version}) is known to break the protocol. The station refuses to start a session with it.`;
  }
  return null;
}

export function spawnActions(
  get: Get,
  post: Post,
  scopes: readonly string[],
  newId: () => string = commandId,
): SpawnActions {
  return {
    canDrive: scopes.includes('drive'),
    list: async (stationId, path) => {
      const query = path === null ? '' : `?path=${encodeURIComponent(path)}`;
      return listingView(
        await get(`/api/stations/${encodeURIComponent(stationId)}/workspaces${query}`),
      );
    },
    preflight: async (stationId, url) => {
      try {
        const reply = await post(
          `/api/stations/${encodeURIComponent(stationId)}/workspace/preflight`,
          { url },
        );
        if (
          !record(reply) ||
          typeof reply['ok'] !== 'boolean' ||
          typeof reply['message'] !== 'string' ||
          !(reply['hint'] === null || typeof reply['hint'] === 'string')
        ) {
          return { ok: false, text: 'The station gave an answer that the board cannot read.' };
        }
        return { ok: true, preflight: reply as Preflight };
      } catch (error) {
        return { ok: false, text: error instanceof Error ? error.message : 'The request failed.' };
      }
    },
    start: async (stationId, spec, prompt, workspace = null, importId, setupCmd) => {
      const first = prompt.trim() === '' ? null : { text: prompt };
      const native = importId?.trim() ?? '';
      const command =
        native === ''
          ? {
              op: 'start',
              spec,
              prompt: first,
              ...(workspace === null ? {} : { workspace }),
              ...(setupCmd !== undefined && setupCmd.trim() !== '' ? { setup_cmd: setupCmd } : {}),
            }
          : { op: 'import', spec, native_session_id: native, prompt: first };
      const body = { station: stationId, command_id: newId(), command };
      try {
        const reply = await post(`/api/commands/session.${command.op}`, body);
        const outcome = outcomeOf(reply);
        const result =
          record(reply) && record(reply['receipt']) && record(reply['receipt']['result'])
            ? reply['receipt']['result']
            : null;
        const sessionId =
          outcome.ok && result !== null && typeof result['session_id'] === 'string'
            ? result['session_id']
            : null;
        return { ...outcome, sessionId };
      } catch (error) {
        return {
          ok: false,
          text: error instanceof Error ? error.message : 'The request failed.',
          sessionId: null,
        };
      }
    },
  };
}
