// R1.4 (DL-170): compare the winner of an `Nx` batch. The siblings come from
// `summary.batch`; "Keep this one" stops the other sessions and moves their
// worktrees to the trash. The choice stays with the owner, never a model.
import type { BatchRef, DeckSession, EventEnvelope, SessionKey, SessionSummary } from '../gen';
import { buildTimeline } from '../session/timeline';
import type { Outcome } from '../session/commands';
import { ApiError } from '../runtime/api';
import { localSet } from '../ui/localSet';

type Post = (path: string, body: unknown) => Promise<unknown>;

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The fewest sessions that make a batch worth a comparison. */
export const MIN_SIBLINGS = 2;

/** One ULID (Crockford base32): 10 characters of time and 16 of randomness. */
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/** A new batch id for one press of "Start N sessions" (a ULID, made on the board). */
export function newBatchId(now: number = Date.now(), random: () => number = Math.random): string {
  let time = '';
  let rest = Math.max(0, Math.floor(now));
  for (let i = 0; i < 10; i += 1) {
    time = CROCKFORD.charAt(rest % 32) + time;
    rest = Math.floor(rest / 32);
  }
  let tail = '';
  for (let i = 0; i < 16; i += 1) tail += CROCKFORD.charAt(Math.floor(random() * 32));
  return time + tail;
}

/** The batch reference of the `index`th (from 1) of `size` sessions. */
export const batchRef = (batchId: string, index: number, size: number): BatchRef => ({
  batch_id: batchId,
  index,
  size,
});

/** The sessions of one batch, in the order of their batch index (then by session id). */
export function siblings(sessions: readonly DeckSession[], batchId: string): DeckSession[] {
  return sessions
    .filter((s) => s.summary.batch?.batch_id === batchId)
    .sort(
      (a, b) =>
        (a.summary.batch?.index ?? 0) - (b.summary.batch?.index ?? 0) ||
        a.summary.session_id.localeCompare(b.summary.session_id),
    );
}

/** How many sessions each batch has on the board; only a batch with two or more is listed. */
export function batchCounts(sessions: readonly DeckSession[]): ReadonlyMap<string, number> {
  const counts = new Map<string, number>();
  for (const s of sessions) {
    const id = s.summary.batch?.batch_id;
    if (id !== undefined) counts.set(id, (counts.get(id) ?? 0) + 1);
  }
  for (const [id, n] of counts) if (n < MIN_SIBLINGS) counts.delete(id);
  return counts;
}

/** The size of the batch of a card on the board; 0 outside a batch or for a batch of one. */
export const batchCountOf = (card: DeckSession, counts: ReadonlyMap<string, number>): number =>
  counts.get(card.summary.batch?.batch_id ?? '') ?? 0;

/** The last agent message of a session timeline (the newest message item of the top level), or `null`. */
export function lastAgentMessage(events: readonly EventEnvelope[]): string | null {
  const items = buildTimeline(events).items;
  for (let i = items.length - 1; i >= 0; i -= 1) {
    const item = items[i];
    if (item?.type === 'message' && item.text.trim() !== '') return item.text;
  }
  return null;
}

/**
 * The digest line of a summary (R10.2), or `null`. Another worker adds the field:
 * it is read defensively, as a string or as an object with a `text`.
 */
export function digestOf(summary: SessionSummary): string | null {
  const digest = (summary as unknown as Record<string, unknown>)['digest'];
  if (typeof digest === 'string') return digest.trim() === '' ? null : digest;
  if (record(digest) && typeof digest['text'] === 'string' && digest['text'].trim() !== '') {
    return digest['text'];
  }
  return null;
}

/** The changed file paths of a unified diff, in order, each once. */
export function patchFiles(patch: string): string[] {
  const paths: string[] = [];
  for (const line of patch.split('\n')) {
    const m = /^diff --git a\/.* b\/(.+)$/.exec(line);
    const path = m?.[1];
    if (path !== undefined && !paths.includes(path)) paths.push(path);
  }
  return paths;
}

/** The sessions that the owner kept from a batch, by `<station>/<session>` (a local note, no backend). */
export const kept = localSet('fd-batch-kept');

const ENDED = ['exited', 'lost'] as const;
export const isEnded = (s: SessionSummary): boolean => ENDED.some((state) => state === s.state);

