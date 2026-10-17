import { describe, expect, test } from 'bun:test';
import type { ReviewComment, ReviewThread, SessionKey } from '../gen';
import { ApiError } from '../runtime/api';
import { type ReviewActions, type ReviewHttp, reviewActions, reviewHttp } from './api';
import { diffReview, type ReviewProps } from './diffReview';
import type { ReviewState } from './useReview';

const key: SessionKey = { station_id: 'st/1', session_id: 's 1' };

const comment = {
  comment_id: 'c1',
  short_id: 'c-abc234',
  thread_id: 'c1',
  snapshot: 'spawn:base',
  path: 'src/a.ts',
  side: 'new',
  line_start: 2,
  line_end: 4,
  anchor_text: 'x',
  anchor_kind: 'diff',
  body: 'b',
  author_kind: 'owner',
  state: 'sent',
  resolved: false,
  created_at: 1,
  updated_at: 1,
} satisfies ReviewComment;

type Seen = { method: string; path: string; body?: unknown; timeout?: number | undefined };

function fakeHttp(answers: Record<string, unknown>, seen: Seen[]): ReviewHttp {
  const answer = (method: string, path: string) => {
    const found = answers[`${method} ${path.split('?')[0]}`];
    if (found === undefined) throw new Error(`no answer for ${method} ${path}`);
    return found;
  };
  return {
    get: async (path, timeout) => {
      seen.push({ method: 'GET', path, timeout });
      return answer('GET', path);
    },
    post: async (path, body) => {
      seen.push({ method: 'POST', path, body });
      return answer('POST', path);
    },
    patch: async (path, body) => {
      seen.push({ method: 'PATCH', path, body });
      return answer('PATCH', path);
    },
    del: async (path) => {
      seen.push({ method: 'DELETE', path });
      return null;
    },
  };
}

describe('reviewActions', () => {
  const base = '/api/sessions/st%2F1/s%201/review';
  const summary = { review_id: 'r1', short_id: 'r-abc234', delivery: 'pending' };
  const answers = {
    [`GET ${base}`]: { threads: [], reviews: [], rev: 3, more_reviews: false },
    [`POST ${base}/comments`]: comment,
    [`PATCH ${base}/comments/c%2F1`]: comment,
    [`POST ${base}/send`]: summary,
    [`POST ${base}/reviews/r%201/send-again`]: summary,
    '/api/stations/st%2F1/sessions/s%201/turn-diff': null,
  };
  const make = () => {
    const seen: Seen[] = [];
    const diff = {
      summary: {},
      patch: '',
      truncated: false,
      from_commit: 'a',
      to_commit: 'b',
    };
    return {
      seen,
      actions: reviewActions(
        fakeHttp({ ...answers, 'GET /api/stations/st%2F1/sessions/s%201/turn-diff': diff }, seen),
      ),
    };
  };

  test('every path segment is escaped and each call uses its method', async () => {
    const { seen, actions } = make();
    await actions.list(key);
    await actions.list(key, 'r/9');
    await actions.add(key, {
      snapshot: 's',
      path: 'p',
      side: 'new',
      line_start: 1,
      line_end: 1,
      body: 'b',
    });
    await actions.edit(key, 'c/1', { body: 'x' });
    await actions.remove(key, 'c/1');
    await actions.send(key, { send_key: 'k', comment_ids: ['c1'] });
    await actions.sendAgain(key, 'r 1', { attempt_key: 'a' });
    expect(seen.map((s) => `${s.method} ${s.path}`)).toEqual([
      `GET ${base}`,
      `GET ${base}?reviews_before=r%2F9`,
      `POST ${base}/comments`,
      `PATCH ${base}/comments/c%2F1`,
      `DELETE ${base}/comments/c%2F1`,
      `POST ${base}/send`,
      `POST ${base}/reviews/r%201/send-again`,
    ]);
  });

  test('the turn diff waits 30 s and every other call waits for the default', async () => {
    const { seen, actions } = make();
    await actions.list(key);
    await actions.turnDiff(key, 'from/1', 'to');
    expect(seen[0]?.timeout).toBeUndefined();
    expect(seen[1]).toMatchObject({
      path: '/api/stations/st%2F1/sessions/s%201/turn-diff?from=from%2F1&to=to',
      timeout: 30_000,
    });
  });

  test('an answer of another shape is refused', async () => {
    const actions = reviewActions(
      fakeHttp(
        { [`GET ${base}`]: { threads: [] }, [`POST ${base}/comments`]: { comment_id: 'x' } },
        [],
      ),
    );
    await expect(actions.list(key)).rejects.toThrow('The deck sent an invalid review list.');
    await expect(
      actions.add(key, {
        snapshot: 's',
        path: 'p',
        side: 'new',
        line_start: 1,
        line_end: 1,
        body: 'b',
      }),
    ).rejects.toThrow('invalid comment');
  });
});

