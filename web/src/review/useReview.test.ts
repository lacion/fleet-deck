import { describe, expect, test } from 'bun:test';
import type { ReviewSummary, ReviewThreads } from '../gen';
import { createGate, needsReload, withOlder } from './useReview';

describe('the stale-answer gate', () => {
  test('only the latest call of the current session is fresh', () => {
    const gate = createGate();
    const first = gate.begin('st/s1');
    expect(gate.fresh(first, 'st/s1')).toBe(true);
    const second = gate.begin('st/s1');
    expect(gate.fresh(first, 'st/s1')).toBe(false);
    expect(gate.fresh(second, 'st/s1')).toBe(true);
  });

  test('an answer for another key is dropped', () => {
    const gate = createGate();
    const old = gate.begin('st/s1');
    const next = gate.begin('st/s2');
    expect(gate.fresh(old, 'st/s1')).toBe(false);
    expect(gate.fresh(next, 'st/s1')).toBe(false);
    expect(gate.fresh(next, 'st/s2')).toBe(true);
  });
});

describe('when the review state loads', () => {
  const base = { key: 'st/s1', rev: 1, live: true };
  test('on mount', () => {
    expect(needsReload(null, base)).toBe(true);
  });
  test('not when nothing changed', () => {
    expect(needsReload(base, { ...base })).toBe(false);
  });
  test('on a new review_rev', () => {
    expect(needsReload(base, { ...base, rev: 2 })).toBe(true);
    expect(needsReload({ ...base, rev: undefined }, base)).toBe(true);
  });
  test('after a reconnect, not when the board goes stale', () => {
    expect(needsReload({ ...base, live: false }, base)).toBe(true);
    expect(needsReload(base, { ...base, live: false })).toBe(false);
  });
  test('for another session', () => {
    expect(needsReload(base, { ...base, key: 'st/s2' })).toBe(true);
  });
});

const review = (id: string): ReviewSummary => ({
  review_id: id,
  short_id: `r-${id}`,
  principal: 'owner',
  delivery: 'delivered',
  attempts: 1,
  sent_at: 1,
  comment_ids: [],
  command_id: 'x',
});

describe('load older reviews', () => {
  const newer: ReviewThreads = {
    threads: [],
    reviews: [review('r3'), review('r2')],
    rev: 5,
    more_reviews: true,
  };
  test('the older reviews are appended and the threads replaced', () => {
    const answer: ReviewThreads = {
      threads: [{ thread_id: 't', comments: [] }],
      reviews: [review('r1')],
      rev: 6,
      more_reviews: false,
    };
    const merged = withOlder(newer, answer);
    expect(merged.reviews.map((r) => r.review_id)).toEqual(['r3', 'r2', 'r1']);
    expect(merged.threads).toEqual(answer.threads);
    expect(merged.rev).toBe(6);
    expect(merged.more_reviews).toBe(false);
  });
  test('a review that is already listed is not listed twice', () => {
    const merged = withOlder(newer, { ...newer, reviews: [review('r2'), review('r1')] });
    expect(merged.reviews.map((r) => r.review_id)).toEqual(['r3', 'r2', 'r1']);
  });
});
