// M2 P4-3: session commands from the board (`POST /api/commands/<method>`).
//
// Each user action gets one fresh command id. The board never resends a
// command by itself: the deck owns retries, and an `ambiguous` command is
// shown as such (A9, PLAN §8). The station answers a refused command with a
// receipt; its message is shown as is.
import { ApiError } from '../runtime/api';
import type {
  DeckCommand,
  ExecutionCheck,
  ExecutionStopPlan,
  ImageInput,
  RevertResolution,
  RepoBranches,
  SessionCommand,
  SessionDiff,
  SessionHistory,
  SessionKey,
  SubagentLog,
  WorkflowsCursor,
  WorkflowsPage,
} from '../gen';
import { isSessionHistory } from '../chat/history';
import { type FleetAnswers, isFleetAnswers } from './fleetAnswers';
import type { Reply } from './question';
import { artifactFiles, type FileActions, fileActions } from './workspace';

type Post = (path: string, body: unknown) => Promise<unknown>;
type Get = (path: string) => Promise<unknown>;

export type Outcome = { readonly ok: boolean; readonly text: string };
export type InputKind = 'prompt' | 'steer' | 'follow_up';

export type SessionActions = {
  readonly canDrive: boolean;
  readonly canApprove: boolean;
  /** The `admin` scope: the owner checks a previous run of the session (DL-182). */
  readonly canAdmin: boolean;
  /** Pasted images go with the text (M3 6-10, DL-304). */
  send(
    kind: InputKind,
    text: string,
    images?: readonly ImageInput[],
    shellResults?: readonly string[],
  ): Promise<Outcome>;
  /** B8: run `command` in the session folder (owner only, `!` in the composer). */
  shellCommand(command: string): Promise<Outcome>;
  /** B8: the owner states that the shell command `effectId` no longer runs. */
  releaseShell(effectId: string): Promise<Outcome>;
  interrupt(): Promise<Outcome>;
  /** End the session process: `graceful` asks it to stop, `kill` ends it at once (DL-140). */
  stop(how: 'graceful' | 'kill'): Promise<Outcome>;
  /** Start an ended session again with the same conversation (DL-140). */
  resume(): Promise<Outcome>;
  /** `reply` carries the answers of a question (DL-068); `null` for a plain option. */
  answer(requestId: string, optionId: string, reply?: Reply | null): Promise<Outcome>;
  setMode(mode: string): Promise<Outcome>;
  setModel(model: string): Promise<Outcome>;
  /** The board title (DL-095); `null` clears it. Allowed on an ended session too. */
  rename(title: string | null): Promise<Outcome>;
  /** The restart auto-continuation preference (ADR-007); default off. */
  setAutoContinue(enabled: boolean): Promise<Outcome>;
  /** Roll back the conversation, then restore the files, to the start of the turn (PLAN §6.6). */
  revert(turnId: string): Promise<Outcome>;
  /** The owner's resolution of a revert in `reconciliation_required` (DL-097). */
  resolveRevert(revertId: string, resolution: RevertResolution): Promise<Outcome>;
  /** A generated summary of the session (R10.2, DL-167). The station stores it as the digest. */
  summarize(): Promise<Outcome>;
  /** What the station sees of the previous run of the session (DL-182, owner only). */
  checkExecution(): Promise<ExecutionCheck>;
  /** Mark the previous run (shim `shimId`) as ended; the station checks the processes again. */
  reconcileExecution(shimId: string): Promise<Outcome>;
  /** BG-4: the stop plan of the lost run `shimId` (owner only); the deck answers 501 for an old station. */
  stopPlan(shimId: string): Promise<ExecutionStopPlan>;
  /**
   * BG-4: stop the listed processes of the plan `planId`. The deck needs the owner's `elevation`;
   * the caller makes the command id (`commandId()`) to find the outcome in `last_stop`.
   */
  stopExecution(
    shimId: string,
    planId: string,
    elevation: string,
    commandId: string,
  ): Promise<Outcome>;
  /** What the coordinator session answered (ADR-008 §5.3, owner only). */
  fleetAnswers(): Promise<FleetAnswers>;
  /** The file service of the session; `null` without a reader (tests). */
  readonly files: FileActions | null;
  /** FG-12: the file service of the artifact folder of the session; `null` without a reader. */
  readonly artifacts?: FileActions | null;
  /** DL-189: a page of the earlier history of an imported session; `null` without a reader. */
  readonly history?: ((before: string | null) => Promise<SessionHistory>) | null;
  /** DL-191: the own messages and tool calls of one subagent (owner only); `null` without a reader. */
  readonly subagent?: ((toolUseId: string) => Promise<SubagentLog>) | null;
  /** B12: a page of the workflows of the session (owner and members); `null` without a reader. */
  readonly workflows?: ((after: WorkflowsCursor | null) => Promise<WorkflowsPage>) | null;
  /** B12: the own messages and tool calls of one workflow agent (owner only); `null` without a reader. */
  readonly workflowAgent?: ((agentId: string) => Promise<SubagentLog>) | null;
  /** DL-191: the other branches of the repository of the session (owner only); `null` without a reader. */
  readonly branches?: (() => Promise<RepoBranches>) | null;
  /** DL-191: the committed diff of one branch head (owner only); `null` without a reader. */
  readonly branchDiff?: ((head: string) => Promise<SessionDiff>) | null;
};

