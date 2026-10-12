// M2 P4-3: session commands from the board (`POST /api/commands/<method>`).
//
// Each user action gets one fresh command id. The board never resends a
// command by itself: the deck owns retries, and an `ambiguous` command is
// shown as such (A9, PLAN §8). The station answers a refused command with a
// receipt; its message is shown as is.
import type { DeckCommand, ImageInput, RevertResolution, SessionCommand, SessionKey } from '../gen';
import type { Reply } from './question';
import { type FileActions, fileActions } from './workspace';

type Post = (path: string, body: unknown) => Promise<unknown>;
type Get = (path: string) => Promise<unknown>;

export type Outcome = { readonly ok: boolean; readonly text: string };
export type InputKind = 'prompt' | 'steer' | 'follow_up';

export type SessionActions = {
  readonly canDrive: boolean;
  readonly canApprove: boolean;
  /** Pasted images go with the text (M3 6-10, DL-304). */
  send(kind: InputKind, text: string, images?: readonly ImageInput[]): Promise<Outcome>;
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
  /** The file service of the session; `null` without a reader (tests). */
  readonly files: FileActions | null;
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

export function sessionActions(
  post: Post,
  key: SessionKey,
  scopes: readonly string[],
  newId: () => string = commandId,
  get: Get | null = null,
): SessionActions {
  const run = async (command: SessionCommand): Promise<Outcome> => {
    try {
      const body = { station: key.station_id, command_id: newId(), command };
      return outcomeOf(await post(`/api/commands/${METHODS[command.op]}`, body));
    } catch (error) {
      return { ok: false, text: error instanceof Error ? error.message : 'The request failed.' };
    }
  };
  const session_id = key.session_id;
  return {
    canDrive: scopes.includes('drive'),
    canApprove: scopes.includes('approve'),
    send: (kind, text, images = []) =>
      run({
        op: kind,
        session_id,
        input: images.length > 0 ? { text, images: [...images] } : { text },
      }),
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
    files: get ? fileActions(get, key) : null,
  };
}
