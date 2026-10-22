// R6.5 (DL-169): `#/activity`, a merged, newest-first stream of agent messages, tool completions,
// approvals, mail and session lifecycle across the fleet. A left list picks the channel: All, a
// repository or a session. The stream comes from `GET /api/activity` and refreshes while the page
// is open. Posting to a channel is mail to its session, or to the live sessions of its repository.
import { type ReactNode, useCallback, useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import { PathTail } from '../chat/PathTail';
import type { ActivityItem, DeckSession, StationView } from '../gen';
import type { FleetActions } from '../fleet/fleet';
import type { ViewsActions } from '../runtime/views';
import {
  ChevronRight,
  FileText,
  Globe,
  Icon,
  type LucideIcon,
  Mail,
  MessageSquare,
  MessagesSquare,
  Pencil,
  Play,
  Plug,
  Search,
  Shield,
  SquareTerminal,
  Workflow,
  Wrench,
} from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { type ActivityTool, activityTool, homePath } from '../watch/tooltext';
import {
  ALL_CHANNEL,
  type Channel,
  channelId,
  channelList,
  channelQuery,
  inChannel,
  mailTargets,
  type SessionEntry,
} from './channels';
import { useMoreBelow } from './useMoreBelow';

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
  // The same icon as the needs-you strips of the fleet (TriangleAlert means a conflict there).
  approval: Shield,
  mail: Mail,
  lifecycle: Play,
} as const;

/** The icon of a tool row: the icon of its tool in the chat (DESIGN-SYSTEM §4.5). */
const TOOL_ICON: Readonly<Record<string, LucideIcon>> = {
  Read: FileText,
  Bash: SquareTerminal,
  Shell: SquareTerminal,
  shell: SquareTerminal,
  exec_command: SquareTerminal,
  Edit: Pencil,
  MultiEdit: Pencil,
  Write: Pencil,
  NotebookEdit: Pencil,
  apply_patch: Pencil,
  Grep: Search,
  Glob: Search,
  LS: Search,
  WebFetch: Globe,
  WebSearch: Globe,
  Task: Workflow,
  Agent: Workflow,
};

/** The tool name of a tool row as the deck wrote it ("Read: /a (ok)" gives "Read"). */
const toolNameOf = (text: string): string => /^([A-Za-z_][\w-]*)/.exec(text)?.[1] ?? '';

/**
 * An older deck sends no `request_kind`, and a question request is then an `approval` row that only
 * its text tells: "Question: …", the board's "Needs you: AskUserQuestion" (Claude) and the display
 * stream's "Needs you: Answer a question". The text is the fallback for such a row only.
 */
const QUESTION_TEXT = /^(?:Question:|Needs you: (?:AskUserQuestion|Answer a question)\b)/;

/** The icon of a row: a question has the message-square of the fleet strips, the Watch tiles and
 * the wall; a permission keeps the shield. The `request_kind` of the row decides; the text decides
 * only when the row has no `request_kind` (an older deck). A tool row has the icon of its tool in
 * the chat (an MCP tool the plug, any other tool the wrench). */
export function rowIcon(item: Pick<ActivityItem, 'kind' | 'text' | 'request_kind'>): LucideIcon {
  if (item.kind === 'tool') {
    const name = toolNameOf(item.text);
    return TOOL_ICON[name] ?? (name.startsWith('mcp__') ? Plug : Wrench);
  }
  return item.kind === 'approval' &&
    (item.request_kind === 'question' ||
      (item.request_kind === undefined && QUESTION_TEXT.test(item.text)))
    ? MessageSquare
    : KIND_ICON[item.kind];
}

/** The repository without its forge host: "github.com/lacion/fleet-deck" → "lacion/fleet-deck". A
 * first part without a dot is not a host and stays. */
