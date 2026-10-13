// R6.5 (DL-169): the channels of the activity stream: All, one per repository and one per
// session. A channel is a filter of `GET /api/activity`; posting to a channel is mail to its
// session, or to every live session of its repository.
import { repoName } from '../board/fleet';
import { sessionTitle } from '../app/sessions';
import type { ActivityItem, DeckSession } from '../gen';
import type { FleetTarget } from '../fleet/fleet';
import type { ActivityQuery } from '../runtime/views';

export type Channel =
  | { readonly kind: 'all' }
  | { readonly kind: 'repo'; readonly name: string }
  | { readonly kind: 'session'; readonly station: string; readonly session: string };

export const ALL_CHANNEL: Channel = { kind: 'all' };

export const channelId = (c: Channel): string =>
  c.kind === 'all'
    ? 'all'
    : c.kind === 'repo'
      ? `repo:${c.name}`
      : `session:${c.station}/${c.session}`;

/** The sessions of a repository that can take mail now. */
const takesMail = (c: DeckSession): boolean =>
  c.status !== 'ended' && c.status !== 'lost' && c.status !== 'unreachable';

/** The sessions of the board that belong to `channel`. */
export function channelSessions(
  sessions: readonly DeckSession[],
  channel: Channel,
): readonly DeckSession[] {
  switch (channel.kind) {
    case 'all':
      return sessions;
    case 'repo':
      return sessions.filter((c) => repoName(c) === channel.name);
    case 'session':
      return sessions.filter(
        (c) => c.station_id === channel.station && c.summary.session_id === channel.session,
      );
  }
}

/**
 * The deck filter of a channel. A session channel sends the session id. The deck matches a
 * repository by its origin or its root, and the board names it by `repoName`: the repository
 * channel sends the filter only when all its sessions have the same origin or root; otherwise
 * it asks for the whole stream and `inChannel` filters the rows.
 */
export function channelQuery(
  sessions: readonly DeckSession[],
  channel: Channel,
  limit: number,
): ActivityQuery {
  if (channel.kind === 'session') return { session: channel.session, limit };
  if (channel.kind === 'repo') {
    const keys = new Set(
      channelSessions(sessions, channel).map((c) => c.summary.repo?.origin ?? c.summary.repo?.root),
    );
    const [only] = [...keys];
    return keys.size === 1 && only !== undefined ? { repo: only, limit } : { limit };
  }
  return { limit };
}

/** The rows of `items` that belong to `channel`. */
export function inChannel(
  items: readonly ActivityItem[],
  sessions: readonly DeckSession[],
  channel: Channel,
): readonly ActivityItem[] {
  if (channel.kind === 'all') return items;
  const own = new Set(
    channelSessions(sessions, channel).map((c) => `${c.station_id}/${c.summary.session_id}`),
  );
  return items.filter((i) => own.has(`${i.station_id}/${i.session_id}`));
}

/** The mail targets of a channel: the live sessions of a repository or the one session. */
export function mailTargets(
  sessions: readonly DeckSession[],
  channel: Channel,
): readonly FleetTarget[] {
  if (channel.kind === 'all') return [];
  return channelSessions(sessions, channel)
    .filter((c) => channel.kind === 'session' || takesMail(c))
    .map((c) => ({ stationId: c.station_id, sessionId: c.summary.session_id }));
}

export type ChannelEntry = {
  readonly channel: Channel;
  readonly label: string;
  readonly count: number;
};

/** The left list below "All": one entry per repository, then one per session, newest first. */
export function channelList(sessions: readonly DeckSession[]): {
  readonly repos: readonly ChannelEntry[];
  readonly sessions: readonly ChannelEntry[];
} {
  const repoCount = new Map<string, number>();
  for (const c of sessions) repoCount.set(repoName(c), (repoCount.get(repoName(c)) ?? 0) + 1);
  const repos = [...repoCount.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(
      ([name, count]): ChannelEntry => ({ channel: { kind: 'repo', name }, label: name, count }),
    );
  const list = [...sessions]
    .sort((a, b) => b.summary.updated_at - a.summary.updated_at)
    .map(
      (c): ChannelEntry => ({
        channel: { kind: 'session', station: c.station_id, session: c.summary.session_id },
        label: sessionTitle(c),
        count: 1,
      }),
    );
  return { repos, sessions: list };
}
