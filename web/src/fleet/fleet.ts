// M3 6-2 / 6-4 (DL-205, DL-206): fleet mail and tasks. The deck stores a
// mail or a task once and its outbox delivers the framed text to the target
// session; a task stays open until the session (or the owner) completes it.
import type { GrantView, TaskView } from '../gen';
import { commandId } from '../session/commands';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

export type FleetTarget = { readonly stationId: string; readonly sessionId: string };

export type FleetResult = { readonly ok: boolean; readonly text: string };

export type FleetActions = {
  /** `mailId` is the `mail_id` of the request; a retry sends the id of its first try. */
  mail(target: FleetTarget, text: string, steer: boolean, mailId?: string): Promise<FleetResult>;
  task(target: FleetTarget, title: string): Promise<FleetResult>;
  tasks(): Promise<readonly TaskView[]>;
  grants(): Promise<readonly GrantView[]>;
  revoke(grantId: string): Promise<FleetResult>;
  /** Renews an idle-expired grant (ADR-006 test 25b). */
  renew(grantId: string): Promise<FleetResult>;
};

/** One target of a mail send: its session ref and where the mail goes. */
export type SendTarget = { readonly ref: string; readonly target: FleetTarget };

/** A target of the last send with its own `mail_id`; a `failed` one is sent again on a retry. */
export type SendEntry = SendTarget & {
  readonly mailId: string;
  readonly state: 'sent' | 'failed';
  readonly error?: string;
};

/** The last send: its text, steer flag, target set (`key`) and one entry for each target. */
export type SendRecord = {
  readonly text: string;
  readonly steer: boolean;
  readonly key: string;
  readonly entries: readonly SendEntry[];
};

/** A new `mail_id`, the generator `fleetActions` uses by default. */
export const newMailId = (): string => commandId();

/**
 * What to send now. The same text, steer flag and target set (any order) as the
 * record: only its failed entries, with their own ids. Anything else: a new record
 * with a new id for each target. An entry starts `failed` until its mail is sent.
 */
export function nextSends(
  record: SendRecord | null,
  targets: readonly SendTarget[],
  text: string,
  steer: boolean,
  newId: () => string = newMailId,
): { readonly record: SendRecord; readonly todo: readonly SendEntry[] } {
  const key = JSON.stringify(targets.map((t) => t.ref).sort());
  if (record !== null && record.text === text && record.steer === steer && record.key === key) {
    return { record, todo: record.entries.filter((e) => e.state === 'failed') };
  }
  const entries = targets.map((t): SendEntry => ({ ...t, mailId: newId(), state: 'failed' }));
  return { record: { text, steer, key, entries }, todo: entries };
}

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const failed = (what: string, error: unknown): FleetResult => ({
  ok: false,
  text: `${what}: ${error instanceof Error ? error.message : String(error)}`,
});

/** The tasks of a `GET /api/tasks` answer; anything else is no task. */
export function taskList(v: unknown): readonly TaskView[] {
  if (!Array.isArray(v)) return [];
  return v.filter(
    (t): t is TaskView =>
      record(t) && typeof t['task_id'] === 'string' && typeof t['title'] === 'string',
  );
}

/** The grants of a `GET /api/coordinator/grants` answer (M3 6-5). */
export function grantList(v: unknown): readonly GrantView[] {
  if (!Array.isArray(v)) return [];
  return v.filter(
    (g): g is GrantView =>
      record(g) && typeof g['grant_id'] === 'string' && typeof g['state'] === 'string',
  );
}

/** The words of a grant row: its classes, spawn use and state. */
export function grantStatus(g: GrantView): string {
  const spawn = g.classes.includes('spawn') ? `, spawned ${g.spawn_used} of ${g.spawn_cap}` : '';
  const state = g.state === 'active' && g.generation === null ? 'waiting for its session' : g.state;
  return `${g.classes.join(', ')}${spawn}: ${state}`;
}

/** The words of a task row: its state, then its delivery while open. */
export function taskStatus(t: TaskView): string {
  if (t.state === 'done') return t.result === null ? 'Done' : `Done: ${t.result}`;
  if (t.state === 'failed') return t.result === null ? 'Failed' : `Failed: ${t.result}`;
  if (t.delivery === 'dead') return `Not delivered${t.reason === null ? '' : `: ${t.reason}`}`;
  return t.delivery === 'delivered' ? 'Open' : 'Open, not yet delivered';
}

export function fleetActions(get: Get, post: Post, newId: () => string = commandId): FleetActions {
  return {
    mail: async (target, text, steer, mailId) => {
      try {
        await post('/api/mail', {
          mail_id: mailId ?? newId(),
          station: target.stationId,
          session_id: target.sessionId,
          text,
          delivery: steer ? 'steer' : 'follow_up',
        });
        return { ok: true, text: 'Mail queued.' };
      } catch (error) {
        return failed('Mail not sent', error);
      }
    },
    task: async (target, title) => {
      try {
        await post('/api/tasks', {
          task_id: newId(),
          title,
          station: target.stationId,
          session_id: target.sessionId,
        });
        return { ok: true, text: 'Task created.' };
      } catch (error) {
        return failed('Task not created', error);
      }
    },
    tasks: async () => {
      try {
        return taskList(await get('/api/tasks'));
      } catch {
        return [];
      }
    },
    grants: async () => {
      try {
        return grantList(await get('/api/coordinator/grants'));
      } catch {
        return [];
      }
    },
    revoke: async (grantId) => {
      try {
        await post(`/api/coordinator/grants/${encodeURIComponent(grantId)}/revoke`, {});
        return { ok: true, text: 'Grant revoked.' };
      } catch (error) {
        return failed('Grant not revoked', error);
      }
    },
    renew: async (grantId) => {
      try {
        await post(`/api/coordinator/grants/${encodeURIComponent(grantId)}/renew`, {});
        return { ok: true, text: 'Renewal sent; the grant is active when its station confirms.' };
      } catch (error) {
        return failed('Grant not renewed', error);
      }
    },
  };
}
