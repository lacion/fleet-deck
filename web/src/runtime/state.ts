import type {
  BoardServerMessage,
  DeckSession,
  EventEnvelope,
  SessionKey,
  StationView,
} from '../gen';
import { appendFeed, type FeedItem, feedItems } from '../board/feed';

export type Freshness = 'stale' | 'live';
export type Transport = 'stopped' | 'connecting' | 'connected' | 'reconnecting';
export type AuthView = {
  actor: string;
  scopes: ('read' | 'drive' | 'approve' | 'admin')[];
  csrf: string | null;
  expires_at: number;
};
export type Timeline = {
  incarnation: string | null;
  cursor: number;
  events: readonly EventEnvelope[];
  freshness: Freshness;
  error: string | null;
  ready: boolean;
};
export type RuntimeState = {
  auth: 'checking' | 'anonymous' | 'authenticated';
  login: AuthView | null;
  transport: Transport;
  freshness: Freshness;
  deckIncarnation: string | null;
  version: number | null;
  stations: readonly StationView[];
  sessions: readonly DeckSession[];
  timelines: Readonly<Record<string, Timeline>>;
  /** The event feed since this page opened (DL-095), oldest first. */
  feed: readonly FeedItem[];
  error: string | null;
};
export const emptyState = (): RuntimeState => ({
  auth: 'checking',
  login: null,
  transport: 'stopped',
  freshness: 'stale',
  deckIncarnation: null,
  version: null,
  stations: [],
  sessions: [],
  timelines: {},
  feed: [],
  error: null,
});
export const sessionKey = (key: SessionKey): string =>
  JSON.stringify([key.station_id, key.session_id]);
export const emptyTimeline = (): Timeline => ({
  incarnation: null,
  cursor: 0,
  events: [],
  freshness: 'stale',
  error: null,
  ready: false,
});
export function stale(state: RuntimeState): RuntimeState {
  return {
    ...state,
    freshness: 'stale',
    timelines: Object.fromEntries(
      Object.entries(state.timelines).map(([key, t]) => [
        key,
        { ...t, freshness: 'stale', ready: false },
      ]),
    ),
  };
}

/** The feed of `state` with the items of the changed `cards`. */
function changes(state: RuntimeState, cards: readonly DeckSession[]): FeedItem[] {
  const before = new Map(
    state.sessions.map((c) => [
      sessionKey({ station_id: c.station_id, session_id: c.summary.session_id }),
      c,
    ]),
  );
  const items = cards.flatMap((c) =>
    feedItems(
      before.get(sessionKey({ station_id: c.station_id, session_id: c.summary.session_id })),
      c,
    ),
  );
  return appendFeed(state.feed, items);
}

/** One ordered socket stream. Old socket callbacks are fenced by Connection. */
export function reduceMessage(state: RuntimeState, msg: BoardServerMessage): RuntimeState {
  switch (msg.type) {
    case 'hello':
      if (msg.protocol !== 1) throw new Error('Unsupported board protocol');
      return { ...state, deckIncarnation: msg.deck_incarnation };
    case 'snapshot':
      // The version is process-local: a deck restart can start again at zero.
      if (state.freshness === 'live') return state;
      return {
        ...state,
        // The first snapshot is the starting point; a later one (after a
        // reconnect) adds the changes that the board missed.
        feed: state.version === null ? state.feed : changes(state, msg.sessions),
        version: msg.version,
        stations: msg.stations,
        sessions: msg.sessions,
        freshness: 'live',
        error: null,
      };
    case 'delta': {
      if (state.freshness !== 'live' || state.version === null || msg.version <= state.version)
        return state;
      const cards = new Map(
        state.sessions.map((c) => [
          sessionKey({
            station_id: c.station_id,
            session_id: c.summary.session_id,
          }),
          c,
        ]),
      );
      const feed = changes(state, msg.sessions);
      for (const key of msg.removed) cards.delete(sessionKey(key));
      for (const c of msg.sessions)
        cards.set(
          sessionKey({
            station_id: c.station_id,
            session_id: c.summary.session_id,
          }),
          c,
        );
      return {
        ...state,
        version: msg.version,
        stations: msg.stations ?? state.stations,
        sessions: [...cards.values()],
        feed,
      };
    }
    case 'timeline_start': {
      const key = sessionKey(msg);
      const t = state.timelines[key];
      if (!t) return state;
      const reset = msg.reset || msg.incarnation !== t.incarnation;
      return {
        ...state,
        timelines: {
          ...state.timelines,
          [key]: {
            ...(reset ? emptyTimeline() : t),
            incarnation: msg.incarnation,
            freshness: 'live',
            ready: true,
            error: null,
          },
        },
      };
    }
    case 'timeline_events': {
      const key = sessionKey(msg);
      const t = state.timelines[key];
      if (!t?.ready) return state;
      let cursor = t.cursor;
      const added: EventEnvelope[] = [];
      for (const event of msg.events) {
        if (event.session_id !== msg.session_id || !Number.isSafeInteger(event.station_seq)) {
          throw new Error('Invalid timeline envelope');
        }
        if (event.station_seq <= cursor) continue;
        added.push(event);
        cursor = event.station_seq;
      }
      return {
        ...state,
        timelines: {
          ...state.timelines,
          [key]: {
            ...t,
            cursor,
            events: [...t.events, ...added].slice(-1000),
          },
        },
      };
    }
    case 'error':
      return {
        ...state,
        error: msg.message,
        timelines: Object.fromEntries(
          Object.entries(state.timelines).map(([key, t]) => [
            key,
            t.ready
              ? t
              : {
                  ...t,
                  freshness: 'stale',
                  error: msg.message,
                },
          ]),
        ),
      };
    default:
      msg satisfies never;
      throw new Error('Unknown board message');
  }
}
