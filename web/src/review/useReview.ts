// DL-179: the review state of one open session: the threads, the reviews and the revision that
// the deck answered. It loads on mount, when `review_rev` of the session card changes, after a
// reconnect and on `reload()`.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReviewComment, ReviewThreads, SessionKey } from '../gen';
import { useReviewActions } from './context';

export type ReviewState = {
  readonly data: ReviewThreads | null;
  readonly error: string | null;
  reload(): void;
  /** Every comment that the caller may see, by short id. */
  readonly byShort: ReadonlyMap<string, ReviewComment>;
  /** The short ids that the caller may see (the chips of the chat use them). */
  readonly known: ReadonlySet<string>;
  /** Older reviews than the loaded ones exist. */
  readonly moreReviews: boolean;
  loadOlder(): void;
};

/** Drops an answer of another session or of an older call. */
export type Gate = {
  begin(key: string): number;
  fresh(call: number, key: string): boolean;
};

export function createGate(): Gate {
  let counter = 0;
  let current = '';
  return {
    begin(key) {
      counter += 1;
      current = key;
      return counter;
    },
    fresh: (call, key) => call === counter && key === current,
  };
}

/** What the last load was for. */
export type LoadFor = {
  readonly key: string;
  readonly rev: number | undefined;
  readonly live: boolean;
};

/** True on the first call, for another session, for a new `review_rev`, and after a reconnect. */
export function needsReload(prev: LoadFor | null, next: LoadFor): boolean {
  if (prev === null) return true;
  return prev.key !== next.key || prev.rev !== next.rev || (next.live && !prev.live);
}

/**
 * The answer of "load older reviews" on top of the loaded one: the older reviews are appended
 * (a review is listed once), the threads and the revision are those of the newer answer.
 */
export function withOlder(data: ReviewThreads, older: ReviewThreads): ReviewThreads {
  const have = new Set(data.reviews.map((r) => r.review_id));
  return {
    ...older,
    reviews: [...data.reviews, ...older.reviews.filter((r) => !have.has(r.review_id))],
  };
}

const NO_REVIEW: ReviewState = {
  data: null,
  error: null,
  reload: () => {},
  byShort: new Map(),
  known: new Set(),
  moreReviews: false,
  loadOlder: () => {},
};

type Held = {
  readonly key: string;
  readonly data: ReviewThreads | null;
  readonly error: string | null;
};

const empty = (key: string): Held => ({ key, data: null, error: null });

/** The review state of the session `key`. It does nothing without review calls. */
export function useReview(key: SessionKey, rev: number | undefined, live: boolean): ReviewState {
  const actions = useReviewActions();
  const id = `${key.station_id}/${key.session_id}`;
  const [held, setHeld] = useState<Held>(() => empty(id));
  const heldRef = useRef(held);
  heldRef.current = held;
  const gate = useRef<Gate>(createGate());
  const last = useRef<LoadFor | null>(null);
  const station = key.station_id;
  const session = key.session_id;

  const load = useCallback(
    (older: boolean) => {
      if (actions === undefined) return;
      const target = { station_id: station, session_id: session };
      const call = gate.current.begin(id);
      const done = (fn: (now: Held) => Held) => {
        if (gate.current.fresh(call, id)) setHeld((now) => fn(now.key === id ? now : empty(id)));
      };
      let before: string | undefined;
      if (older) {
        const now = heldRef.current;
        const oldest = now.key === id ? now.data?.reviews.at(-1)?.review_id : undefined;
        if (oldest === undefined) return;
        before = oldest;
      }
      actions.list(target, before).then(
        (data) =>
          done((now) => ({
            key: id,
            error: null,
            data: older && now.data !== null ? withOlder(now.data, data) : data,
          })),
        (e: unknown) =>
          done((now) => ({
            ...now,
            error: e instanceof Error ? e.message : 'The review request failed.',
          })),
      );
    },
    [actions, station, session, id],
  );
  const loadRef = useRef(load);
  loadRef.current = load;

  useEffect(() => {
    const next = { key: id, rev, live };
    const prev = last.current;
    last.current = next;
    if (needsReload(prev, next)) loadRef.current(false);
  }, [id, rev, live]);

  const data = held.key === id ? held.data : null;
  const byShort = useMemo(() => {
    const map = new Map<string, ReviewComment>();
    for (const t of data?.threads ?? []) for (const c of t.comments) map.set(c.short_id, c);
    return map;
  }, [data]);
  const known = useMemo(() => new Set(byShort.keys()), [byShort]);
  const reload = useCallback(() => loadRef.current(false), []);
  const loadOlder = useCallback(() => loadRef.current(true), []);
  if (actions === undefined) return NO_REVIEW;
  return {
    data,
    error: held.key === id ? held.error : null,
    reload,
    byShort,
    known,
    moreReviews: data?.more_reviews ?? false,
    loadOlder,
  };
}
