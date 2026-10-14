import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ReviewComment, ReviewSummary, ReviewThread, SessionKey } from '../gen';
import type { ReviewActions } from './api';
import { ReviewBar } from './ReviewBar';
import {
  canSendAgain,
  confirmText,
  discardDrafts,
  discardIds,
  draftsText,
  keyHolder,
  keyMap,
  principalOf,
  sendDrafts,
  sendIds,
  sendLabel,
  troubled,
  troubleText,
} from './send';
import type { ReviewState } from './useReview';

const session: SessionKey = { station_id: 'st', session_id: 's1' };

function comment(id: string, patch: Partial<ReviewComment> = {}): ReviewComment {
  return {
    comment_id: id,
    short_id: `c-${id}`,
    thread_id: id,
    snapshot: 'spawn:b',
    path: 'a.ts',
    side: 'new',
    line_start: 1,
    line_end: 1,
    anchor_text: 'x',
    anchor_kind: 'diff',
    body: 'b',
    author_kind: 'owner',
    state: 'draft',
    resolved: false,
    created_at: 1,
    updated_at: 1,
    ...patch,
  };
}

const thread = (...comments: ReviewComment[]): ReviewThread => ({
  thread_id: comments[0]?.comment_id ?? '',
  comments,
});

function review(id: string, patch: Partial<ReviewSummary> = {}): ReviewSummary {
  return {
    review_id: id,
    short_id: `r-${id}`,
    principal: 'owner',
    delivery: 'delivered',
    attempts: 1,
    sent_at: 1,
    comment_ids: [],
    command_id: 'x',
    ...patch,
  };
}

function stateOf(threads: ReviewThread[], reviews: ReviewSummary[] = []): ReviewState {
  return {
    data: { threads, reviews, rev: 1, more_reviews: false },
    error: null,
    reload: () => {},
    byShort: new Map(),
    known: new Set(),
    moreReviews: false,
    loadOlder: () => {},
  };
}

const actions: ReviewActions = {
  list: async () => {
    throw new Error('unused');
  },
  add: async () => {
    throw new Error('unused');
  },
  edit: async () => {
    throw new Error('unused');
  },
  remove: async () => {},
  send: async () => review('r1'),
  sendAgain: async () => review('r1'),
  turnDiff: async () => {
    throw new Error('unused');
  },
};

const bar = (state: ReviewState, canWrite = true, self = 'owner') =>
  renderToStaticMarkup(
    <ReviewBar state={state} actions={actions} session={session} canWrite={canWrite} self={self} />,
  );

describe('the draft ids', () => {
  const threads = [
    thread(comment('t1', { created_at: 3 }), comment('r1', { created_at: 4 })),
    thread(comment('t2', { created_at: 1 }), comment('r2', { created_at: 2, state: 'sent' })),
    thread(comment('t3', { created_at: 2, state: 'sent' }), comment('r3', { created_at: 5 })),
  ];
  test('a send has the top comments first, then the replies, oldest first', () => {
    expect(sendIds(threads)).toEqual(['t2', 't1', 'r1', 'r3']);
  });
  test('discard has the replies first, then the top comments', () => {
    expect(discardIds(threads)).toEqual(['r1', 'r3', 't2', 't1']);
  });
  test('a send has at most 50 comments', () => {
    const many = Array.from({ length: 51 }, (_, i) => thread(comment(`d${i}`, { created_at: i })));
    expect(sendIds(many)).toHaveLength(50);
    expect(sendIds(many)[0]).toBe('d0');
    expect(sendLabel(51)).toBe('Send 50 of 51');
    expect(sendLabel(50)).toBe('Send review');
  });
});

describe('texts', () => {
  test('draft counts and the confirm text', () => {
    expect(draftsText(1)).toBe('Review: 1 draft');
    expect(draftsText(3)).toBe('Review: 3 drafts');
    expect(confirmText(1)).toBe('Delete 1 draft?');
    expect(confirmText(3)).toBe('Delete 3 drafts?');
  });
  test('a failed review says why, or only that it was not delivered', () => {
    expect(troubleText(review('a', { delivery: 'failed', reason: 'station offline' }))).toBe(
      'Review r-a was not delivered: station offline.',
    );
    expect(troubleText(review('a', { delivery: 'failed' }))).toBe('Review r-a was not delivered.');
    expect(troubleText(review('a', { delivery: 'unknown' }))).toBe(
      'Delivery unknown: the agent may have received review r-a.',
    );
  });
  test('Send again is for a failed review with fewer than 5 attempts', () => {
    expect(canSendAgain(review('a', { delivery: 'failed', attempts: 4 }))).toBe(true);
    expect(canSendAgain(review('a', { delivery: 'failed', attempts: 5 }))).toBe(false);
    expect(canSendAgain(review('a', { delivery: 'unknown', attempts: 1 }))).toBe(false);
  });
});

