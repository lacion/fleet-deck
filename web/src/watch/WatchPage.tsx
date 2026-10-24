// R6.2 (DL-169): `#/watch`, the first explored form of "watch many sessions" (DL-157): a wall of
// compact live tiles, one per live session, and a Focus mode that cycles through the sessions
// that need you. The page is isolated: no other screen imports it.
import {
  type CSSProperties,
  type FocusEvent,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
} from 'react';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import { useLayout } from '../app/viewport';
import { atWork } from '../board/agentCounts';
import { NO_REPO, repoName, stationName } from '../board/fleet';
import { PathTail } from '../chat/PathTail';
import type { ApprovalOption, DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import { optionClass as approvalClass } from '../session/ApprovalDock';
import type { SessionActions } from '../session/commands';
import {
  CircleDot,
  Eye,
  Folder,
  GitBranch,
  Icon,
  ListFilter,
  MessageSquare,
  Shield,
} from '../ui/icons';
import { Menu } from '../ui/Menu';
import { EmptyState, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { FleetSentence } from '../views/FleetSentence';
import { FocusLine, focusOf, UnreadMark, unreadOf } from '../views/marks';
import { StatusSeg, statusMatches } from '../views/StatusSeg';
import { displayTileLines } from '../wall/displayLines';
import {
  ALL,
  emptyWatchFilter,
  FOCUS_MS,
  filterChoices,
  focusOrder,
  MAX_TILES,
  nextFocus,
  refOf,
  tileLines,
  tileRequest,
  type WatchFilter,
  watchColumns,
  watchSessions,
  withoutCall,
} from './tiles';
import { homePath, type TargetForm, toolText, toolWords } from './tooltext';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly timelines?: Readonly<Record<string, Timeline>> | undefined;
  /** Opens the live timeline of a session; returns the cancel function. */
  readonly watch?: ((key: SessionKey) => () => void) | undefined;
  readonly sessionActions?: ((key: SessionKey) => SessionActions) | undefined;
  /** Focus is on at the first render (tests). */
  readonly initialFocus?: boolean;
  /** The focused session at the first render (tests). */
  readonly initialFocused?: string | null;
  /** The filter at the first render (tests). */
  readonly initialFilter?: WatchFilter;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
};

/** The look of an answer: the ApprovalDock rule, at the small size. */
const answerClass = (option: ApprovalOption, n: number) =>
  [approvalClass(option, n), 'fd-sm'].filter(Boolean).join(' ');

const timelineOf = (timelines: Props['timelines'], key: SessionKey): Timeline | undefined =>
  timelines?.[JSON.stringify([key.station_id, key.session_id])];

/**
 * A tool call on one line (DESIGN-SYSTEM §4.1, §4.5): the tool name in sans weight 600, the
 * target (a path keeps its file name: `PathTail` drops the leading folders; a command in mono; a
 * description in words), then a plain note (a subagent title) and a failed or denied mark.
 */
function ToolCall({
  name,
  target,
  form = 'code',
  note = '',
  outcome,
}: {
  readonly name: string;
  readonly target: string;
  readonly form?: TargetForm | undefined;
  readonly note?: string | undefined;
  readonly outcome?: 'failed' | 'denied' | undefined;
}) {
  return (
    <>
      <b className="fd-wtile-toolname">{name}</b>
      {target === '' ? null : form === 'path' ? (
        <code className="fd-wtile-path">
          <PathTail path={homePath(target)} />
        </code>
      ) : form === 'words' ? (
        <span className="fd-wtile-words">{target}</span>
      ) : (
        <code>{target}</code>
      )}
      {note === '' ? null : <span className="fd-wtile-words">{note}</span>}
      {outcome === undefined ? null : <span className="fd-wtile-fail">{outcome}</span>}
    </>
  );
}

/** A line that came several times in a row ends in its count (`foldLines`): "3" reads "3 times". */
function Repeat({ n }: { readonly n: number | undefined }) {
  if (n === undefined || n < 2) return null;
  return (
    <span className="fd-count fd-wtile-repeat">
      {n}
      <span className="fd-vh"> times</span>
    </span>
  );
}

/** The fewest characters of a branch or folder that a tile keeps before it cuts the name. */
const BRANCH_MIN_CH = 18;

/** Where a tile works: the station (never cut), the repository (none outside a repository:
 * principle 7), then the branch or the folder with its icon (the strip's rule). A short line cuts
 * the repository first: the branch keeps its first 18 characters (views.css). Each part that can
 * be cut has its whole text in its title. */
function Where({
  card,
  stations,
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
}) {
  const s = card.summary;
  const repo = repoName(card);
  const place =
    s.branch !== null && s.branch !== ''
      ? { text: s.branch, icon: GitBranch, label: 'Branch' }
      : s.cwd !== null && s.cwd !== ''
        ? { text: basename(s.cwd), icon: Folder, label: 'Folder' }
        : null;
  // The branch keeps at least its first 18 characters, a shorter name all of its own.
  const keep = place === null ? 0 : Math.min(BRANCH_MIN_CH, [...place.text].length);
  return (
    <span className="fd-wtile-where">
      <span className="fd-wtile-station">{stationName(card.station_id, stations)}</span>
      {repo === NO_REPO ? null : (
        <span className="fd-wtile-place" title={repo}>
          <span aria-hidden="true">· </span>
          {repo}
        </span>
      )}
      {place === null ? null : (
        <span
          className="fd-wtile-branch"
          title={`${place.label}: ${place.text}`}
          style={{ '--fd-wtile-keep': `${keep}ch` } as CSSProperties}
        >
          <Icon of={place.icon} size={12} />
          <span className="fd-vh">{place.label}</span>
          <code>{place.text}</code>
        </span>
      )}
    </span>
  );
}

/** The handlers that pause Focus while the pointer or the keyboard is in the tile. */
type TileHold = {
  readonly onPointerEnter: () => void;
  readonly onPointerLeave: () => void;
  readonly onFocus: () => void;
  readonly onBlur: (event: FocusEvent<HTMLElement>) => void;
};

/**
 * One tile. With `readOnly` (the wall, R13 P1) it has no answer button and no other control: the
 * tile shows the request as text, and it is a focus target for the keyboard instead of a link.
 */
export function Tile({
  card,
  stations,
  timeline,
  actions,
  freshness,
  focused,
  readOnly = false,
  unavailable = false,
  hold,
  now: givenNow,
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly actions: SessionActions | undefined;
  readonly freshness: Freshness;
  readonly focused: boolean;
  readonly readOnly?: boolean;
  /** The timeline subscription of the tile failed (the wall, R13 P1). */
  readonly unavailable?: boolean;
  /** Focus mode: the tile tells the page while the pointer or the keyboard is in it. */
  readonly hold?: TileHold | undefined;
  /** Unix ms of "now" (the page's clock); absent: the tile keeps its own. */
  readonly now?: number | undefined;
}) {
  const now = useNow(givenNow, 30_000);
  const s = card.summary;
  const key: SessionKey = { station_id: card.station_id, session_id: s.session_id };
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  // A wall display gets its own event type (R13 §5D item 7).
  const shown = timeline?.display ?? [];
  const tool = s.running_tools[0];
  // The call that runs now has its own line under the chat: its chat line folds into it.
  const all = shown.length > 0 ? displayTileLines(shown) : tileLines(timeline?.events ?? []);
  const lines = tool === undefined ? all : withoutCall(all, tool.node_id);
  const ask = tileRequest(card);
  const answerable =
    !readOnly &&
    ask?.plain === true &&
    s.capabilities?.approvals === true &&
    actions?.canApprove === true &&
    freshness === 'live';
  const href = formatHash({
    name: 'session',
    station: key.station_id,
    session: key.session_id,
    tab: 'chat',
  });
  const running =
    tool === undefined ? null : toolText(tool.title, tool.input_summary, { kind: tool.tool_kind });
  // The tile line is short: an MCP tool has no server there; the title keeps the whole name.
  const runningName = tool === undefined ? '' : toolWords(tool.title);
  // The call that runs now: the working glyph and its name tell it from the past calls.
  const runTitle = running === null || running.name === runningName ? undefined : running.name;
  const run =
    running === null ? null : (
      <>
        <span className="fd-dot" data-status="working" aria-hidden="true" />
        <span className="fd-vh">Running now: </span>
        <ToolCall
          name={runningName}
          target={running.target}
          form={running.form}
          note={running.target === '' ? 'running' : ''}
        />
      </>
    );
  // On the wall it is the newest chat line: the room that no whole line fills falls at the tile
  // foot (wall.css), never between the chat and the call. A tile that loads or failed keeps its
  // word, and the call under it.
  const runInLines =
    readOnly && run !== null && !unavailable && (lines.length > 0 || timeline?.ready === true);
  const answer = (requestId: string, optionId: string) => {
    if (actions === undefined) return;
    setBusy(true);
    setNote(null);
    actions.answer(requestId, optionId).then(
      (o) => {
        setBusy(false);
        setNote(o.ok ? null : o.text);
      },
      (e: unknown) => {
        setBusy(false);
        setNote(e instanceof Error ? e.message : 'The request failed.');
      },
    );
  };
  const body = (
    <>
      {/* The age at the right, as on every strip and sidebar row: a stale tile does not look
          like a busy one. */}
      <span className="fd-wtile-head">
        <span className="fd-dot" data-status={card.status} aria-hidden="true" />
        <span className="fd-wtile-title">{s.title ?? s.callsign ?? s.session_id}</span>
        <UnreadMark count={unreadOf(card)} />
        <time className="fd-wtile-age" dateTime={new Date(s.updated_at).toISOString()}>
          {relativeAge(s.updated_at, now)}
        </time>
      </span>
      <Where card={card} stations={stations} />
      <FocusLine summary={s} />
      {unavailable ? (
        <span className="fd-wtile-empty">Unavailable</span>
      ) : lines.length === 0 && !runInLines ? (
        // The empty state of a tile: a loaded tile without chat says so, unlike a broken one. A
        // running tool or a focus line already shows what the session does (principle 7).
        timeline?.ready !== true ? (
          <span className="fd-wtile-empty">Loading…</span>
        ) : tool === undefined && focusOf(s) === null ? (
          <span className="fd-wtile-empty">No chat yet.</span>
        ) : null
      ) : (
        <span className="fd-wtile-lines">
          {lines.map((l) => {
            const repeated = l.repeat !== undefined && l.repeat > 1;
            return (
              <span
                key={l.key}
                className="fd-wtile-line"
                data-kind={l.kind}
                data-repeat={repeated ? '' : undefined}
                title={l.label}
              >
                {l.name !== undefined ? (
                  <ToolCall
                    name={l.name}
                    target={l.target ?? ''}
                    form={l.form}
                    note={l.note}
                    outcome={l.outcome}
                  />
                ) : repeated ? (
                  <span className="fd-wtile-text">{l.text}</span>
                ) : (
                  l.text
                )}
                <Repeat n={l.repeat} />
              </span>
            );
          })}
          {runInLines ? (
            <span className="fd-wtile-line" data-kind="tool" data-running="" title={runTitle}>
              {run}
            </span>
          ) : null}
        </span>
      )}
      {run === null || runInLines ? null : (
        <span className="fd-wtile-tool" title={runTitle}>
          {run}
        </span>
      )}
    </>
  );
  return (
    <li
      className={card.status === 'needs_you' ? 'fd-wtile fd-glow' : 'fd-wtile'}
      data-status={card.status}
      data-focused={focused ? 'true' : undefined}
      data-session={s.session_id}
      data-ref={readOnly ? `${card.station_id}/${s.session_id}` : undefined}
      tabIndex={readOnly ? 0 : undefined}
      {...hold}
    >
      <span className="fd-holder" data-status={card.status} aria-hidden="true" />
      {readOnly ? (
        <div className="fd-wtile-open">{body}</div>
      ) : (
        <a className="fd-wtile-open" href={href}>
          {body}
        </a>
      )}
      {ask === null ? null : (
        <div className="fd-wtile-ask" role="group" aria-label={`Needs you: ${ask.request.title}`}>
          <span className="fd-card-ask">
            <Icon of={ask.request.request_kind === 'question' ? MessageSquare : Shield} size={12} />{' '}
            {ask.request.title}
          </span>
          {answerable ? (
            <span className="fd-options">
              {ask.request.options.map((o, n) => (
                <button
                  key={o.option_id}
                  type="button"
                  className={answerClass(o, n)}
                  disabled={busy}
                  aria-label={`${o.label}: ${ask.request.title} in ${s.title ?? s.session_id}`}
                  onClick={() => answer(ask.request.request_id, o.option_id)}
                >
                  {o.label}
                </button>
              ))}
            </span>
          ) : readOnly ? null : (
            <span className="fd-wtile-note">Open the session to answer.</span>
          )}
          {note === null ? null : (
            <span className="fd-wtile-note" role="alert">
              {note}
            </span>
          )}
        </div>
      )}
    </li>
  );
}

/** The width of an element, kept current while it resizes; `null` before the first measure. */
function useWidth(element: HTMLElement | null): number | null {
  const [width, setWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (element === null || typeof ResizeObserver === 'undefined') return undefined;
    const measure = () => setWidth(element.clientWidth);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [element]);
  return width;
}

/** The watch page: filters, the Focus toggle and the wall. */
export function WatchPage({
  sessions,
  stations,
  freshness,
  timelines,
  watch,
  sessionActions,
  initialFocus = false,
  initialFocused = null,
  initialFilter = emptyWatchFilter,
  now: givenNow,
}: Props) {
  const [filter, setFilter] = useState<WatchFilter>(initialFilter);
  const [focusOn, setFocusOn] = useState(initialFocus);
  const [focused, setFocused] = useState<string | null>(initialFocused);
  const phone = useLayout() === 'phone';
  const now = useNow(givenNow, 30_000);
  // The wall balances its rows (`watchColumns`): it measures its own width.
  const [wall, setWall] = useState<HTMLUListElement | null>(null);
  const wallWidth = useWidth(wall);
  // Focus waits while the pointer or the keyboard is in the shown tile (WCAG 2.2.1, 2.2.2).
  const [hovered, setHovered] = useState(false);
  const [focusWithin, setFocusWithin] = useState(false);
  const paused = focusOn && (hovered || focusWithin);
  const choices = filterChoices(sessions);
  const matching = watchSessions(sessions, filter);
  const shown = matching.slice(0, MAX_TILES);
  const order = focusOrder(sessions, filter);
  const orderKey = order.join('\n');
  // Focus stays on one session while it needs you; the first one starts it.
  const current = focusOn
    ? focused !== null && order.includes(focused)
      ? focused
      : (order[0] ?? null)
    : null;
  useEffect(() => {
    if (!focusOn) return undefined;
    setFocused((now) => (now !== null && order.includes(now) ? now : (order[0] ?? null)));
    if (order.length < 2 || paused) return undefined;
    const timer = setInterval(() => setFocused((now) => nextFocus(order, now)), FOCUS_MS);
    return () => clearInterval(timer);
    // `orderKey` stands for the content of `order`.
  }, [focusOn, orderKey, paused]);
  // A removed tile sends no leave or blur event: a new shown tile (or none) ends the pause. The
  // pointer pauses again when it moves on the new tile.
  const shownRef = focusOn ? current : null;
  useEffect(() => {
    if (shownRef === null) return;
    return () => {
      setHovered(false);
      setFocusWithin(false);
    };
  }, [shownRef]);
  const hold: TileHold = {
    onPointerEnter: () => setHovered(true),
    onPointerLeave: () => setHovered(false),
    onFocus: () => setFocusWithin(true),
    onBlur: (event) => {
      const next = event.relatedTarget;
      if (!(next instanceof Node) || !event.currentTarget.contains(next)) setFocusWithin(false);
    },
  };

  // Focus picks from every matching session; the tile limit is for the wall only.
  const visible = focusOn ? matching.filter((c) => refOf(c) === current) : shown;
  const refs = visible.map(refOf).join('\n');
  const keys = useMemo(
    () =>
      visible.map(
        (c): SessionKey => ({ station_id: c.station_id, session_id: c.summary.session_id }),
      ),
    // `refs` stands for the content of `visible`.
    [refs],
  );
  useEffect(() => {
    if (watch === undefined) return undefined;
    const cancels: (() => void)[] = [];
    for (const key of keys) {
      try {
        cancels.push(watch(key));
      } catch {
        // More timelines than the socket allows: the tile shows no chat lines.
      }
    }
    return () => {
      for (const cancel of cancels) cancel();
    };
  }, [watch, keys]);

  const select = (
    label: string,
    value: string,
    all: string,
    options: readonly { value: string; label: string }[],
    set: (v: string) => void,
  ) => (
    <label className="fd-wfilter">
      <span className={phone ? 'fd-label' : 'fd-vh'}>{label}</span>
      {/* A view option of the header (§4.3): a ghost select; a chosen filter shows it is on. The
          phone menu has the plain select of a form. */}
      <select
        className={phone ? undefined : 'fd-ghost fd-sm'}
        data-active={!phone && value !== ALL ? 'true' : undefined}
        value={value}
        aria-label={label}
        onChange={(e) => set(e.target.value)}
      >
        <option value={ALL}>{all}</option>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
  // The header sentence (DESIGN-SYSTEM §4.3, §4.4): the fleet's sentence, for the tiles that match.
  const needs = matching.filter((c) => statusMatches(c, 'needs_you')).length;
  const onStations = new Set(matching.map((c) => c.station_id)).size;
  const summary = (
    <span aria-live="polite">
      <FleetSentence
        live={matching.length}
        stations={onStations}
        needsYou={needs}
        atWork={atWork(matching)}
      />
    </span>
  );
  const stationSelect = select(
    'Station',
    filter.station,
    'All stations',
    choices.stations.map((id) => ({ value: id, label: stationName(id, stations) })),
    (station) => setFilter({ ...filter, station }),
  );
  const repoSelect = select(
    'Repository',
    filter.repo,
    'All repositories',
    choices.repos.map((r) => ({ value: r, label: r })),
    (repo) => setFilter({ ...filter, repo }),
  );
  // The status filter is the fleet's segmented control (one control for one filter, §4.3).
  const status = (
    <StatusSeg
      value={filter.status}
      onChange={(next) => setFilter({ ...filter, status: next })}
      className="fd-watch-status"
    />
  );
  const focusTitle = `Cycles every ${FOCUS_MS / 1000} s through the sessions that need you`;
  const pausedNote = paused ? (
    <span className="fd-watch-paused" role="status">
      Paused
    </span>
  ) : null;
  const active =
    (filter.station === ALL ? 0 : 1) +
    (filter.repo === ALL ? 0 : 1) +
    (filter.status === 'all' ? 0 : 1) +
    (focusOn ? 1 : 0);
  // A phone (§4.8): one Filters button, the fleet's pattern; its menu names each control. A
  // desktop: the selects, the status segments and the Focus toggle in the header row (§4.3).
  const filters = phone ? (
    <>
      <span className="fd-watch-filtermenu">
        <Menu
          summary={
            <>
              <Icon of={ListFilter} size={14} />
              Filters
              {active > 0 ? (
                <>
                  {' '}
                  <span className="fd-count">{active}</span>
                </>
              ) : null}
            </>
          }
          summaryClass="fd-ghost fd-sm"
        >
          {() => (
            <div className="fd-watch-filtergrid" role="group" aria-label="Watch filters">
              {stationSelect}
              {repoSelect}
              <div className="fd-wfilter">
                <span className="fd-label" aria-hidden="true">
                  Status
                </span>
                {status}
              </div>
              <label className="fd-menu-switch" title={focusTitle}>
                <span className="fd-menu-switch-label">
                  <Icon of={CircleDot} size={14} />
                  Focus
                </span>
                <input
                  type="checkbox"
                  role="switch"
                  className="fd-switch"
                  aria-checked={focusOn}
                  checked={focusOn}
                  onChange={(e) => setFocusOn(e.target.checked)}
                />
              </label>
            </div>
          )}
        </Menu>
      </span>
      {pausedNote}
    </>
  ) : (
    <>
      {/* One order on every page header (the fleet's): the status segments, the filters, then
          the view toggles. */}
      {status}
      {stationSelect}
      {repoSelect}
      {/* A ghost toggle with an icon (the header's one control language, §4.3): pressed, it takes
          the pressed-segment look of base.css. */}
      <button
        type="button"
        className="fd-ghost fd-sm"
        aria-pressed={focusOn}
        title={focusTitle}
        onClick={() => setFocusOn(!focusOn)}
      >
        <Icon of={CircleDot} size={14} /> Focus
      </button>
      {pausedNote}
    </>
  );
  const columns = focusOn || wallWidth === null ? null : watchColumns(visible.length, wallWidth);
  return (
    <Page title="Watch" className="fd-watch" description={summary} filters={filters} wide>
      {matching.length > MAX_TILES && !focusOn ? (
        <p className="fd-note">
          Showing {MAX_TILES} of {matching.length} sessions. Filter to see the others.
        </p>
      ) : null}
      {focusOn && current === null ? (
        <EmptyState
          icon={Eye}
          title="Nothing needs you right now"
          body="Focus waits for the next request."
        />
      ) : visible.length === 0 ? (
        <EmptyState
          icon={Eye}
          title={sessions.length === 0 ? 'No sessions yet' : 'No live session matches'}
          body={
            sessions.length === 0
              ? 'A live session shows here as a tile.'
              : 'Change a filter to see more.'
          }
        />
      ) : (
        <ul
          ref={setWall}
          className="fd-wall"
          data-focus={focusOn ? 'true' : undefined}
          data-cols={columns === null ? undefined : columns}
          style={columns === null ? undefined : ({ '--fd-watch-cols': columns } as CSSProperties)}
        >
          {visible.map((card) => (
            <Tile
              key={refOf(card)}
              card={card}
              stations={stations}
              timeline={timelineOf(timelines, {
                station_id: card.station_id,
                session_id: card.summary.session_id,
              })}
              actions={sessionActions?.({
                station_id: card.station_id,
                session_id: card.summary.session_id,
              })}
              freshness={freshness}
              focused={focusOn}
              hold={focusOn ? hold : undefined}
              now={now}
            />
          ))}
        </ul>
      )}
    </Page>
  );
}
