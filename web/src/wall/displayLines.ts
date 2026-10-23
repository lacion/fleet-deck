// R13 P2c (design §5D item 7): the lines that a wall display draws from its own timeline events.
// A display event holds no input, output, author or file list, so these functions only join the
// fixed fields: the text of the run start, the assistant message, the tool name and status, and
// the request state.
import type { DisplayEvent, DisplayItem, RequestKind } from '../gen';
import { agentLine } from '../chat/notices';
import { plainLine } from '../ui/format';
import { foldLines, TILE_LINES, type TileLine } from '../watch/tiles';

/** The most items that the chat pane draws (the same as `CHAT_ITEMS`). */
const CHAT_MAX = 120;
/** The longest tile line, in characters (the same as `LINE_CHARS`). */
const LINE_MAX = 140;

export type DisplayLine = {
  readonly key: string;
  readonly kind: 'prompt' | 'message' | 'tool' | 'ask';
  readonly text: string;
  /** The node of a tool call (the call that runs now folds into its tile line). */
  readonly node?: string;
};

/** The fixed label of a request kind (the same four texts as the deck's `request_label`). */
export const REQUEST_LABEL: Readonly<Record<RequestKind, string>> = {
  command: 'Approve a command',
  file_change: 'Approve a file change',
  tool: 'Approve a tool use',
  question: 'Answer a question',
};

const requestName = (item: Extract<DisplayItem, { item: 'request' }>): string =>
  item.tool_name !== null && item.tool_name !== '' ? item.tool_name : REQUEST_LABEL[item.kind];

function lineOf(event: DisplayEvent, withEnds: boolean): DisplayLine | null {
  const { item } = event;
  const key = `${event.station_seq}`;
  switch (item.item) {
    case 'run_started':
      return { key, kind: 'prompt', text: item.summary };
    case 'message':
      return item.text.trim() === '' ? null : { key, kind: 'message', text: item.text };
    case 'tool': {
      const mark = item.status === 'failed' || item.status === 'denied' ? ` (${item.status})` : '';
      return { key, kind: 'tool', text: `${item.name}${mark}`, node: item.node_id };
    }
    case 'request': {
      const name = requestName(item);
      if (item.state === 'opened') {
        return { key, kind: 'ask', text: `Waiting for an answer · ${name}` };
      }
      return {
        key,
        kind: 'tool',
        text: `${name} · ${item.state === 'resolved' ? 'answered' : 'expired'}`,
      };
    }
    case 'run_ended':
      return withEnds ? { key, kind: 'tool', text: `Run ended · ${item.status}` } : null;
    default:
      return null;
  }
}

/**
 * The lines of the events, oldest first. One tool call is one line: a later event of the same call
 * (its end, with its status) replaces the line of its start in place, so a call never reads twice.
 */
function linesOf(events: readonly DisplayEvent[], withEnds: boolean): DisplayLine[] {
  const out: DisplayLine[] = [];
  const calls = new Map<string, number>();
  for (const e of events) {
    const line = lineOf(e, withEnds);
    if (line === null) continue;
    const node = e.item.item === 'tool' ? e.item.node_id : null;
    const at = node === null ? undefined : calls.get(node);
    const first = at === undefined ? undefined : out[at];
    if (at !== undefined && first !== undefined) {
      out[at] = { ...line, key: first.key };
      continue;
    }
    if (node !== null) calls.set(node, out.length);
    out.push(line);
  }
  return out;
}

/** The lines of the chat pane, oldest first, at most `max`. */
export function displayChatLines(
  events: readonly DisplayEvent[],
  max: number = CHAT_MAX,
): DisplayLine[] {
  return linesOf(events, true).slice(-max);
}

const cut = (text: string): string => {
  const line =
    text
      .split('\n')
      .map((l) => plainLine(l))
      .find((l) => l !== '') ?? '';
  return line.length <= LINE_MAX ? line : `${line.slice(0, LINE_MAX - 1)}…`;
};

/**
 * The last agent text and tool lines of a tile, oldest first (the form of `tileLines`). A line that
 * is empty without its Markdown marks is no line: a tile never draws a blank row. Lines that came
 * several times in a row ("Bash", "Bash") are one line with its count (`foldLines`).
 */
export function displayTileLines(
  events: readonly DisplayEvent[],
  max: number = TILE_LINES,
): readonly TileLine[] {
  const out: TileLine[] = [];
  for (const line of linesOf(events, false)) {
    if (line.kind !== 'message' && line.kind !== 'tool') continue;
    // A provider error reads as its cause; the provider's own words stay in the title.
    const said = line.kind === 'message' ? agentLine(line.text) : line.text;
    const text = cut(said);
    if (text === '') continue;
    out.push({
      key: line.key,
      kind: line.kind,
      text,
      ...(said === line.text ? {} : { label: line.text.trim() }),
      ...(line.node === undefined ? {} : { node: line.node }),
    });
  }
  return foldLines(out).slice(-max);
}

/** The last assistant message of a display timeline, or `null` (the race column). */
export function lastDisplayMessage(events: readonly DisplayEvent[]): string | null {
  for (let i = events.length - 1; i >= 0; i -= 1) {
    const item = events[i]?.item;
    if (item?.item === 'message' && item.text.trim() !== '') return item.text;
  }
  return null;
}
