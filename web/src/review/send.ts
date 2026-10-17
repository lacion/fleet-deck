// DL-179: the logic of the review bar: which drafts a review sends, the keys that make a send
// safe to repeat, and the reviews whose delivery needs the caller.
import type { ReviewSummary, ReviewThread, SessionKey } from '../gen';
import { commandId } from '../session/commands';
import type { ReviewActions } from './api';

/** The most comments of one review (`MAX_COMMENTS_PER_REVIEW` of the deck). */
export const MAX_SEND = 50;

/** The most failed or unknown reviews that the bar lists. */
export const MAX_LISTED = 5;

/** The most "Send again" attempts of a review. */
export const MAX_ATTEMPTS = 5;

type Draft = { readonly id: string; readonly top: boolean; readonly at: number };

/** The drafts of the caller (the deck returns no other draft), oldest first inside each group. */
function drafts(threads: readonly ReviewThread[]): { tops: Draft[]; replies: Draft[] } {
  const tops: Draft[] = [];
  const replies: Draft[] = [];
  for (const t of threads) {
    t.comments.forEach((c, n) => {
      if (c.state !== 'draft') return;
      (n === 0 ? tops : replies).push({ id: c.comment_id, top: n === 0, at: c.created_at });
    });
  }
  const byAge = (a: Draft, b: Draft) => a.at - b.at;
  return { tops: tops.sort(byAge), replies: replies.sort(byAge) };
}

/** How many drafts the caller has. */
export function draftCount(threads: readonly ReviewThread[]): number {
  const d = drafts(threads);
  return d.tops.length + d.replies.length;
}

/** The comments that "Send review" sends: the top comments first, then the replies, at most 50. */
export function sendIds(threads: readonly ReviewThread[]): string[] {
  const d = drafts(threads);
  return [...d.tops, ...d.replies].slice(0, MAX_SEND).map((x) => x.id);
}

/** The drafts that "Discard all" deletes: the replies first, then the top comments. */
export function discardIds(threads: readonly ReviewThread[]): string[] {
  const d = drafts(threads);
  return [...d.replies, ...d.tops].map((x) => x.id);
}

export const draftsText = (n: number): string => `Review: ${n} ${n === 1 ? 'draft' : 'drafts'}`;

export const sendLabel = (n: number): string =>
  n > MAX_SEND ? `Send ${MAX_SEND} of ${n}` : 'Send review';

export const confirmText = (n: number): string => `Delete ${n} ${n === 1 ? 'draft' : 'drafts'}?`;

/**
 * A key bound to one selection of comments: a repeat of the same selection keeps its key (a send
 * is safe to repeat), another selection gets a new key, `rotate()` follows a send that returned.
 */
export type KeyHolder = { current(ids: string): string; rotate(): void };

export function keyHolder(make: () => string = commandId): KeyHolder {
  let key = make();
  // The selection that `key` is bound to; null until the first send after a new key.
  let bound: string | null = null;
  return {
    current: (ids) => {
      if (bound !== null && bound !== ids) key = make();
      bound = ids;
      return key;
    },
    rotate: () => {
      key = make();
      bound = null;
    },
  };
}

/** A key for each id; a key stays until `done(id)`. */
export type KeyMap = { get(id: string): string; done(id: string): void };

export function keyMap(make: () => string = commandId): KeyMap {
  const keys = new Map<string, string>();
  return {
    get(id) {
      let key = keys.get(id);
      if (key === undefined) {
        key = make();
        keys.set(id, key);
      }
      return key;
    },
    done: (id) => void keys.delete(id),
  };
}

/** Sends the drafts. The key changes after a send that succeeded, and for another selection. */
export async function sendDrafts(
  actions: Pick<ReviewActions, 'send'>,
  session: SessionKey,
  holder: KeyHolder,
  threads: readonly ReviewThread[],
): Promise<ReviewSummary> {
  const ids = sendIds(threads);
  const done = await actions.send(session, {
    send_key: holder.current([...ids].sort().join(',')),
    comment_ids: ids,
  });
  holder.rotate();
  return done;
}

/** Deletes every draft, replies first. A failure stops the loop. */
export async function discardDrafts(
  actions: Pick<ReviewActions, 'remove'>,
  session: SessionKey,
  threads: readonly ReviewThread[],
): Promise<void> {
  for (const id of discardIds(threads)) await actions.remove(session, id);
}

/** `owner`, or `member:<name>`: the `principal` of the reviews of the caller. */
export const principalOf = (self: string): string =>
  self === 'owner' ? 'owner' : `member:${self}`;

/** The reviews of the caller that were not delivered or whose delivery is unknown: newest first, at most 5. */
export function troubled(reviews: readonly ReviewSummary[], self: string): ReviewSummary[] {
  const me = principalOf(self);
  return reviews
    .filter((r) => r.principal === me && (r.delivery === 'failed' || r.delivery === 'unknown'))
    .sort((a, b) => b.sent_at - a.sent_at)
    .slice(0, MAX_LISTED);
}

/** The line about a review: "not delivered" with its reason, or "delivery unknown". */
export function troubleText(r: ReviewSummary): string {
  if (r.delivery === 'unknown') {
    return `Delivery unknown: the agent may have received review ${r.short_id}.`;
  }
  return r.reason === undefined || r.reason === ''
    ? `Review ${r.short_id} was not delivered.`
    : `Review ${r.short_id} was not delivered: ${r.reason}.`;
}

/** "Send again" is offered for a failed review with fewer than 5 attempts. */
export const canSendAgain = (r: ReviewSummary): boolean =>
  r.delivery === 'failed' && r.attempts < MAX_ATTEMPTS;
