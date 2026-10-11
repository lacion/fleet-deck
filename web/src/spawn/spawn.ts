// M2 P4-4: the spawn form's calls (PLAN §11 "Spawn form", Phase 4 exit).
//
// The directory picker reads `GET /api/stations/<id>/workspaces?path=` (the
// station lists its allowed roots through the link; drive scope). A start is
// one `session.start` command with a fresh command id; the board never
// resends it (A9). Station "auto", worktrees, batch and Nx are Phase 6
// (DL-061).
import type { SessionSpec, StationProvider, StationView, WorkspaceListing } from '../gen';
import { commandId, type Outcome, outcomeOf } from '../session/commands';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

export type SpawnOutcome = Outcome & { readonly sessionId: string | null };

export type SpawnActions = {
  readonly canDrive: boolean;
  list(stationId: string, path: string | null): Promise<WorkspaceListing>;
  start(stationId: string, spec: SessionSpec, prompt: string): Promise<SpawnOutcome>;
};

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
    start: async (stationId, spec, prompt) => {
      const body = {
        station: stationId,
        command_id: newId(),
        command: {
          op: 'start',
          spec,
          prompt: prompt.trim() === '' ? null : { text: prompt },
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