/** What "Keep this one" did for one other session. */
export type KeepStep = 'waiting' | 'stopping' | 'trashing' | 'done' | 'failed';
export type KeepProgress = {
  readonly key: SessionKey;
  readonly step: KeepStep;
  readonly text: string;
};

export type KeepDeps = {
  /** The newest summary of the session; `null` when it left the board. */
  summary(key: SessionKey): SessionSummary | null;
  /** The graceful stop command. */
  stop(key: SessionKey): Promise<Outcome>;
  /** The worktree trash command. */
  trash(key: SessionKey): Promise<Outcome>;
  sleep(ms: number): Promise<void>;
};

/** How often the board looks for the end of a stopping session, and how long it waits. */
export const KEEP_POLL_MS = 500;
export const KEEP_WAIT_MS = 30_000;

async function keepOther(
  key: SessionKey,
  deps: KeepDeps,
  report: (p: KeepProgress) => void,
): Promise<void> {
  const say = (step: KeepStep, text: string) => report({ key, step, text });
  const first = deps.summary(key);
  if (first === null) return say('failed', 'The session is not on the board.');
  if (!isEnded(first)) {
    if (first.state !== 'stopping') {
      say('stopping', 'Stopping…');
      const stopped = await deps.stop(key);
      if (!stopped.ok) return say('failed', stopped.text);
    }
    say('waiting', 'Waiting for the session to end…');
    let waited = 0;
    for (;;) {
      const now = deps.summary(key);
      if (now !== null && isEnded(now)) break;
      if (waited >= KEEP_WAIT_MS) return say('failed', 'The session did not end in 30 seconds.');
      await deps.sleep(KEEP_POLL_MS);
      waited += KEEP_POLL_MS;
    }
  }
  const ended = deps.summary(key);
  if (ended === null || ended.worktree === null) {
    return say('done', 'Stopped. It has no worktree of its own to move.');
  }
  say('trashing', 'Moving the worktree to the trash…');
  const trashed = await deps.trash(key);
  if (!trashed.ok) return say('failed', `Stopped, but the worktree stayed: ${trashed.text}`);
  say('done', 'Stopped. The worktree is in the trash; restore it from Stations.');
}

/**
 * "Keep this one" (R1.4): each other session is stopped when it is live, then its worktree moves to the
 * trash (DL-150). The sessions run side by side; one failure does not stop the others. Nothing is deleted.
 */
export async function keepOne(
  others: readonly SessionKey[],
  deps: KeepDeps,
  report: (p: KeepProgress) => void,
): Promise<void> {
  await Promise.all(
    others.map((key) =>
      keepOther(key, deps, report).catch((e: unknown) =>
        report({
          key,
          step: 'failed',
          text: e instanceof Error ? e.message : 'The request failed.',
        }),
      ),
    ),
  );
}

/** The confirm text of "Keep this one". */
export const keepConfirm = (title: string, others: number): string =>
  `Keep ${title}? The other ${others} ${others === 1 ? 'session is' : 'sessions are'} stopped and ${
    others === 1 ? 'its worktree moves' : 'their worktrees move'
  } to the trash; nothing is deleted.`;

/** The answer of the race summary route (R10.5). */
export type RaceSummary =
  | { readonly kind: 'text'; readonly text: string }
  | { readonly kind: 'absent' }
  | { readonly kind: 'error'; readonly text: string };

/**
 * `POST /api/assist/race` (R10.5, another worker). A 404 means the deck has no such route: the page hides
 * the slot. The body names the sibling sessions; the deck builds the model input from its own data.
 */
export async function raceSummary(post: Post, keys: readonly SessionKey[]): Promise<RaceSummary> {
  try {
    const answer = await post('/api/assist/race', {
      sessions: keys.map((k) => ({ station_id: k.station_id, session_id: k.session_id })),
    });
    if (record(answer) && typeof answer['text'] === 'string') {
      return { kind: 'text', text: answer['text'] };
    }
    return { kind: 'error', text: 'The deck sent an answer that the board cannot read.' };
  } catch (e) {
    // A deck without the route answers 404, with a JSON body or without one.
    if ((e instanceof ApiError && e.status === 404) || e instanceof SyntaxError) {
      return { kind: 'absent' };
    }
    return { kind: 'error', text: e instanceof Error ? e.message : 'The request failed.' };
  }
}
