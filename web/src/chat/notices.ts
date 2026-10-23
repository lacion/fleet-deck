// The words of the station notices and request outcomes for the owner (DESIGN-SYSTEM §5): never a
// raw error code, an input id, a reason id or a model id in the transcript. The raw value stays in
// the `title` of the notice. The timeline (session/timeline.ts) builds its notices with these, so
// that the chat and the wall say the same.
import type { Item } from '../session/timeline';
import type { Event } from '../gen';
import { contextLabel, modeLabel, shortModel } from '../ui/format';

/** A notice as the chat shows it: its words, and the raw text for its `title`. */
export type NoticeText = { readonly text: string; readonly title?: string };

/** The fields of a notice item that the chat reads besides its text (the raw text, the repeats). */
export type NoticeExtra = { readonly title?: string; readonly repeat?: number };

/** The notice item of the timeline with its extra fields. */
export type NoticeItem = Extract<Item, { type: 'notice' }> & NoticeExtra;

const capital = (text: string): string => text.charAt(0).toUpperCase() + text.slice(1);
const sentence = (text: string): string => (/[.!?…]$/.test(text) ? text : `${text}.`);

/** An input id (a ULID): the owner never needs it in a sentence. */
const ULID = / ?\b[0-9A-HJKMNP-TV-Z]{26}\b/g;

/** Why the station lost track of an input (`delivery_unknown`), in words. */
const DELIVERY_CAUSE: Readonly<Record<string, string>> = {
  process_lost: 'the agent process stopped',
  station_restart: 'the station restarted',
};

/** `delivery_unknown`: an input (a message) or a command may have reached the agent, or not. */
function deliveryText(message: string): string {
  const reason = /\(([a-z_]+)\)/.exec(message)?.[1];
  const cause = reason === undefined ? undefined : DELIVERY_CAUSE[reason];
  const input = /\binput\b/.test(message);
  const what = input ? 'A message' : 'A command';
  const again = input ? ' The station does not send it again.' : '';
  return `${what} may not have reached the agent${cause === undefined ? '' : `: ${cause}`}.${again}`;
}

/**
 * An `error` event as a notice: `delivery_unknown` says what may have happened to the message;
 * any other code shows its message as a sentence, without the code and the input ids.
 */
export function errorNotice(code: string, message: string): NoticeText {
  const title = message === '' ? code : `${code}: ${message}`;
  if (code === 'delivery_unknown') return { text: deliveryText(message), title };
  const words = message.replace(ULID, '').trim();
  return { text: sentence(capital(words === '' ? code.replaceAll('_', ' ') : words)), title };
}

/** Why a request can no longer be answered (`ExpiredReason`), in words. */
const EXPIRED: Readonly<Record<string, string>> = {
  process_lost: 'expired: the agent process stopped',
  turn_ended: 'expired: the turn ended',
  not_resumable: 'expired: the session cannot resume',
  cancelled_by_provider: 'withdrawn by the agent',
};

/** The outcome of an expired request ("expired: the turn ended"); an unknown reason says "expired". */
export const expiredText = (reason: string): string => EXPIRED[reason] ?? 'expired';

/** Who answered a request (`ResolvedBy`), in words. The board is the owner: it says nothing. */
const RESOLVED_BY: Readonly<Record<string, string>> = {
  board: '',
  policy: 'by a rule',
  timeout: 'after the time limit',
  coordinator: 'by the coordinator',
  local: 'in the terminal',
};

/** The words of who answered a request; '' for the board (and an unknown resolver). */
export const resolvedByText = (by: string): string => RESOLVED_BY[by] ?? '';

/** The outcome of an answered request: the chosen option, and who chose it when it was not you. */
export function answeredText(label: string, by: string): string {
  const who = resolvedByText(by);
  return who === '' ? label : `${label}, ${who}`;
}

/** A model change: the family and version, and the context window as an aside (§5 Models). */
export function modelNotice(model: string): NoticeText {
  const context = contextLabel(model);
  const text = `Model changed to ${shortModel(model)}${context === null ? '' : ` (${context})`}`;
  return { text, title: model };
}

/** A reasoning effort change (B18); the model change notice keeps the model alone. */
export const effortNotice = (effort: string): NoticeText => ({
  text: `Effort changed to ${effort}`,
  title: `Reasoning effort: ${effort}`,
});

/**
 * The notice of an `effort.set_result` (B18 Phase 2). `accepted`, `applied` and `superseded` add
 * none: an `effort.changed` notice shows a real change. `adjusted` and `unverified` are `info`; a
 * refusal is an `error`, as a refused interrupt.
 */
