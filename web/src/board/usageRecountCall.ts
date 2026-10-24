// B20 (design `usage-cost.md` §4.4): the typed call of the recount. `sessionActions(...).run` is
// private and its `Outcome` loses the delivery state and the reject code, so the recount has its
// own call: it sends `session.usage_recount`, reads the answer, and polls a pending command.
import { ApiError } from '../runtime/api';
import type { DeckCommand, SessionKey } from '../gen';
import { commandId } from '../session/commands';

export type RecountResult =
  | { readonly kind: 'accepted'; readonly commandId: string }
  | { readonly kind: 'refused'; readonly code: string; readonly message: string }
  | { readonly kind: 'not_supported' }
  | { readonly kind: 'pending'; readonly commandId: string }
  | { readonly kind: 'failed'; readonly text: string };

export type RecountCall = (key: SessionKey) => Promise<RecountResult>;

export type RecountOptions = {
  /** The command id of a call; default `commandId()`. */
  newId?: () => string;
  /** The wait between two polls of a pending command, in ms; default 1000. */
  pollMs?: number;
  /** The total wait for a pending command, in ms; default 30000. */
  limitMs?: number;
  sleep?: (ms: number) => Promise<void>;
};

const INVALID = 'The deck sent an invalid command response.';

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The reject code and text: `session_running: This session …` is the code and its text. */
function splitReject(text: string): { code: string; message: string } {
  const at = text.indexOf(': ');
  if (at > 0 && /^[a-z_]+$/.test(text.slice(0, at))) {
    return { code: text.slice(0, at), message: text.slice(at + 2) };
  }
  return { code: 'refused', message: text };
}

/** The result of an answer that is not `pending`; `null` for a pending one. */
function classify(v: unknown, id: string): RecountResult | null {
  if (!record(v) || typeof v['state'] !== 'string') return { kind: 'failed', text: INVALID };
  const cmd = v as DeckCommand;
  switch (cmd.state) {
    case 'done':
      if (cmd.receipt?.state === 'rejected') {
        return {
          kind: 'refused',
          ...splitReject(cmd.receipt.reject?.message ?? 'no reason given'),
        };
      }
      return { kind: 'accepted', commandId: id };
    case 'pending':
      return null;
    case 'failed':
      return { kind: 'failed', text: `Not delivered: ${cmd.reason ?? 'no reason given'}` };
    case 'ambiguous':
      return { kind: 'failed', text: 'Delivery is unknown.' };
    default:
      return { kind: 'failed', text: INVALID };
  }
}

const wait = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/** The recount call over one authenticated connection (owner only: the deck refuses any other). */
export function usageRecountCall(
  get: (path: string) => Promise<unknown>,
  post: (path: string, body: unknown) => Promise<unknown>,
  options: RecountOptions = {},
): RecountCall {
  const newId = options.newId ?? commandId;
  const pollMs = options.pollMs ?? 1000;
  const limitMs = options.limitMs ?? 30_000;
  const sleep = options.sleep ?? wait;
  return async (key) => {
    const id = newId();
    try {
      let answer = await post('/api/commands/session.usage_recount', {
        station: key.station_id,
        command_id: id,
        command: { op: 'usage_recount', session_id: key.session_id },
      });
      let waited = 0;
      for (;;) {
        const result = classify(answer, id);
        if (result !== null) return result;
        if (waited >= limitMs) return { kind: 'pending', commandId: id };
        await sleep(pollMs);
        waited += pollMs;
        answer = await get(`/api/commands/${encodeURIComponent(id)}`);
      }
    } catch (error) {
      if (error instanceof ApiError && error.code === 'not_supported') {
        return { kind: 'not_supported' };
      }
      return {
        kind: 'failed',
        text: error instanceof Error ? error.message : 'The request failed.',
      };
    }
  };
}