describe('the send key', () => {
  test('it stays the same across a failed send and changes after a success', async () => {
    let n = 0;
    const holder = keyHolder(() => `key${++n}`);
    const seen: string[] = [];
    let fail = true;
    const fake = {
      send: async (_k: SessionKey, body: { send_key: string }) => {
        seen.push(body.send_key);
        if (fail) throw new Error('offline');
        return review('r1');
      },
    };
    const threads = [thread(comment('t1'))];
    await expect(sendDrafts(fake, session, holder, threads)).rejects.toThrow('offline');
    await expect(sendDrafts(fake, session, holder, threads)).rejects.toThrow('offline');
    fail = false;
    await sendDrafts(fake, session, holder, threads);
    await sendDrafts(fake, session, holder, threads);
    expect(seen).toEqual(['key1', 'key1', 'key1', 'key2']);
  });

  test('a send sends the comment ids and the key, and no other field', async () => {
    const bodies: unknown[] = [];
    const fake = {
      send: async (_k: SessionKey, body: unknown) => {
        bodies.push(body);
        return review('r1');
      },
    };
    await sendDrafts(
      fake,
      session,
      keyHolder(() => 'k'),
      [thread(comment('t1'), comment('r1'))],
    );
    expect(bodies).toEqual([{ send_key: 'k', comment_ids: ['t1', 'r1'] }]);
  });

  test('the key of the board is 25 characters of [0-9a-z]', () => {
    expect(keyHolder().current()).toMatch(/^[0-9a-z]{25}$/);
  });
});

describe('the attempt key', () => {
  test('it is kept per review until the attempt is done', () => {
    let n = 0;
    const keys = keyMap(() => `a${++n}`);
    expect(keys.get('r1')).toBe('a1');
    expect(keys.get('r1')).toBe('a1');
    expect(keys.get('r2')).toBe('a2');
    keys.done('r1');
    expect(keys.get('r1')).toBe('a3');
  });
});

describe('discard', () => {
  test('it removes every draft, replies first, and stops at a failure', async () => {
    const removed: string[] = [];
    const fake = {
      remove: async (_k: SessionKey, id: string) => {
        if (id === 't2') throw new Error('no');
        removed.push(id);
      },
    };
    const threads = [thread(comment('t1'), comment('r1')), thread(comment('t2'))];
    await expect(discardDrafts(fake, session, threads)).rejects.toThrow('no');
    expect(removed).toEqual(['r1', 't1']);
  });
});

describe('the troubled reviews', () => {
  const reviews = [
    review('a', { delivery: 'failed', sent_at: 1 }),
    review('b', { delivery: 'unknown', sent_at: 3 }),
    review('c', { delivery: 'delivered', sent_at: 4 }),
    review('d', { delivery: 'failed', sent_at: 2, principal: 'member:ana' }),
    ...['e', 'f', 'g', 'h'].map((id, n) => review(id, { delivery: 'failed', sent_at: 10 + n })),
  ];
  test('the owner sees its own failed and unknown reviews, newest first, at most 5', () => {
    expect(troubled(reviews, 'owner').map((r) => r.review_id)).toEqual(['h', 'g', 'f', 'e', 'b']);
  });
  test('a member sees its own', () => {
    expect(troubled(reviews, 'ana').map((r) => r.review_id)).toEqual(['d']);
    expect(principalOf('ana')).toBe('member:ana');
    expect(principalOf('owner')).toBe('owner');
  });
});

describe('ReviewBar', () => {
  test('it shows the drafts, Send review and Discard all', () => {
    const html = bar(stateOf([thread(comment('t1')), thread(comment('t2'))]));
    expect(html).toContain('Review: 2 drafts');
    expect(html).toContain('Send review');
    expect(html).toContain('Discard all');
    expect(html).toContain('class="fd-review-bar"');
  });

  test('51 drafts say "Send 50 of 51"', () => {
    const many = Array.from({ length: 51 }, (_, i) => thread(comment(`d${i}`)));
    expect(bar(stateOf(many))).toContain('Send 50 of 51');
  });

  test('a failed review shows its text and Send again', () => {
    const html = bar(
      stateOf([], [review('a', { delivery: 'failed', reason: 'offline', attempts: 1 })]),
    );
    expect(html).toContain('Review r-a was not delivered: offline.');
    expect(html).toContain('Send again');
    // No drafts: no draft bar.
    expect(html).not.toContain('Send review');
    expect(html).not.toContain('Discard all');
  });

  test('a failed review with 5 attempts has no Send again', () => {
    const html = bar(stateOf([], [review('a', { delivery: 'failed', attempts: 5 })]));
    expect(html).not.toContain('Send again');
  });

  test('an unknown delivery has no button', () => {
    const html = bar(stateOf([], [review('a', { delivery: 'unknown' })]));
    expect(html).toContain('Delivery unknown: the agent may have received review r-a.');
    expect(html).not.toContain('<button');
  });

  test('nothing renders without write access, without drafts, or without data', () => {
    expect(bar(stateOf([thread(comment('t1'))]), false)).toBe('');
    expect(bar(stateOf([]))).toBe('');
    expect(bar({ ...stateOf([]), data: null })).toBe('');
  });

  test('a delivered review is not listed', () => {
    expect(bar(stateOf([], [review('a')]))).toBe('');
  });

  test("another principal's review is not listed", () => {
    expect(bar(stateOf([], [review('a', { delivery: 'failed', principal: 'member:ana' })]))).toBe(
      '',
    );
  });
});
