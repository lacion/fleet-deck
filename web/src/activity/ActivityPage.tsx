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
  CircleStop,
  CloudOff,
  FileText,
  Globe,
  Icon,
  type LucideIcon,
  Mail,
  MessageSquare,
  MessagesSquare,
  Moon,
  Pencil,
  Play,
  Plug,
  Search,
  Shield,
  SquareTerminal,
  Workflow,
  Wrench,
} from '../ui/icons';
import { homeShortIn, plainLine } from '../ui/format';
import { Loading } from '../ui/Loading';
import { EmptyState, ErrorNotice, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { foldLines } from '../watch/tiles';
import { type ActivityTool, activityTool, homePath, statusWord } from '../watch/tooltext';
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

/** The icon of a lifecycle row: a start plays, an end stops, a hibernation sleeps (the deck writes
 * "Session started", "Session ended…" and "Hibernated after…"). */
function lifecycleIcon(text: string): LucideIcon {
  if (text.startsWith('Session ended')) return CircleStop;
  if (text.startsWith('Hibernated')) return Moon;
  return Play;
}

/** The icon of a tool row: the icon of its tool in the chat (DESIGN-SYSTEM §4.5). */
const TOOL_ICON: Readonly<Record<string, LucideIcon>> = {
  Read: FileText,
  Bash: SquareTerminal,
  Shell: SquareTerminal,
  shell: SquareTerminal,
  exec_command: SquareTerminal,
  Monitor: SquareTerminal,
  BashOutput: SquareTerminal,
  KillShell: SquareTerminal,
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
  // A question has the message-square of the fleet strips, the Watch tiles and the wall.
  AskUserQuestion: MessageSquare,
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
    return (
      (Object.hasOwn(TOOL_ICON, name) ? TOOL_ICON[name] : undefined) ??
      (name.startsWith('mcp__') ? Plug : Wrench)
    );
  }
  if (item.kind === 'lifecycle') return lifecycleIcon(item.text);
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
  if (tool.form === 'code') {
    return <code className="fd-arow-target">{homeShortIn(tool.target)}</code>;
  }
  return <span className="fd-arow-words">{tool.target}</span>;
}

/** The lead of a request that waits for the owner, and of one that no longer does: the history
 * never claims an open request that does not exist. */
const NEEDS_LEAD = 'Needs you:';
const ASKED_LEAD = 'Asked:';

/** A request row of the deck: its `request_kind`, or (an older deck) its lead. */
const isRequest = (item: Pick<ActivityItem, 'kind' | 'text' | 'request_kind'>): boolean =>
  item.kind === 'approval' &&
  (item.request_kind !== undefined || /^(?:Needs you|Permission|Question): /.test(item.text));

/** The words of a row: a closed request says "Asked:", not "Needs you:". */
function words(text: string, open: boolean): string {
  return !open && (text === NEEDS_LEAD || text.startsWith(`${NEEDS_LEAD} `))
    ? `${ASKED_LEAD}${text.slice(NEEDS_LEAD.length)}`
    : text;
}

/**
 * The text of a row. A tool or request row of a tool ("Read: src/a.ts (ok)", "Permission: Bash
 * cargo test") shows the tool name in weight 600 and its target (the look of a Watch tile and the
 * chat, §4.5): no colon, no "(ok)"; a finished call with no target says "done" (a lone name read as
 * a heading). Every other text is one plain line: chat text loses its Markdown marks
 * (`plainLine`), so a message reads the same here as on a Watch tile and the wall. A request that
 * no longer waits (`open` false) leads with "Asked:".
 */
export function RowText({
  item,
  open = false,
}: {
  readonly item: ActivityItem;
  readonly open?: boolean;
}) {
  const tool = activityTool(item.kind, item.text);
  if (tool === null)
    return <span className="fd-arow-text">{words(plainLine(item.text), open)}</span>;
  const lead = words(tool.lead, open);
  const done = item.kind === 'tool' && tool.target === '' && tool.outcome === null;
  return (
    <span className="fd-arow-text" data-tool="">
      {lead === '' ? null : <span className="fd-arow-lead">{lead}</span>}
      <b className="fd-arow-tool">{tool.name}</b>
      {tool.target === '' ? null : <Target tool={tool} />}
      {done ? <span className="fd-arow-words">{statusWord('ok')}</span> : null}
    </span>
  );
}

/** The whole text of a row in one line, for its hover title (the row cuts it at its width). */
function fullText(item: ActivityItem, open: boolean): string {
  const tool = activityTool(item.kind, item.text);
  if (tool === null) return words(plainLine(item.text), open);
  const target = tool.form === 'path' ? homePath(tool.target) : homeShortIn(tool.target);
  return [words(tool.lead, open), tool.name, target].filter((part) => part !== '').join(' ');
}

