// DL-179: the review calls of the board. The shapes are the deck's
// (`crates/fleetdeck-deck/src/http/review_routes.rs`).
import type {
  CommentPatch,
  NewComment,
  ReviewComment,
  ReviewSummary,
  ReviewThreads,
  SendAgain,
  SendReview,
  SessionKey,
  TurnDiff,
} from '../gen';
import { ApiError, type Fetch } from '../runtime/api';

/** The longest wait for a turn diff, in ms (the station reads it from Git). */
export const TURN_DIFF_WAIT_MS = 30_000;

export type ReviewHttp = {
  get(path: string, timeoutMs?: number): Promise<unknown>;
  post(path: string, body: unknown): Promise<unknown>;
  patch(path: string, body: unknown): Promise<unknown>;
  del(path: string): Promise<unknown>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

async function failure(response: Response): Promise<ApiError> {
  let message = `Request failed (${response.status})`;
  let code: string | null = null;
  try {
    const value: unknown = await response.json();
    if (record(value)) {
      if (typeof value['message'] === 'string') message = value['message'];
      if (typeof value['error'] === 'string') code = value['error'];
    }
  } catch {
    // The body is not JSON: the status text stays.
  }
  return new ApiError(response.status, message, code);
}

/**
 * `PATCH` and `DELETE` with the CSRF token of the login, read when the call is made. The same
 * rules as `integrationHttp`: the cookie, no cache, no redirect, one time limit.
 */
export function reviewHttp(
  fetcher: Fetch,
  csrf: () => string | null,
  timeoutMs: number,
): Pick<ReviewHttp, 'patch' | 'del'> {
  const call = async (
    path: string,
    method: 'PATCH' | 'DELETE',
    body?: unknown,
  ): Promise<unknown> => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const token = csrf();
      const headers: Record<string, string> = {};
      if (body !== undefined) headers['Content-Type'] = 'application/json';
      if (token !== null) headers['X-CSRF-Token'] = token;
      const response = await fetcher(path, {
        method,
        credentials: 'same-origin',
        cache: 'no-store',
        redirect: 'error',
        headers,
        signal: controller.signal,
        body: body === undefined ? null : JSON.stringify(body),
      });
      if (!response.ok) throw await failure(response);
      return response.status === 204 ? null : response.json();
    } finally {
      clearTimeout(timer);
    }
  };
  return {
    patch: (path, body) => call(path, 'PATCH', body),
    del: (path) => call(path, 'DELETE'),
  };
}

export type ReviewActions = {
  list(key: SessionKey, before?: string): Promise<ReviewThreads>;
  add(key: SessionKey, body: NewComment): Promise<ReviewComment>;
  edit(key: SessionKey, commentId: string, patch: CommentPatch): Promise<ReviewComment>;
  remove(key: SessionKey, commentId: string): Promise<void>;
  send(key: SessionKey, body: SendReview): Promise<ReviewSummary>;
  sendAgain(key: SessionKey, reviewId: string, body: SendAgain): Promise<ReviewSummary>;
  turnDiff(key: SessionKey, from: string, to: string): Promise<TurnDiff>;
};

function shaped<T>(v: unknown, fields: readonly string[], what: string): T {
  if (!record(v) || fields.some((f) => !(f in v))) {
    throw new Error(`The deck sent an invalid ${what}.`);
  }
  return v as T;
}

const seg = encodeURIComponent;

/** `/api/sessions/<station>/<session>/review`: the base of the review routes. */
export const reviewBase = (key: SessionKey): string =>
  `/api/sessions/${seg(key.station_id)}/${seg(key.session_id)}/review`;

const COMMENT_FIELDS = ['comment_id', 'short_id', 'thread_id'];
const SUMMARY_FIELDS = ['review_id', 'short_id', 'delivery'];

/** The review calls over one transport. */
export function reviewActions(http: ReviewHttp): ReviewActions {
  return {
    list: async (key, before) =>
      shaped<ReviewThreads>(
        await http.get(
          before === undefined
            ? reviewBase(key)
            : `${reviewBase(key)}?reviews_before=${seg(before)}`,
        ),
        ['threads', 'reviews', 'rev'],
        'review list',
      ),
    add: async (key, body) =>
      shaped<ReviewComment>(
        await http.post(`${reviewBase(key)}/comments`, body),
        COMMENT_FIELDS,
        'comment',
      ),
    edit: async (key, commentId, patch) =>
      shaped<ReviewComment>(
        await http.patch(`${reviewBase(key)}/comments/${seg(commentId)}`, patch),
        COMMENT_FIELDS,
        'comment',
      ),
    remove: async (key, commentId) => {
      await http.del(`${reviewBase(key)}/comments/${seg(commentId)}`);
    },
    send: async (key, body) =>
      shaped<ReviewSummary>(
        await http.post(`${reviewBase(key)}/send`, body),
        SUMMARY_FIELDS,
        'review',
      ),
    sendAgain: async (key, reviewId, body) =>
      shaped<ReviewSummary>(
        await http.post(`${reviewBase(key)}/reviews/${seg(reviewId)}/send-again`, body),
        SUMMARY_FIELDS,
        'review',
      ),
    turnDiff: async (key, from, to) =>
      shaped<TurnDiff>(
        await http.get(
          `/api/stations/${seg(key.station_id)}/sessions/${seg(key.session_id)}/turn-diff?from=${seg(from)}&to=${seg(to)}`,
          TURN_DIFF_WAIT_MS,
        ),
        ['summary', 'patch', 'truncated', 'from_commit', 'to_commit'],
        'turn diff',
      ),
  };
}
