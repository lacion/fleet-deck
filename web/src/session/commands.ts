// M2 P4-3: session commands from the board (`POST /api/commands/<method>`).
//
// Each user action gets one fresh command id. The board never resends a
// command by itself: the deck owns retries, and an `ambiguous` command is
// shown as such (A9, PLAN §8). The station answers a refused command with a
// receipt; its message is shown as is.
import type { DeckCommand, SessionCommand, SessionKey } from '../gen';
import type { Reply } from './question';

type Post = (path: string, body: unknown) => Promise<unknown>;

export type Outcome = { readonly ok: boolean; readonly text: string };
export type InputKind = 'prompt' | 'steer' | 'follow_up';

export type SessionActions = {
  readonly canDrive: boolean;
  readonly canApprove: boolean;
  send(kind: InputKind, text: string): Promise<Outcome>;
  interrupt(): Promise<Outcome>;
  /** `reply` carries the answers of a question (DL-068); `null` for a plain option. */
  answer(requestId: string, optionId: string, reply?: Reply | null): Promise<Outcome>;
  setMode(mode: string): Promise<Outcome>;
  setModel(model: string): Promise<Outcome>;
};

const METHODS: Record<SessionCommand['op'], string> = {
  start: 'session.start',
  resume: 'session.resume',
  prompt: 'session.prompt',
  steer: 'session.steer',
  follow_up: 'session.follow_up',
  interrupt: 'session.interrupt',
  answer: 'approval.answer',
  set_mode: 'session.set_mode',
  set_model: 'session.set_model',
  fork: 'session.fork',
  stop: 'session.stop',
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
    send: (kind, text) => run({ op: kind, session_id, input: { text } }),
    interrupt: () => run({ op: 'interrupt', session_id }),
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
  };
}
