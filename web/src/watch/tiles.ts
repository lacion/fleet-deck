// R6.2 (DL-169): the watch page, the first explored form of "watch many sessions" (DL-157).
// A wall of compact live tiles: the model of one tile and of the page. The wall display reuses
// the tiles, and the Activity stream folds its repeated rows with `foldLines`.
import { isLiveSession as isLive, repoName, repoNames } from '../board/fleet';
import { homeShortIn, plainLine } from '../ui/format';
import type { DeckSession, EventEnvelope, OpenRequest } from '../gen';
import { buildTimeline, type Item } from '../session/timeline';
import { questionForm } from '../session/question';
import { type StatusFilter, statusMatches } from '../views/StatusSeg';
import { statusWord, type TargetForm, toolText, toolWords } from './tooltext';

/** The most tiles; one timeline subscription each, and a socket holds 32 (the rest is spare). */
export const MAX_TILES = 24;
/** The lines of chat that a tile shows. */
export const TILE_LINES = 4;
/** How long Focus stays on one session before it moves to the next one that needs you. */
export const FOCUS_MS = 20_000;
/** The longest line, in characters. */
export const LINE_CHARS = 140;
/** The narrowest tile of the Watch wall: a tool line keeps its command and its file name. */
export const TILE_MIN_PX = 420;
/** The widest tile of a wall that fits in one row (the width of the Focus tile). */
export const TILE_MAX_PX = 720;
/** The gap between the tiles (views.css `.fd-wall`, `--sp-2`). */
export const TILE_GAP_PX = 8;

export const ALL = '';

export type WatchFilter = {
  /** A station id, or `ALL`. */
  readonly station: string;
  /** A repository name (`repoName`), or `ALL`. */
  readonly repo: string;
  /** The status segment of the header, the fleet's (`statusMatches`). */
  readonly status: StatusFilter;
};

export const emptyWatchFilter: WatchFilter = { station: ALL, repo: ALL, status: 'all' };

export type TileLine = {
  readonly key: string;
  readonly kind: 'message' | 'tool';
  /** The whole line as one text: the chat text, or the tool call ("Bash: cargo test"). */
  readonly text: string;
  /**
   * A tool call in its parts (DESIGN-SYSTEM §4.1, §4.5): the tool name for the owner (sans,
   * weight 600; an MCP tool without its server, `toolWords`), the target and a plain note after
   * them (the subagent title). A wall display line has none of them: its `text` is the tool name.
   */
  readonly name?: string;
  /** The whole tool name (`toolLabel`, with the MCP server) when `name` is shorter: the title. */
  readonly label?: string;
  readonly target?: string;
  /** How the target reads: a path (`PathTail`), a command (mono) or words; mono without it. */
  readonly form?: TargetForm;
  readonly note?: string;
  /** A call that failed or that the owner denied. */
  readonly outcome?: 'failed' | 'denied';
  /** The times the same line came in a row (`foldLines`); absent: once. */
  readonly repeat?: number;
  /** The node of a tool call: the call that runs now (`running_tools`) folds into it (`withoutCall`). */
  readonly node?: string;
};

/** A line that `foldLines` can fold. */
type Foldable = { readonly kind: string; readonly text: string; readonly repeat?: number };

const sameLine = (a: Foldable, b: Foldable): boolean => a.kind === b.kind && a.text === b.text;

/**
 * The lines without the call `node` when it is the last one: a tile shows the call that runs now
 * on a line of its own (with the working glyph), so the chat line of the same call folds into it
 * and one call never reads twice. A folded run keeps its other copies.
 */
export function withoutCall(lines: readonly TileLine[], node: string): readonly TileLine[] {
  const last = lines[lines.length - 1];
  if (last === undefined || last.node !== node) return lines;
  const rest = lines.slice(0, -1);
  const times = last.repeat ?? 1;
  return times > 1 ? [...rest, { ...last, repeat: times - 1 }] : rest;
}

/**
 * Consecutive equal lines (the same kind and text, or `same`) as one line with the number of
 * times: four "Bash cargo test" in a row are one line and a count, not four lines that look the
 * same. The line keeps the last copy of its run (the newest in a list that is oldest first). The
 * Watch and wall tiles and the Activity stream fold with it.
 */
export function foldLines<T extends Foldable>(
  lines: readonly T[],
  same: (a: T, b: T) => boolean = sameLine,
): T[] {
  const out: T[] = [];
  for (const line of lines) {
    const last = out[out.length - 1];
    if (last !== undefined && same(last, line)) {
      out[out.length - 1] = { ...line, repeat: (last.repeat ?? 1) + (line.repeat ?? 1) };
    } else {
      out.push(line);
    }
  }
  return out;
}

/** The first non-empty line, cut to `max` characters. Chat text loses its Markdown marks; a tool call keeps its text. */
const cut = (text: string, markdown = false, max = LINE_CHARS): string => {
  const line =
    text
      .split('\n')
      .map((l) => (markdown ? plainLine(l) : l.trim()))
      .find((l) => l !== '') ?? '';
  return line.length <= max ? line : `${line.slice(0, Math.max(max - 1, 0))}…`;
};