describe('reviewHttp', () => {
  const reply = (status: number, body: unknown) =>
    new Response(body === undefined ? null : JSON.stringify(body), { status });

  test('PATCH and DELETE carry the CSRF token and the cookie', async () => {
    const calls: { path: string; init: RequestInit }[] = [];
    const http = reviewHttp(
      async (path, init) => {
        calls.push({ path, init });
        return init.method === 'DELETE' ? reply(204, undefined) : reply(200, { ok: true });
      },
      () => 'tok',
      1000,
    );
    expect(await http.patch('/p', { body: 'x' })).toEqual({ ok: true });
    expect(await http.del('/p')).toBeNull();
    expect(calls.map((c) => c.init.method)).toEqual(['PATCH', 'DELETE']);
    for (const c of calls) {
      expect(c.init.credentials).toBe('same-origin');
      expect((c.init.headers as Record<string, string>)['X-CSRF-Token']).toBe('tok');
    }
    expect(calls[0]?.init.body).toBe('{"body":"x"}');
    expect(calls[1]?.init.body).toBeNull();
  });

  test('a failure throws an ApiError with the message and the code of the deck', async () => {
    const http = reviewHttp(
      async () => reply(409, { error: 'not_a_draft', message: 'The comment was sent.' }),
      () => null,
      1000,
    );
    const error = await http.patch('/p', {}).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      status: 409,
      message: 'The comment was sent.',
      code: 'not_a_draft',
    });
  });

  test('a failure without a JSON body keeps the status text', async () => {
    const http = reviewHttp(
      async () => new Response('nope', { status: 500 }),
      () => null,
      1000,
    );
    const error = await http.del('/p').catch((e: unknown) => e);
    expect(error).toMatchObject({ status: 500, message: 'Request failed (500)', code: null });
  });
});

describe('diffReview', () => {
  const thread: ReviewThread = { thread_id: 'c1', comments: [comment] };
  function props() {
    const calls: unknown[][] = [];
    let reloads = 0;
    const actions: ReviewActions = {
      list: async () => {
        throw new Error('unused');
      },
      add: async (_k, body) => {
        calls.push(['add', body]);
        return comment;
      },
      edit: async (_k, id, patch) => {
        calls.push(['edit', id, patch]);
        return comment;
      },
      remove: async (_k, id) => {
        calls.push(['remove', id]);
      },
      send: async () => {
        throw new Error('unused');
      },
      sendAgain: async () => {
        throw new Error('unused');
      },
      turnDiff: async () => {
        throw new Error('unused');
      },
    };
    const state: ReviewState = {
      data: { threads: [thread], reviews: [], rev: 1, more_reviews: false },
      error: null,
      reload: () => {
        reloads += 1;
      },
      byShort: new Map(),
      known: new Set(),
      moreReviews: false,
      loadOlder: () => {},
    };
    const review: ReviewProps = {
      state,
      actions,
      key,
      canWrite: true,
      self: 'owner',
      focus: { threadId: 'c1', nonce: 1 },
    };
    return { calls, review, reloads: () => reloads };
  }

  test('Add sends exactly the six fields and reloads', async () => {
    const { calls, review, reloads } = props();
    const r = diffReview(review, 'spawn:base');
    await r.onAdd({ side: 'old', path: 'src/a.ts', line_start: 2, line_end: 3 }, 'hi');
    expect(calls).toEqual([
      [
        'add',
        {
          snapshot: 'spawn:base',
          path: 'src/a.ts',
          side: 'old',
          line_start: 2,
          line_end: 3,
          body: 'hi',
        },
      ],
    ]);
    expect(Object.keys(calls[0]?.[1] as object).sort()).toEqual(
      ['body', 'line_end', 'line_start', 'path', 'side', 'snapshot'].sort(),
    );
    expect(reloads()).toBe(1);
  });

  test('Reply sends the thread_id and the anchor fields of the top comment', async () => {
    const { calls, review } = props();
    await diffReview(review, 'turn:a..b').onReply('c1', 'me too');
    expect(calls).toEqual([
      [
        'add',
        {
          snapshot: 'spawn:base',
          path: 'src/a.ts',
          side: 'new',
          line_start: 2,
          line_end: 4,
          body: 'me too',
          thread_id: 'c1',
        },
      ],
    ]);
  });

  test('a reply to a thread that is not loaded fails and sends nothing', async () => {
    const { calls, review } = props();
    await expect(diffReview(review, 's').onReply('nope', 'x')).rejects.toThrow();
    expect(calls).toEqual([]);
  });

  test('Edit, Resolve and Delete call the deck and reload; the focus is the thread id', async () => {
    const { calls, review, reloads } = props();
    const r = diffReview(review, 's');
    await r.onEdit('c1', 'new text');
    await r.onResolve('c1', true);
    await r.onDelete('c1');
    expect(calls).toEqual([
      ['edit', 'c1', { body: 'new text' }],
      ['edit', 'c1', { resolved: true }],
      ['remove', 'c1'],
    ]);
    expect(reloads()).toBe(3);
    expect(r.focus).toBe('c1');
    expect(r.threads).toEqual([thread]);
  });
});