/**
 * The open requests of the shown rows: for each session that needs the owner now, its newest
 * request row (the stream is newest first). Only such a row is yellow (principle 2); every other
 * request row is history.
 */
function openRequests(
  shown: readonly ActivityItem[],
  sessions: readonly DeckSession[],
): ReadonlySet<ActivityItem> {
  const waiting = new Set(
    sessions
      .filter((s) => s.status === 'needs_you')
      .map((s) => `${s.station_id}/${s.summary.session_id}`),
  );
  const seen = new Set<string>();
  const open = new Set<ActivityItem>();
  for (const item of shown) {
    if (item.kind !== 'approval') continue;
    const key = `${item.station_id}/${item.session_id}`;
    if (seen.has(key)) continue;
    seen.add(key);
    if (waiting.has(key) && isRequest(item)) open.add(item);
  }
  return open;
}

/** A row of the stream: one event, or the same event that came several times in a row. */
type Shown = ActivityItem & { readonly repeat?: number };

/**
 * The same event of one session several times in a row is one row with its count (`foldLines`),
 * the newest kept: four "Bash cargo test" rows are one row and a "4".
 */
function foldRows(items: readonly ActivityItem[]): Shown[] {
  const same = (a: Shown, b: Shown) =>
    a.station_id === b.station_id &&
    a.session_id === b.session_id &&
    a.kind === b.kind &&
    a.text === b.text;
  // `foldLines` keeps the last line of a run: the stream is newest first, so it folds reversed.
  return foldLines<Shown>([...items].reverse(), same).reverse();
}

/**
 * One event. The first row of a run of one session (`first`) names the session and, after it, its
 * repository; the rows after it leave the name to it (a phone shows the name as the head of the
 * run). The whole row opens the session: the session link covers it, and its title names the
 * session, the repository and the station, then the whole text. A failed or denied call ends in
 * its result at the right, the chat's result column (§4.5). An open request (`open`) has the
 * yellow icon of "needs you".
 */
function Row({
  item,
  first,
  open,
  now,
  stations,
}: {
  readonly item: Shown;
  readonly first: boolean;
  readonly open: boolean;
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
  const station = stationName(item.station_id, stations);
  const place =
    item.repo === null
      ? `${title} on ${station}`
      : `${title} · ${repoLabel(item.repo)} on ${station}`;
  const repeat = item.repeat ?? 1;
  return (
    <li
      className="fd-arow"
      data-kind={item.kind}
      data-run={first ? 'start' : undefined}
      data-open={open ? 'true' : undefined}
      data-outcome={outcome ?? undefined}
    >
      <span className="fd-arow-kind">
        <Icon of={rowIcon(item)} size={14} />
        <span className="fd-vh">{item.kind}</span>
      </span>
      <span className="fd-arow-head">
        <a className="fd-arow-session" href={href} title={`${place}\n${fullText(item, open)}`}>
          {first ? title : <span className="fd-vh">{title}</span>}
        </a>
        {first && item.repo !== null ? (
          <span className="fd-arow-repo">{` · ${repoLabel(item.repo)}`}</span>
        ) : null}
      </span>
      <RowText item={item} open={open} />
      {repeat < 2 ? null : (
        <span className="fd-count fd-arow-repeat">
          {repeat}
          <span className="fd-vh"> times</span>
        </span>
      )}
      {outcome === null ? null : <span className="fd-arow-result">{outcome}</span>}
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
  const shown = foldRows(inChannel(items ?? [], sessions, channel));
  const open = openRequests(shown, sessions);
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
          {list.repos.length === 0 ? null : (
            <h2 className="fd-label">
              Repositories <span className="fd-count">{list.repos.length}</span>
            </h2>
          )}
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
          {error === null ? null : <ErrorNotice text={error} />}
          {items === null ? (
            live ? (
              <Loading text="Loading…" />
            ) : (
              <EmptyState
                icon={CloudOff}
                title="Offline"
                body="The stream needs a live connection."
              />
            )
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
                // A start, an end and a request always name their session: they start a run.
                const first =
                  before === undefined ||
                  before.station_id !== item.station_id ||
                  before.session_id !== item.session_id ||
                  item.kind === 'lifecycle' ||
                  item.kind === 'approval';
                return (
                  <Row
                    key={`${item.station_id}/${item.session_id}/${item.station_seq ?? `mail${n}`}`}
                    item={item}
                    first={first}
                    open={open.has(item)}
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