/** The longest title, in characters (the station refuses a longer one). */
export const MAX_TITLE_CHARS = 80;

const METHODS: Record<SessionCommand['op'], string> = {
  start: 'session.start',
  resume: 'session.resume',
  import: 'session.import',
  prompt: 'session.prompt',
  steer: 'session.steer',
  follow_up: 'session.follow_up',
  interrupt: 'session.interrupt',
  answer: 'approval.answer',
  set_mode: 'session.set_mode',
  set_model: 'session.set_model',
  fork: 'session.fork',
  stop: 'session.stop',
  revert: 'session.revert',
  rename: 'session.rename',
  set_auto_continue: 'session.set_auto_continue',
  resolve_revert: 'session.resolve_revert',
  coordinator_renew: 'session.coordinator_renew',
  coordinator_revoke: 'session.coordinator_revoke',
  coordinator_set: 'session.coordinator_set',
  worktree_trash: 'session.worktree_trash',
  worktree_restore: 'session.worktree_restore',
  worktree_purge: 'session.worktree_purge',
  worktree_release: 'session.worktree_release',
  reconcile_execution: 'session.reconcile_execution',
  stop_execution: 'session.stop_execution',
  shell_command: 'session.shell_command',
  release_shell: 'session.release_shell',
  land: 'session.land',
  move_out: 'session.move_out',
  move_in: 'session.move_in',
  move_finish: 'session.move_finish',
  move_undo: 'session.move_undo',
};

/** A command id: `b` and 24 random base-36 characters (`[0-9A-Za-z_-]{1,64}`). */
export function commandId(random: () => number = Math.random): string {
  let id = 'b';
  for (let i = 0; i < 24; i += 1) id += Math.floor(random() * 36).toString(36);
  return id;
}

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The text for a deck command response. */
export function outcomeOf(v: unknown): Outcome {
  if (!record(v) || typeof v['state'] !== 'string') {
    return { ok: false, text: 'The deck sent an invalid command response.' };
  }
  const cmd = v as DeckCommand;
  switch (cmd.state) {
    case 'done':
      if (cmd.receipt?.state === 'rejected') {
        return { ok: false, text: `Refused: ${cmd.receipt.reject?.message ?? 'no reason given'}` };
      }
      return { ok: true, text: 'Done.' };
    case 'pending':
      return { ok: true, text: 'Sent. The station has not confirmed it yet.' };
    case 'failed':
      return { ok: false, text: `Not delivered: ${cmd.reason ?? 'no reason given'}` };
    case 'ambiguous':
      return {
        ok: false,
        text: 'Delivery is unknown. The deck does not send it again; check the timeline.',
      };
    default:
      return { ok: false, text: 'The deck sent an invalid command response.' };
  }
}

