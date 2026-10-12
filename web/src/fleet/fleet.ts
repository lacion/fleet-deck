// M3 6-2 / 6-4 (DL-205, DL-206): fleet mail and tasks. The deck stores a
// mail or a task once and its outbox delivers the framed text to the target
// session; a task stays open until the session (or the owner) completes it.
import type { TaskView } from '../gen';
import { commandId } from '../session/commands';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

export type FleetTarget = { readonly stationId: string; readonly sessionId: string };

export type FleetResult = { readonly ok: boolean; readonly text: string };

export type FleetActions = {
  mail(target: FleetTarget, text: string, steer: boolean): Promise<FleetResult>;
  task(target: FleetTarget, title: string): Promise<FleetResult>;
  tasks(): Promise<readonly TaskView[]>;
};

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

/** The words of a task row: its state, then its delivery while open. */
export function taskStatus(t: TaskView): string {
  if (t.state === 'done') return t.result === null ? 'Done' : `Done: ${t.result}`;
  if (t.state === 'failed') return t.result === null ? 'Failed' : `Failed: ${t.result}`;
  if (t.delivery === 'dead') return `Not delivered${t.reason === null ? '' : `: ${t.reason}`}`;
  return t.delivery === 'delivered' ? 'Open' : 'Open, not yet delivered';
}

export function fleetActions(get: Get, post: Post, newId: () => string = commandId): FleetActions {
  return {
    mail: async (target, text, steer) => {
      try {
        await post('/api/mail', {
          mail_id: newId(),
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
  };
}
