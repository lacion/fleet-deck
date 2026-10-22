// R6.5 (DL-169): `#/activity`, a merged, newest-first stream of agent messages, tool completions,
// approvals, mail and session lifecycle across the fleet. A left list picks the channel: All, a
// repository or a session. The stream comes from `GET /api/activity` and refreshes while the page
// is open. Posting to a channel is mail to its session, or to the live sessions of its repository.
import { type ReactNode, useCallback, useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import type { ActivityItem, DeckSession, StationView } from '../gen';
import type { FleetActions } from '../fleet/fleet';
import type { ViewsActions } from '../runtime/views';
import {
  Icon,
  Mail,
  MessageSquare,
  MessagesSquare,
  Play,
  TriangleAlert,
  Wrench,
} from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import {
  ALL_CHANNEL,
  type Channel,
  channelId,
  channelList,
  channelQuery,
  inChannel,
  mailTargets,
} from './channels';

/** The rows that one request asks for. */
export const PAGE_LIMIT = 200;
/** How often the open page asks again. */
export const REFRESH_MS = 10_000;

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly live: boolean;
  readonly views: ViewsActions | undefined;
  /** The existing mail action; absent without the drive scope. */
  readonly fleet: FleetActions | undefined;
  /** The stream at the first render (tests). */
  readonly initialItems?: readonly ActivityItem[] | null;
  readonly initialChannel?: Channel;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
};

const KIND_ICON = {
  message: MessageSquare,
  tool: Wrench,
  approval: TriangleAlert,
  mail: Mail,
  lifecycle: Play,
} as const;

/** The repository without its forge host: "github.com/lacion/fleet-deck" → "lacion/fleet-deck". A
 * first part without a dot is not a host and stays. */
const repoLabel = (repo: string) => repo.replace(/^[^/]*\.[^/]*\//, '');

function Row({
  item,
  now,
  stations,
}: {
  readonly item: ActivityItem;
  readonly now: number;
  readonly stations: readonly StationView[];
}) {
  const href = formatHash({
    name: 'session',
    station: item.station_id,
    session: item.session_id,
    tab: 'chat',
  });
  return (
    <li className="fd-arow" data-kind={item.kind}>
      <span className="fd-arow-kind">
        <Icon of={KIND_ICON[item.kind]} size={14} />
        <span className="fd-vh">{item.kind}</span>
      </span>
      <span className="fd-arow-main">
        <a className="fd-arow-session" href={href}>
          {item.session_title ?? item.session_id}
        </a>
        <span className="fd-arow-text">{item.text}</span>
      </span>
      <span className="fd-arow-meta">
        <span
          title={
            item.repo === null
              ? stationName(item.station_id, stations)
              : `${item.repo} on ${stationName(item.station_id, stations)}`
          }
        >
          {item.repo === null ? '' : repoLabel(item.repo)}
        </span>
        <time dateTime={new Date(item.at).toISOString()}>{relativeAge(item.at, now)}</time>
      </span>
    </li>
  );
}

function Post({
  channel,
  sessions,
  fleet,
}: {
  readonly channel: Channel;
  readonly sessions: readonly DeckSession[];
  readonly fleet: FleetActions | undefined;
}) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const targets = mailTargets(sessions, channel);
  if (fleet === undefined || channel.kind === 'all') return null;
  const where =
    channel.kind === 'repo' ? `${targets.length} live in ${channel.name}` : 'this session';
  const send = () => {
    const body = text.trim();
    if (body === '' || targets.length === 0) return;
    setBusy(true);
    setNote(null);
    void Promise.all(targets.map((t) => fleet.mail(t, body, false))).then((results) => {
      setBusy(false);
      const failed = results.filter((r) => !r.ok);
      if (failed.length === 0) {
        setText('');
        setNote({
          ok: true,
          text:
            results.length === 1 ? 'Mail queued.' : `Mail queued for ${results.length} sessions.`,
        });
      } else {
        setNote({ ok: false, text: failed[0]?.text ?? 'Mail not sent.' });
      }
    });
  };
  return (
    <form
      className="fd-apost"
      onSubmit={(event) => {
        event.preventDefault();
        send();
      }}
    >
      <label className="fd-vh" htmlFor="fd-apost-text">
        Mail to {where}
      </label>
      <input
        id="fd-apost-text"
        type="text"
        autoComplete="off"
        placeholder={`Mail to ${where}`}
        value={text}
        onChange={(event) => setText(event.target.value)}
      />
      <button
        type="submit"
        className="fd-primary"
        disabled={busy || text.trim() === '' || targets.length === 0}
      >
        Send
      </button>
      {note === null ? null : (
        <span className="fd-note" role={note.ok ? 'status' : 'alert'}>
          {note.text}
        </span>
      )}
    </form>
  );
}