/** BG-4 (design `board-gaps.md` §5.5): the six codes of a refused plan or a stop, with the board texts. */
export const STOP_TEXTS: Readonly<Record<string, string>> = {
  identity_unproven:
    'Fleet Deck cannot safely prove that these processes belong to the previous run. No processes were stopped.',
  inspection_incomplete:
    'Fleet Deck could not complete the safety check. No processes were stopped.',
  plan_changed: 'The process list changed. Review it again before stopping processes.',
  permission_denied: 'Fleet Deck could not stop one or more processes as the station user.',
  processes_remain: 'Some processes remain. Review the updated list.',
  outcome_unknown:
    'The stop result is unknown. Refresh the process check before taking another action.',
};

/** The text of a plan that the deck sent with a shape or a code that the board does not know. */
export const INVALID_STOP_PLAN = 'The deck sent an invalid stop plan.';

const isExecutionCheck = (v: unknown): v is ExecutionCheck =>
  record(v) &&
  typeof v['reason'] === 'string' &&
  typeof v['complete'] === 'boolean' &&
  Array.isArray(v['group_members']) &&
  Array.isArray(v['session_members']) &&
  Array.isArray(v['escaped']);

const isProcessList = (v: unknown): boolean =>
  Array.isArray(v) &&
  v.every((p) => record(p) && typeof p['pid'] === 'number' && typeof p['command'] === 'string');

const isStopPlan = (v: unknown): v is ExecutionStopPlan =>
  record(v) &&
  typeof v['plan_id'] === 'string' &&
  typeof v['expires_at'] === 'number' &&
  typeof v['eligible'] === 'boolean' &&
  (v['code'] === null || (typeof v['code'] === 'string' && Object.hasOwn(STOP_TEXTS, v['code']))) &&
  isProcessList(v['targets']) &&
  isProcessList(v['escaped']) &&
  isExecutionCheck(v['check']);

const invalidAnswer = (): Error => new Error('The deck sent an invalid answer.');

const isSubagentLog = (v: unknown): v is SubagentLog =>
  record(v) &&
  Array.isArray(v['items']) &&
  record(v['state']) &&
  typeof v['state']['kind'] === 'string';

const isWorkflowsPage = (v: unknown): v is WorkflowsPage =>
  record(v) && typeof v['as_of'] === 'number' && Array.isArray(v['workflows']);

const isRepoBranches = (v: unknown): v is RepoBranches =>
  record(v) && Array.isArray(v['branches']) && typeof v['truncated'] === 'boolean';

const isBranchDiff = (v: unknown): v is SessionDiff =>
  record(v) && ['summary', 'patch', 'truncated'].every((f) => f in v);

