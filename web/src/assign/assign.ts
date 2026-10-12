// M3 6-3 (PLAN §7.3, DL-204): `assign auto: <task>`. The deck picks the best
// connected session (idle first, least buried, right repo, right provider)
// and sends the task to it. When no session fits, the deck names the best
// station for a new session and the board offers "Spawn on <station>".
import type { AssignOutcome } from '../gen';
import { commandId } from '../session/commands';

type Post = (path: string, body: unknown) => Promise<unknown>;

export type AssignResult = {
  readonly ok: boolean;
  readonly text: string;
  /** The station id and name for a new session (unrouted only). */
  readonly spawnOn: { readonly stationId: string; readonly name: string } | null;
};

export type AssignActions = {
  assign(text: string, repo: string): Promise<AssignResult>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The text of an assign answer. */
export function assignResult(v: unknown): AssignResult {
  if (!record(v) || (v['outcome'] !== 'assigned' && v['outcome'] !== 'unrouted')) {
    return { ok: false, text: 'The deck sent an invalid assign response.', spawnOn: null };
  }
  const o = v as AssignOutcome;
  if (o.outcome === 'assigned') {
    const who = o.callsign ?? o.session_id;
    const sent = o.state === 'done' ? 'Assigned to' : 'Sent to';
    return { ok: true, text: `${sent} ${who}.`, spawnOn: null };
  }
  const spawnOn =
    o.spawn_on === null ? null : { stationId: o.spawn_on.station_id, name: o.spawn_on.name };
  const text =
    spawnOn === null
      ? 'No session fits, and no station can start a new one.'
      : `No session fits. ${spawnOn.name} can start a new session.`;
  return { ok: false, text, spawnOn };
}

export function assignActions(post: Post, newId: () => string = commandId): AssignActions {
  return {
    assign: async (text, repo) => {
      const body = {
        command_id: newId(),
        text,
        ...(repo.trim() === '' ? {} : { repo: repo.trim() }),
      };
      try {
        return assignResult(await post('/api/assign', body));
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return { ok: false, text: `Not assigned: ${message}`, spawnOn: null };
      }
    },
  };
}