function ChannelButton({
  label,
  count,
  on,
  onPick,
  children,
}: {
  readonly label: string;
  readonly count?: number | undefined;
  readonly on: boolean;
  readonly onPick: () => void;
  readonly children?: ReactNode;
}) {
  return (
    <button type="button" className="fd-achan" aria-pressed={on} onClick={onPick}>
      <span className="fd-achan-label">{label}</span>
      {count === undefined ? null : <span className="fd-count">{count}</span>}
      {children}
    </button>
  );
}

/** The activity page: the channel list, the stream and the mail form. */
export function ActivityPage({
  sessions,
  stations,
  live,
  views,
  fleet,
  initialItems = null,
  initialChannel = ALL_CHANNEL,
  now: givenNow,
}: Props) {
  const [channel, setChannel] = useState<Channel>(initialChannel);
  const [items, setItems] = useState<readonly ActivityItem[] | null>(initialItems);
  const [error, setError] = useState<string | null>(null);
  const now = useNow(givenNow, 30_000);
  const list = channelList(sessions);
  // The query depends on the repository's origin; keep the latest sessions in a ref so a card
  // change does not restart the refresh timer.
  const latest = useRef(sessions);
  latest.current = sessions;
  const id = channelId(channel);
  const shown = inChannel(items ?? [], sessions, channel);
  const load = useCallback(() => {
    if (views === undefined) return;
    views.activity(channelQuery(latest.current, channel, PAGE_LIMIT)).then(
      (result) => {
        setItems(result.items);
        setError(null);
      },
      (e: unknown) => setError(e instanceof Error ? e.message : 'The stream did not load.'),
    );
  }, [views, channel]);
  useEffect(() => {
    if (views === undefined || !live) return undefined;
    setItems(null);
    load();
    const timer = setInterval(load, REFRESH_MS);
    return () => clearInterval(timer);
    // `id` stands for the channel; `load` already depends on it.
  }, [views, live, id, load]);

  return (
    <Page title="Activity" className="fd-activity" wide>
      <div className="fd-awrap">
        <nav className="fd-achans" aria-label="Channels">
          <ChannelButton
            label="All"
            on={channel.kind === 'all'}
            onPick={() => setChannel(ALL_CHANNEL)}
          />
          {list.repos.length === 0 ? null : <h2 className="fd-label">Repositories</h2>}
          {list.repos.map((e) => (
            <ChannelButton
              key={channelId(e.channel)}
              label={e.label}
              count={e.count}
              on={channelId(e.channel) === id}
              onPick={() => setChannel(e.channel)}
            />
          ))}
          {list.sessions.length === 0 ? null : <h2 className="fd-label">Sessions</h2>}
          {list.sessions.map((e) => (
            <ChannelButton
              key={channelId(e.channel)}
              label={e.label}
              on={channelId(e.channel) === id}
              onPick={() => setChannel(e.channel)}
            />
          ))}
        </nav>
        <div className="fd-astream">
          <Post key={id} channel={channel} sessions={sessions} fleet={fleet} />
          {error === null ? null : (
            <p className="fd-note" role="alert">
              {error}
            </p>
          )}
          {items === null ? (
            <p className="fd-empty">{live ? 'Loading…' : 'The stream needs a live connection.'}</p>
          ) : shown.length === 0 ? (
            <EmptyState
              icon={MessagesSquare}
              title="Nothing happened here yet"
              body="Turns, tools, asks and mail of the sessions show up here."
            />
          ) : (
            <ol className="fd-alist" aria-label="Activity, newest first">
              {shown.map((item, n) => (
                <Row
                  key={`${item.station_id}/${item.session_id}/${item.station_seq ?? `mail${n}`}`}
                  item={item}
                  now={now}
                  stations={stations}
                />
              ))}
            </ol>
          )}
        </div>
      </div>
    </Page>
  );
}