const repoLabel = (repo: string) => repo.replace(/^[^/]*\.[^/]*\//, '');

/** The target of a tool row: a path loses its leading folders (`PathTail`), a command or an id is
 * mono, a description or a query is words. */
function Target({ tool }: { readonly tool: ActivityTool }) {
  if (tool.form === 'path') {
    return (
      <code className="fd-arow-target" data-path="">
        <PathTail path={homePath(tool.target)} />
      </code>
    );
  }
  if (tool.form === 'code') return <code className="fd-arow-target">{tool.target}</code>;
  return <span className="fd-arow-words">{tool.target}</span>;
}

/**
 * The text of a row. A tool or request row of a tool ("Read: src/a.ts (ok)", "Permission: Bash
 * cargo test") shows the tool name in weight 600 and its target (the look of a Watch tile and the
 * chat, §4.5): no colon, no "(ok)"; a failed or denied call ends in a hazard badge. Every other
 * text stays one string.
 */
export function RowText({ item }: { readonly item: ActivityItem }) {
  const tool = activityTool(item.kind, item.text);
  if (tool === null) return <span className="fd-arow-text">{item.text}</span>;
  return (
    <span className="fd-arow-text" data-tool="">
      {tool.lead === '' ? null : <span className="fd-arow-lead">{tool.lead}</span>}
      <b className="fd-arow-tool">{tool.name}</b>
      {tool.target === '' ? null : <Target tool={tool} />}
      {tool.outcome === null ? null : (
        <span className="fd-badge" data-tone="hazard">
          {tool.outcome === 'failed' ? 'Failed' : 'Denied'}
        </span>
      )}
    </span>
  );
}

/**
 * One event. The first row of a run of one session (`first`) names the session and its repository;
 * the rows after it leave the name to it (a phone shows the name as the head of the run). The whole
 * row opens the session: the session link covers it.
 */
function Row({
  item,
  first,
  now,
  stations,
}: {
  readonly item: ActivityItem;
  readonly first: boolean;
  readonly now: number;
  readonly stations: readonly StationView[];
}) {
  const href = formatHash({
    name: 'session',
    station: item.station_id,
    session: item.session_id,
    tab: 'chat',
  });
  const title = item.session_title ?? item.session_id;
  const outcome = activityTool(item.kind, item.text)?.outcome ?? null;
  return (
    <li
      className="fd-arow"
      data-kind={item.kind}
      data-run={first ? 'start' : undefined}
      data-outcome={outcome ?? undefined}
    >
      <span className="fd-arow-kind">
        <Icon of={rowIcon(item)} size={14} />
        <span className="fd-vh">{item.kind}</span>
      </span>
      <span className="fd-arow-head">
        <a className="fd-arow-session" href={href}>
          {first ? title : <span className="fd-vh">{title}</span>}
        </a>
        {first && item.repo !== null ? (
          <span
            className="fd-arow-repo"
            title={`${item.repo} on ${stationName(item.station_id, stations)}`}
          >
            {repoLabel(item.repo)}
          </span>
        ) : null}
      </span>
      <RowText item={item} />
      <time className="fd-arow-age" dateTime={new Date(item.at).toISOString()}>
        {relativeAge(item.at, now)}
      </time>
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
  side,
  on,
  onPick,
  children,
}: {
  readonly label: ReactNode;
  readonly count?: number | undefined;
  /** A quiet word at the right: the station or the age that tells a namesake apart. */
  readonly side?: string | undefined;
  readonly on: boolean;
  readonly onPick: () => void;
  readonly children?: ReactNode;
}) {
  return (
    <button type="button" className="fd-achan fd-selbar" aria-pressed={on} onClick={onPick}>
      {children}
      <span className="fd-achan-label">{label}</span>
      {count === undefined ? null : <span className="fd-count">{count}</span>}
      {side === undefined ? null : <span className="fd-achan-side">{side}</span>}
    </button>
  );
}

/** A session channel: its status glyph, its title (or its callsign) and what tells it from a
 * namesake. */
function SessionChannel({
  entry,
  on,
  onPick,
  now,
  stations,
}: {
  readonly entry: SessionEntry;
  readonly on: boolean;
  readonly onPick: () => void;
  readonly now: number;
  readonly stations: readonly StationView[];
}) {
  const side =
    entry.namesake === 'station'
      ? stationName(entry.stationId, stations)
      : entry.namesake === 'age'
        ? relativeAge(entry.updatedAt, now)
        : undefined;
  return (
    <ChannelButton
      label={entry.callsign ? <span className="fd-callsign">{entry.label}</span> : entry.label}
      side={side}
      on={on}
      onPick={onPick}
    >
      <span className="fd-dot" data-status={entry.status} aria-hidden="true" />
    </ChannelButton>
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
  const [endedOpen, setEndedOpen] = useState(false);
  const now = useNow(givenNow, 30_000);
  const list = channelList(sessions);
  const nav = useRef<HTMLElement>(null);
  // The query depends on the repository's origin; keep the latest sessions in a ref so a card
  // change does not restart the refresh timer.
  const latest = useRef(sessions);
  latest.current = sessions;
  const id = channelId(channel);
  const shown = inChannel(items ?? [], sessions, channel);
  // The Ended group stays open while its channel is the chosen one (the sidebar's rule).
  const endedShown = endedOpen || list.ended.some((e) => channelId(e.channel) === id);
  const more = useMoreBelow(nav, `${list.repos.length}/${list.live.length}/${endedShown}`);
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
  const sessionChannel = (e: SessionEntry) => (
    <SessionChannel
      key={channelId(e.channel)}
      entry={e}
      on={channelId(e.channel) === id}
      onPick={() => setChannel(e.channel)}
      now={now}
      stations={stations}
    />
  );

  return (
    <Page
      title="Activity"
      description="What the sessions did, newest first."
      className="fd-activity"
      wide
    >
      <div className="fd-awrap">
        <nav
          ref={nav}
          className="fd-achans"
          aria-label="Channels"
          data-more={more ? 'true' : undefined}
        >
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
          {list.live.length === 0 ? null : (
            <h2 className="fd-label">
              Live <span className="fd-count">{list.live.length}</span>
            </h2>
          )}
          {list.live.map(sessionChannel)}
          {list.ended.length === 0 ? null : (
            <button
              type="button"
              className="fd-achan-ended"
              aria-expanded={endedShown}
              onClick={() => setEndedOpen(!endedShown)}
            >
              <Icon of={ChevronRight} size={12} />
              Ended <span className="fd-count">{list.ended.length}</span>
            </button>
          )}
          {endedShown ? list.ended.map(sessionChannel) : null}
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
              {shown.map((item, n) => {
                const before = shown[n - 1];
                const first =
                  before === undefined ||
                  before.station_id !== item.station_id ||
                  before.session_id !== item.session_id;
                return (
                  <Row
                    key={`${item.station_id}/${item.session_id}/${item.station_seq ?? `mail${n}`}`}
                    item={item}
                    first={first}
                    now={now}
                    stations={stations}
                  />
                );
              })}
            </ol>
          )}
        </div>
      </div>
    </Page>
  );
}
