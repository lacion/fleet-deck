// M2 P4-4: the spawn form's calls (PLAN §11 "Spawn form", Phase 4 exit).
//
// The directory picker reads `GET /api/stations/<id>/workspaces?path=` (the
// station lists its allowed roots through the link; drive scope). A start is
// one `session.start` command with a fresh command id; the board never
// resends it (A9). M3 6-1 (DL-200, DL-202): a start can ask for a new
// worktree or a clone, and a batch starts one session for each line
// (`3x task` starts three), each in its own worktree.
import type {
  SessionSpec,
  StationProvider,
  StationView,
  WorkspaceListing,
  WorkspaceRequest,
} from '../gen';
import { commandId, type Outcome, outcomeOf } from '../session/commands';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

export type SpawnOutcome = Outcome & { readonly sessionId: string | null };

export type SpawnActions = {
  readonly canDrive: boolean;
  list(stationId: string, path: string | null): Promise<WorkspaceListing>;
  start(
    stationId: string,
    spec: SessionSpec,
    prompt: string,
    workspace?: WorkspaceRequest | null,
  ): Promise<SpawnOutcome>;
};

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

export function providerLabel(p: StationProvider): string {
  const version = p.version === null ? 'version unknown' : p.version;
  return `${p.instance} · ${p.provider} · ${version}`;
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
    start: async (stationId, spec, prompt, workspace = null) => {
      const body = {
        station: stationId,
        command_id: newId(),
        command: {
          op: 'start',
          spec,
          prompt: prompt.trim() === '' ? null : { text: prompt },
          ...(workspace === null ? {} : { workspace }),
        },
      };
      try {
        const reply = await post('/api/commands/session.start', body);
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
