import { basename } from '../app/sessions';
import { commandId, type Outcome, outcomeOf } from '../session/commands';
import type { RecountCall } from './usageRecountCall';
import type { ShellActions } from '../shell/shell';
import type {
  AddStationRequest,
  PairingInvitation,
  StationView,
  WorktreeInfo,
  WorktreeList,
} from '../gen';

/** The pairing calls of the stations screen (admin scope). */
export type StationActions = {
  readonly canAdmin: boolean;
  invite(): Promise<PairingInvitation>;
  add(request: AddStationRequest): Promise<StationView>;
  remove(name: string): Promise<StationView>;
  /** The owner's station shells (PLAN §6.8); admin scope. */
  readonly shells?: ShellActions | undefined;
  /** The read-only worktree inventory of a station (DL-142); owner only: admin scope. */
  readonly worktrees?: ((station: string) => Promise<WorktreeList>) | undefined;
  /** Move to trash, restore, delete permanently or stop tracking a worktree (DL-150); owner only. */
  readonly worktreeAction?: ReturnType<typeof worktreeActor> | undefined;
  /** Recount the old usage count of a session (B20); the owner's board login only. */
  readonly usageRecount?: RecountCall | undefined;
};

type Post = (path: string, body: unknown) => Promise<unknown>;
type Get = (path: string) => Promise<unknown>;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

export function invitationView(v: unknown): PairingInvitation {
  if (
    !record(v) ||
    typeof v['invitation'] !== 'string' ||
    v['invitation'].length === 0 ||
    typeof v['join_command'] !== 'string' ||
    typeof v['expires_at'] !== 'number' ||
    !Number.isSafeInteger(v['expires_at'])
  ) {
    throw new Error('Invalid invitation response');
  }
  return v as PairingInvitation;
}

export function stationResult(v: unknown): StationView {
  if (!record(v) || typeof v['station_id'] !== 'string' || typeof v['name'] !== 'string') {
    throw new Error('Invalid station response');
  }
  return v as StationView;
}

/** The board-API pairing calls over one authenticated connection. */
export function stationActions(post: Post, canAdmin: boolean): StationActions {
  return {
    canAdmin,
    invite: async () => invitationView(await post('/api/stations/invite', {})),
    add: async (request) => stationResult(await post('/api/stations', request)),
    remove: async (name) =>
      stationResult(await post(`/api/stations/${encodeURIComponent(name)}/remove`, {})),
  };
}

/** `GET /api/stations/{station}/worktrees` (DL-142). */
export function worktreeLoader(get: Get): (station: string) => Promise<WorktreeList> {
  return async (station) => {
    const v = await get(`/api/stations/${encodeURIComponent(station)}/worktrees`);
    if (!record(v) || !Array.isArray(v['worktrees'])) {
      throw new Error('The deck sent an invalid worktree list.');
    }
    return v as WorktreeList;
  };
}

export type WorktreeAction = 'trash' | 'restore' | 'purge' | 'release';

const PENDING: Record<WorktreeAction, string> = {
  trash: 'Moving the worktree to the trash…',
  restore: 'Restoring the worktree…',
  purge: 'Deleting the trashed folder…',
  release: 'Fleet Deck no longer tracks this folder.',
};

/**
 * `POST /api/stations/{station}/worktrees/{action}` (DL-150). An accepted command only starts the
 * work: the outcome shows in the worktree inventory.
 */
export function worktreeActor(
  post: Post,
): (
  station: string,
  action: WorktreeAction,
  sessionId: string,
  confirmName?: string,
) => Promise<Outcome> {
  return async (station, action, sessionId, confirmName) => {
    try {
      const body = {
        session_id: sessionId,
        command_id: commandId(),
        ...(confirmName ? { confirm_name: confirmName } : {}),
      };
      const answer = await post(
        `/api/stations/${encodeURIComponent(station)}/worktrees/${action}`,
        body,
      );
      if (record(answer) && answer['state'] === 'done' && record(answer['receipt'])) {
        const receipt = answer['receipt'];
        if (receipt['state'] === 'accepted') return { ok: true, text: PENDING[action] };
        if (receipt['state'] === 'rejected') {
          const reject = receipt['reject'];
          const message = record(reject) ? reject['message'] : undefined;
          return {
            ok: false,
            text: typeof message === 'string' ? message : 'The station refused it.',
          };
        }
      }
      return outcomeOf(answer);
    } catch (error) {
      return { ok: false, text: error instanceof Error ? error.message : 'The request failed.' };
    }
  };
}

/** True when the typed text is exactly the name of the trash folder. */
export function confirmMatches(input: string, trashPath: string): boolean {
  const name = basename(trashPath);
  return name !== '' && input === name;
}

/** Single-quotes a value for a POSIX shell: `'` becomes `'\''`. */
const shellQuote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;

/**
 * The command that removes one worktree (and a branch that starts with `fd/`).
 * The board never runs it: the person copies it. `null` when it does not apply.
 */
export function removeCommand(w: WorktreeInfo): string | null {
  if (!w.exists || w.live || w.main === null) return null;
  const main = shellQuote(w.main);
  const remove = `git -C ${main} worktree remove ${shellQuote(w.path)}`;
  return w.branch?.startsWith('fd/') === true
    ? `${remove} && git -C ${main} branch -d ${shellQuote(w.branch)}`
    : remove;
}

/** A name for the deck to use: trimmed, or `null` for the station's own name. */
export function requestedName(value: string): string | null {
  const name = value.trim();
  return name === '' ? null : name;
}

export function authLabel(auth: string): string {
  switch (auth) {
    case 'signed_in':
      return 'signed in';
    case 'signed_out':
      return 'signed out';
    default:
      return 'sign-in unknown';
  }
}

export function expiresText(at: number): string {
  return `${new Date(at).toISOString().slice(0, 16).replace('T', ' ')} UTC`;
}
