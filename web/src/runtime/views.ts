// R6 (DL-169): the board calls of the views at many sessions: the read cursor
// of a session (`POST /api/sessions/{station}/{session}/read`) and the
// activity stream (`GET /api/activity`).
import type { ActivityItem, ActivityKind, ActivityList, SessionKey } from '../gen';

type Get = (path: string) => Promise<unknown>;
type Post = (path: string, body: unknown) => Promise<unknown>;

/** The filter of `GET /api/activity`; every part is optional. */
export type ActivityQuery = {
  readonly since?: number;
  readonly repo?: string;
  readonly session?: string;
  readonly limit?: number;
};

export type ViewsActions = {
  /** Moves the read cursor of the session up to `seq` (a `session_seq`). */
  markRead(key: SessionKey, seq: number): Promise<void>;
  activity(query: ActivityQuery): Promise<ActivityList>;
};

const record = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const KINDS: readonly ActivityKind[] = ['message', 'tool', 'approval', 'mail', 'lifecycle'];

/** Validates one row of the stream. */
export function activityItem(v: unknown): ActivityItem {
  if (
    !record(v) ||
    typeof v['station_id'] !== 'string' ||
    typeof v['session_id'] !== 'string' ||
    !(v['session_title'] === null || typeof v['session_title'] === 'string') ||
    !(v['repo'] === null || typeof v['repo'] === 'string') ||
    !KINDS.includes(v['kind'] as ActivityKind) ||
    typeof v['text'] !== 'string' ||
    typeof v['at'] !== 'number' ||
    !(v['station_seq'] === null || typeof v['station_seq'] === 'number')
  ) {
    throw new Error('The deck sent an invalid activity row.');
  }
  return v as ActivityItem;
}

/** The path of `GET /api/activity` for `query`. */
export function activityPath(query: ActivityQuery): string {
  const params = new URLSearchParams();
  if (query.since !== undefined) params.set('since', String(Math.trunc(query.since)));
  if (query.repo !== undefined && query.repo !== '') params.set('repo', query.repo);
  if (query.session !== undefined && query.session !== '') params.set('session', query.session);
  if (query.limit !== undefined) params.set('limit', String(query.limit));
  const text = params.toString();
  return text === '' ? '/api/activity' : `/api/activity?${text}`;
}

export function viewsActions(get: Get, post: Post): ViewsActions {
  return {
    markRead: async (key, seq) => {
      const station = encodeURIComponent(key.station_id);
      const session = encodeURIComponent(key.session_id);
      await post(`/api/sessions/${station}/${session}/read`, { seq });
    },
    activity: async (query) => {
      const v = await get(activityPath(query));
      if (!record(v) || !Array.isArray(v['items'])) {
        throw new Error('The deck sent an invalid activity list.');
      }
      return { items: v['items'].map(activityItem) };
    },
  };
}