function lineOf(item: Item): TileLine | null {
  if (item.type === 'message') {
    const text = cut(item.text, true);
    return text === '' ? null : { key: item.key, kind: 'message', text };
  }
  if (item.type === 'tool') {
    // The owner's words: no MCP id, no JSON payload (`toolText`); a path keeps its file name. The
    // name has no MCP server (the line is short); the title keeps the whole name.
    const call = toolText(item.title, item.input, { kind: item.tool_kind, files: item.files });
    const name = cut(toolWords(item.title));
    // A path is never cut at its end: the end names the file (the tile drops leading folders).
    const target =
      call.form === 'path'
        ? call.target
        : cut(homeShortIn(call.target), false, LINE_CHARS - name.length - 1);
    const outcome = item.status === 'failed' || item.status === 'denied' ? item.status : null;
    // A call without a target says how it stands ("Workflow done"): a lone name read as a heading.
    const note = target === '' && outcome === null ? statusWord(item.status) : '';
    const text =
      target === '' ? [name, note].filter((p) => p !== '').join(' ') : `${name}: ${target}`;
    return {
      key: item.key,
      kind: 'tool',
      text: outcome === null ? text : `${text} (${outcome})`,
      name,
      ...(call.name === name ? {} : { label: call.name }),
      target,
      form: call.form,
      note,
      ...(outcome === null ? {} : { outcome }),
      node: item.node_id,
    };
  }
  if (item.type === 'subagent') {
    const name = 'Subagent';
    const note = cut(item.title, false, LINE_CHARS - name.length - 1);
    return { key: item.key, kind: 'tool', text: `${name}: ${note}`, name, target: '', note };
  }
  return null;
}

/** The last `max` agent text and tool lines of a timeline, oldest first; a line that came
 * several times in a row is one line with its count (`foldLines`). */
export function tileLines(
  events: readonly EventEnvelope[],
  max: number = TILE_LINES,
): readonly TileLine[] {
  const lines = buildTimeline(events)
    .items.map(lineOf)
    .filter((l): l is TileLine => l !== null);
  return foldLines(lines).slice(-max);
}

/**
 * The columns of the Watch wall for `n` tiles in a `width` px column: as many tiles of
 * `TILE_MIN_PX` as fit, then as few columns as keep the rows even (6 tiles where 4 fit are
 * 3 + 3, not 4 + 2; 4 where 3 fit are 2 + 2). Tiles that fit in one row keep at most
 * `TILE_MAX_PX`, and the rest of the row stays empty.
 */
export function watchColumns(n: number, width: number): number {
  const fit = Math.max(1, Math.floor((width + TILE_GAP_PX) / (TILE_MIN_PX + TILE_GAP_PX)));
  if (n <= fit) {
    const narrow = Math.ceil((width + TILE_GAP_PX) / (TILE_MAX_PX + TILE_GAP_PX));
    return Math.min(fit, Math.max(n, narrow, 1));
  }
  return Math.ceil(n / Math.ceil(n / fit));
}

/** The sessions that a tile can show: live ones that pass the filter, needs-you first. */
export function watchSessions(
  sessions: readonly DeckSession[],
  filter: WatchFilter,
): readonly DeckSession[] {
  const rank = (c: DeckSession) => (c.status === 'needs_you' ? 0 : 1);
  return sessions
    .filter(isLive)
    .filter((c) => filter.station === ALL || c.station_id === filter.station)
    .filter((c) => filter.repo === ALL || repoName(c) === filter.repo)
    .filter((c) => statusMatches(c, filter.status))
    .sort(
      (a, b) =>
        rank(a) - rank(b) ||
        b.summary.updated_at - a.summary.updated_at ||
        a.summary.session_id.localeCompare(b.summary.session_id),
    );
}

export const refOf = (card: DeckSession): string => `${card.station_id}/${card.summary.session_id}`;

/** The sessions that Focus cycles through: those that need you, the oldest request first. */
export function focusOrder(
  sessions: readonly DeckSession[],
  filter: WatchFilter,
): readonly string[] {
  return sessions
    .filter(isLive)
    .filter((c) => filter.station === ALL || c.station_id === filter.station)
    .filter((c) => filter.repo === ALL || repoName(c) === filter.repo)
    .filter((c) => c.status === 'needs_you')
    .sort(
      (a, b) =>
        (a.summary.open_requests[0]?.requested_at ?? 0) -
          (b.summary.open_requests[0]?.requested_at ?? 0) || refOf(a).localeCompare(refOf(b)),
    )
    .map(refOf);
}

/**
 * The session that Focus shows after `current`. With one candidate it stays on it; when
 * `current` is not a candidate any more it starts at the first one; with none it is null.
 */
export function nextFocus(order: readonly string[], current: string | null): string | null {
  if (order.length === 0) return null;
  const at = current === null ? -1 : order.indexOf(current);
  if (at < 0) return order[0] ?? null;
  return order[(at + 1) % order.length] ?? null;
}

/** The request that a tile answers in place, and whether its options are plain buttons. */
export function tileRequest(card: DeckSession): { request: OpenRequest; plain: boolean } | null {
  const request = card.summary.open_requests[0];
  if (request === undefined) return null;
  return { request, plain: questionForm(request) === null };
}

/** The station ids and repository names of the live sessions, for the filter menus (the sessions
 * outside a repository last, as everywhere). */
export function filterChoices(sessions: readonly DeckSession[]): {
  readonly stations: readonly string[];
  readonly repos: readonly string[];
} {
  const live = sessions.filter(isLive);
  return {
    stations: [...new Set(live.map((c) => c.station_id))].sort(),
    repos: repoNames(live),
  };
}