export function effortSetNotice(
  e: Extract<Event, { kind: 'effort.set_result' }>,
): (NoticeText & { readonly tone: 'info' | 'error' }) | null {
  switch (e.outcome) {
    case 'adjusted':
      return {
        text:
          e.effort === null
            ? 'This model does not use an effort setting'
            : `Effort is ${e.effort}, not ${e.requested}`,
        tone: 'info',
      };
    case 'unverified':
      return {
        text: `Effort set to ${e.requested}; the provider does not report it`,
        tone: 'info',
      };
    case 'refused':
      return {
        text: e.message ? `Effort not changed: ${e.message}` : 'Effort not changed.',
        tone: 'error',
      };
    default:
      return null;
  }
}

/** A permission mode change, with the one name of the mode (§5 Modes). */
export const modeNotice = (mode: string): NoticeText => ({
  text: `Mode: ${modeLabel(mode)}`,
  title: mode,
});

/** Why the station gave a session back to the board after a terminal held it, in words. */
const TERMINAL_RETURN: readonly (readonly [RegExp, string])[] = [
  [/^(?:the terminal process exited|tui exited)\b/, 'the terminal closed'],
  [/^the station restarted\b/, 'the station restarted'],
  [/^the terminal did not start\b/, 'the terminal did not start'],
  [/^the owner released the terminal hold\b/, 'the terminal was released'],
];

/** The notice of a session that a terminal held and gave back; a reason without words is left out. */
export function terminalReturnNotice(reason: string | null | undefined): NoticeText {
  const raw = reason?.trim() ?? '';
  const words = TERMINAL_RETURN.find(([pattern]) => pattern.test(raw))?.[1];
  const text = `Returned to board control${words === undefined ? '' : `: ${words}`}.`;
  return raw === '' ? { text } : { text, title: raw };
}

/** The line of a turn that did not end well (§4.7 notice for a failure, a quiet line otherwise). */
export const TURN_TEXT: Readonly<Record<'failed' | 'interrupted', string>> = {
  failed: 'This turn failed.',
  interrupted: 'The turn was interrupted.',
};

/** The agent text of a turn that the provider API refused (Claude: "API Error: …"). */
const API_ERROR = /^\s*API Error:/;

/** What stopped a turn, from the raw text of the provider error, in words; '' when not known. */
const API_CAUSE: readonly (readonly [RegExp, string])[] = [
  [/\b429\b|rate[_ ]limit/i, "The provider's rate limit stopped it. Try again later."],
  [/\b529\b|overloaded/i, 'The provider is overloaded. Try again later.'],
  [/\b40[13]\b|authenticat|credential|api key|oauth/i, 'The provider refused the sign-in.'],
  [/prompt is too long|context (?:window|length)/i, 'The conversation is too long for the model.'],
  [/\b5\d\d\b/, 'The provider had an error. Try again later.'],
];

export function apiCause(raw: string): string {
  return API_CAUSE.find(([pattern]) => pattern.test(raw))?.[1] ?? '';
}

/** The text of an agent message for a one-line view (a tile, a stream row): a provider error
 * ("API Error: …") becomes its cause in words; any other text stays. */
export function agentLine(text: string): string {
  if (!API_ERROR.test(text)) return text;
  const cause = apiCause(text);
  return cause === '' ? 'The provider refused the turn.' : cause;
}

export type TurnErrors = {
  /** The keys of the agent texts that a failed turn shows as its cause. */
  readonly hidden: ReadonlySet<string>;
  /** The raw provider error of each failed turn, by the key of its turn item. */
  readonly detail: ReadonlyMap<string, string>;
};

/**
 * Pairs each failed turn of `items` (one list of the timeline) with its last agent text when that
 * text is a provider error ("API Error: …"): the turn row shows it as its cause and its details,
 * and the text is not shown again as agent prose.
 */
export function pairTurnErrors(items: readonly Item[]): TurnErrors {
  const hidden = new Set<string>();
  const detail = new Map<string, string>();
  let last: Extract<Item, { type: 'message' }> | null = null;
  for (const item of items) {
    if (item.type === 'run') last = null;
    else if (item.type === 'message') last = item;
    else if (item.type === 'turn') {
      if (item.status === 'failed' && last?.done && API_ERROR.test(last.text)) {
        hidden.add(last.key);
        detail.set(item.key, last.text.trim());
      }
      last = null;
    }
  }
  return { hidden, detail };
}