export function sessionActions(
  post: Post,
  key: SessionKey,
  scopes: readonly string[],
  newId: () => string = commandId,
  get: Get | null = null,
): SessionActions {
  const run = async (
    command: SessionCommand,
    elevation?: string,
    id: string = newId(),
  ): Promise<Outcome> => {
    try {
      const body = {
        station: key.station_id,
        command_id: id,
        command,
        ...(elevation === undefined ? {} : { elevation }),
      };
      return outcomeOf(await post(`/api/commands/${METHODS[command.op]}`, body));
    } catch (error) {
      // R11.7: in mob mode a member who does not drive is refused.
      if (error instanceof ApiError && error.code === 'not_driver') {
        return { ok: false, text: 'Ask to drive first.' };
      }
      return { ok: false, text: error instanceof Error ? error.message : 'The request failed.' };
    }
  };
  const session_id = key.session_id;
  const sessionPath = `/api/sessions/${encodeURIComponent(key.station_id)}/${encodeURIComponent(session_id)}`;
  return {
    canDrive: scopes.includes('drive'),
    canApprove: scopes.includes('approve'),
    canAdmin: scopes.includes('admin'),
    send: (kind, text, images = [], shellResults = []) =>
      run({
        op: kind,
        session_id,
        input: {
          text,
          ...(images.length > 0 ? { images: [...images] } : {}),
          ...(shellResults.length > 0 ? { shell_results: [...shellResults] } : {}),
        },
      }),
    shellCommand: (command) => run({ op: 'shell_command', session_id, command }),
    releaseShell: (effect_id) => run({ op: 'release_shell', session_id, effect_id }),
    interrupt: () => run({ op: 'interrupt', session_id }),
    stop: (how) => run({ op: 'stop', session_id, how }),
    resume: () => run({ op: 'resume', session_id }),
    answer: (request_id, option_id, reply = null) =>
      run({
        op: 'answer',
        session_id,
        request_id,
        decision: {
          option_id,
          updated_input: reply?.updated_input ?? null,
          text: reply?.text ?? null,
        },
      }),
    setMode: (mode) => run({ op: 'set_mode', session_id, mode }),
    setModel: (model) => run({ op: 'set_model', session_id, model }),
    rename: (title) => run({ op: 'rename', session_id, title }),
    setAutoContinue: (enabled) => run({ op: 'set_auto_continue', session_id, enabled }),
    revert: (turn_id) => run({ op: 'revert', session_id, turn_id }),
    resolveRevert: (revert_id, resolution) =>
      run({ op: 'resolve_revert', session_id, revert_id, resolution }),
    summarize: async () => {
      const path = `/api/stations/${encodeURIComponent(key.station_id)}/sessions/${encodeURIComponent(session_id)}/digest`;
      try {
        await post(path, {});
        return { ok: true, text: 'Summarized.' };
      } catch (error) {
        // A failed summary leaves the session as it was: the text says so, nothing else changes.
        const reason = error instanceof Error ? error.message : 'The request failed.';
        return { ok: false, text: `No summary was made: ${reason}` };
      }
    },
    checkExecution: async () => {
      if (get === null) throw new Error('No reader.');
      const path = `/api/sessions/${encodeURIComponent(key.station_id)}/${encodeURIComponent(session_id)}/execution`;
      const v = await get(path);
      if (!isExecutionCheck(v)) throw new Error('The deck sent an invalid execution check.');
      return v;
    },
    reconcileExecution: (shim_id) => run({ op: 'reconcile_execution', session_id, shim_id }),
    stopPlan: async (shimId) => {
      if (get === null) throw new Error('No reader.');
      const path = `${sessionPath}/execution/stop-plan?shim_id=${encodeURIComponent(shimId)}`;
      const v = await get(path);
      if (!isStopPlan(v)) throw new Error(INVALID_STOP_PLAN);
      return v;
    },
    stopExecution: (shim_id, plan_id, elevation, id) =>
      run({ op: 'stop_execution', session_id, shim_id, plan_id }, elevation, id),
    fleetAnswers: async () => {
      if (get === null) throw new Error('No reader.');
      const path = `/api/sessions/${encodeURIComponent(key.station_id)}/${encodeURIComponent(session_id)}/fleet-answers`;
      const v = await get(path);
      if (!isFleetAnswers(v)) throw new Error('The deck sent invalid fleet answers.');
      return v;
    },
    files: get ? fileActions(get, key) : null,
    artifacts: get ? artifactFiles(get, key) : null,
    history: get
      ? async (before) => {
          const base = `/api/sessions/${encodeURIComponent(key.station_id)}/${encodeURIComponent(session_id)}/history`;
          const v = await get(
            before === null ? base : `${base}?before=${encodeURIComponent(before)}`,
          );
          if (!isSessionHistory(v)) throw new Error('The deck sent an invalid history.');
          return v;
        }
      : null,
    subagent: get
      ? async (toolUseId) => {
          const v = await get(`${sessionPath}/subagents/${encodeURIComponent(toolUseId)}`);
          if (!isSubagentLog(v)) throw invalidAnswer();
          return v;
        }
      : null,
    workflows: get
      ? async (after) => {
          const query =
            after === null
              ? ''
              : `?after_order=${after.run_order}&after_index=${after.agent_index}`;
          const v = await get(`${sessionPath}/workflows${query}`);
          if (!isWorkflowsPage(v)) throw invalidAnswer();
          return v;
        }
      : null,
    workflowAgent: get
      ? async (agentId) => {
          const v = await get(`${sessionPath}/workflow-agents/${encodeURIComponent(agentId)}`);
          if (!isSubagentLog(v)) throw invalidAnswer();
          return v;
        }
      : null,
    branches: get
      ? async () => {
          const v = await get(`${sessionPath}/branches`);
          if (!isRepoBranches(v)) throw invalidAnswer();
          return v;
        }
      : null,
    branchDiff: get
      ? async (head) => {
          const v = await get(`${sessionPath}/branches/${encodeURIComponent(head)}/diff`);
          if (!isBranchDiff(v)) throw invalidAnswer();
          return v;
        }
      : null,
  };
}
