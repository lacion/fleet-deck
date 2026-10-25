// M3 6-8: the event feed (v0 `Feed.tsx` parity, DL-095). The board derives
// feed items from the session-summary changes of the deck stream: the deck
// already sends every changed card, so the feed needs no new wire contract.
// The feed shows what changed while this page was open; it is not a
// history (the session timeline is the history).
import type { DeckSession, SessionKey } from '../gen';

export type FeedKind = 'join' | 'ask' | 'turn' | 'tool' | 'end' | 'rename' | 'revert' | 'error';

export type FeedItem = {
  /** Unique in the feed: the session key, the session sequence and the kind. */
  readonly id: string;
  readonly kind: FeedKind;
  readonly key: SessionKey;
  /** The card title at the time of the item. */
  readonly name: string;
  readonly text: string;
  /** Milliseconds since the epoch: `updated_at` of the summary. */
  readonly at: number;
};

/** The most items that the feed keeps (the oldest go first). */
export const FEED_MAX = 200;

/** The filter chips, in display order (`all` shows every kind). */
export const FEED_FILTERS: readonly { readonly id: 'all' | FeedKind; readonly label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'ask', label: 'Asks' },
  { id: 'turn', label: 'Turns' },
  { id: 'tool', label: 'Tools' },
  { id: 'join', label: 'Joins' },
  { id: 'end', label: 'Ends' },
  { id: 'revert', label: 'Reverts' },
  { id: 'error', label: 'Errors' },
  { id: 'rename', label: 'Renames' },
];

const nameOf = (s: DeckSession) => s.summary.title ?? s.summary.session_id;

/**
 * The feed items for one card change. `before` is `undefined` for a card
 * that is new to the board. An unchanged card gives no item.
 */
export function feedItems(before: DeckSession | undefined, after: DeckSession): FeedItem[] {
  const a = after.summary;
  const key = { station_id: after.station_id, session_id: a.session_id };
  const items: FeedItem[] = [];
  const add = (kind: FeedKind, text: string) =>
    items.push({
      id: `${after.station_id}/${a.session_id}/${a.session_seq}/${kind}/${items.length}`,
      kind,
      key,
      name: nameOf(after),
      text,
      at: a.updated_at,
    });
  if (before === undefined) {
    add('join', a.cwd ? `joined in ${a.cwd}` : 'joined');
    for (const r of a.open_requests) add('ask', `asks: ${r.title}`);
    return items;
  }
  const b = before.summary;
  if (a.session_seq <= b.session_seq && after.status === before.status) return items;
  if (a.title !== b.title) {
    add('rename', a.title ? `renamed from ${nameOf(before)}` : `title cleared (was ${b.title})`);
  }
  const asked = new Set(b.open_requests.map((r) => r.request_id));
  for (const r of a.open_requests) if (!asked.has(r.request_id)) add('ask', `asks: ${r.title}`);
  const running = new Set(b.running_tools.map((t) => t.node_id));
  for (const t of a.running_tools) {
    if (!running.has(t.node_id)) add('tool', `${t.title}: ${t.input_summary}`);
  }
  if (b.current_turn !== null && a.current_turn !== b.current_turn) add('turn', 'turn ended');
  if (a.revert !== null && a.revert.state !== b.revert?.state) {
    const reason = a.revert.reason ? ` (${a.revert.reason})` : '';
    add('revert', `revert ${a.revert.state.replaceAll('_', ' ')}${reason}`);
  }
  if (a.last_error !== null && a.last_error.code !== b.last_error?.code) {
    add('error', `${a.last_error.code}: ${a.last_error.message}`);
  }
  const ended = (s: DeckSession['summary']) => s.state === 'exited' || s.state === 'lost';
  if (ended(a) && !ended(b)) add('end', a.state === 'lost' ? 'lost' : 'ended');
  return items;
}

/** Appends `items` to `feed` and keeps the newest {@link FEED_MAX}. */
export function appendFeed(feed: readonly FeedItem[], items: readonly FeedItem[]): FeedItem[] {
  if (items.length === 0) return [...feed];
  return [...feed, ...items].slice(-FEED_MAX);
}

/** `hh:mm:ss` in local time. */
export function clock(at: number): string {
  const d = new Date(at);
  const two = (n: number) => String(n).padStart(2, '0');
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
}
