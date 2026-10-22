// R6.2 (DL-169): `#/watch`, the first explored form of "watch many sessions" (DL-157): a wall of
// compact live tiles, one per live session, and a Focus mode that cycles through the sessions
// that need you. The page is isolated: no other screen imports it.
import { type FocusEvent, useEffect, useMemo, useState } from 'react';
import { formatHash } from '../app/routes';
import { basename } from '../app/sessions';
import { repoName, stationName } from '../board/fleet';
import type { ApprovalOption, DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import { optionClass as approvalClass } from '../session/ApprovalDock';
import type { SessionActions } from '../session/commands';
import { Eye, Icon, MessageSquare, Shield } from '../ui/icons';
import { EmptyState, Page } from '../ui/page';
import { FocusLine, focusOf, UnreadMark, unreadOf } from '../views/marks';
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
  watchSessions,
} from './tiles';

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
};

/** The look of an answer: the ApprovalDock rule, at the small size. */
const answerClass = (option: ApprovalOption, n: number) =>
  [approvalClass(option, n), 'fd-sm'].filter(Boolean).join(' ');

const timelineOf = (timelines: Props['timelines'], key: SessionKey): Timeline | undefined =>
  timelines?.[JSON.stringify([key.station_id, key.session_id])];

/**
 * A tool call on one line (DESIGN-SYSTEM §4.1, §4.5): the tool name in sans weight 600, the
 * target in mono, then a plain note ("(failed)", a subagent title).
 */
function ToolCall({
  name,
  target,
  note,
}: {
  readonly name: string;
  readonly target: string;
  readonly note: string;
}) {
  return (
    <>
      <b className="fd-wtile-toolname">{name}</b>
      {target === '' ? null : (
        <>
          {' '}
          <code>{target}</code>
        </>
      )}
      {note === '' ? null : ` ${note}`}
    </>
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
}) {
  const s = card.summary;
  const key: SessionKey = { station_id: card.station_id, session_id: s.session_id };
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  // A wall display gets its own event type (R13 §5D item 7).
  const shown = timeline?.display ?? [];
  const lines = shown.length > 0 ? displayTileLines(shown) : tileLines(timeline?.events ?? []);
  const tool = s.running_tools[0];
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
  const where = [stationName(card.station_id, stations), repoName(card)].join(' · ');
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
      <span className="fd-wtile-head">
        <span className="fd-dot" data-status={card.status} aria-hidden="true" />
        <span className="fd-wtile-title">{s.title ?? s.callsign ?? s.session_id}</span>
        <UnreadMark count={unreadOf(card)} />
      </span>
      <span className="fd-wtile-where">
        {where}
        {s.branch !== null && s.branch !== '' ? (
          <>
            {' · '}
            <code>{s.branch}</code>
          </>
        ) : s.cwd != null ? (
          <>
            {' · '}
            <code>{basename(s.cwd)}</code>
          </>
        ) : null}
      </span>
      <FocusLine summary={s} />
      {unavailable ? (
        <span className="fd-wtile-empty">Unavailable</span>
      ) : lines.length === 0 ? (
        // The empty state of a tile: a loaded tile without chat says so, unlike a broken one. A
        // running tool or a focus line already shows what the session does (principle 7).
        timeline?.ready !== true ? (
          <span className="fd-wtile-empty">Loading…</span>
        ) : tool === undefined && focusOf(s) === null ? (
          <span className="fd-wtile-empty">No chat yet.</span>
        ) : null
      ) : (
        <span className="fd-wtile-lines">
          {lines.map((l) => (
            <span key={l.key} className="fd-wtile-line" data-kind={l.kind}>
              {l.name === undefined ? (
                l.text
              ) : (
                <ToolCall name={l.name} target={l.target ?? ''} note={l.note ?? ''} />
              )}
            </span>
          ))}
        </span>
      )}
      {tool === undefined ? null : (
        <span className="fd-wtile-tool" title="Running now">
          <ToolCall name={tool.title} target={tool.input_summary} note="" />
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
}: Props) {
  const [filter, setFilter] = useState<WatchFilter>(initialFilter);
  const [focusOn, setFocusOn] = useState(initialFocus);
  const [focused, setFocused] = useState<string | null>(initialFocused);
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
      <span className="fd-vh">{label}</span>
      <select value={value} aria-label={label} onChange={(e) => set(e.target.value)}>
        <option value={ALL}>{all}</option>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
  const needs = matching.filter((c) => c.status === 'needs_you').length;
  // The header sentence (DESIGN-SYSTEM §4.3): the live count, then who needs you in --act.
  const summary = (
    <span aria-live="polite">
      {matching.length === 1 ? '1 live session' : `${matching.length} live sessions`}.{' '}
      {needs === 0 ? (
        'All clear.'
      ) : (
        <>
          <span className="fd-watch-needs">
            {needs === 1 ? '1 needs you' : `${needs} need you`}
          </span>
          .
        </>
      )}
    </span>
  );
  const filters = (
    <>
      {select(
        'Station',
        filter.station,
        'All stations',
        choices.stations.map((id) => ({ value: id, label: stationName(id, stations) })),
        (station) => setFilter({ ...filter, station }),
      )}
      {select(
        'Repository',
        filter.repo,
        'All repositories',
        choices.repos.map((r) => ({ value: r, label: r })),
        (repo) => setFilter({ ...filter, repo }),
      )}
      <button
        type="button"
        className="fd-chip"
        aria-pressed={filter.needsOnly}
        onClick={() => setFilter({ ...filter, needsOnly: !filter.needsOnly })}
      >
        Needs you only
      </button>
      <button
        type="button"
        className="fd-chip"
        aria-pressed={focusOn}
        title={`Cycles every ${FOCUS_MS / 1000} s through the sessions that need you`}
        onClick={() => setFocusOn(!focusOn)}
      >
        Focus
      </button>
      {paused ? (
        <span className="fd-watch-paused" role="status">
          Paused
        </span>
      ) : null}
    </>
  );
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
        <ul className="fd-wall" data-focus={focusOn ? 'true' : undefined}>
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
            />
          ))}
        </ul>
      )}
    </Page>
  );
}
