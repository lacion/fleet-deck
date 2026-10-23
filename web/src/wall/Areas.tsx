// R13 P1 (design §4.2): the four areas of the mission mode: needs, tiles, side and stream.
// Everything here only reads. A part with no data is left out.
import { type CSSProperties, Fragment, type RefObject, useEffect, useMemo, useState } from 'react';
import type { WallRoute } from '../app/routes';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import { rateMeters, resetsText, tightest, WARN_PERCENT } from '../board/usage';
import type { ActivityItem, DeckSession, StationView } from '../gen';
import { prChipTitle, prStateText } from '../integrations/PrChip';
import { usePrs, useProjects } from '../integrations/context';
import { prOfSession } from '../integrations/model';
import type { Freshness, Timeline } from '../runtime/state';
import type { ViewsActions } from '../runtime/views';
import { agentLine } from '../chat/notices';
import { shortPath } from '../chat/PathTail';
import { homeShortIn, plainLine } from '../ui/format';
import { activityTool, homePath } from '../watch/tooltext';
import { Tile } from '../watch/WatchPage';
import { foldLines, refOf } from '../watch/tiles';
import { keyOf, timelineOf } from './hooks';
import { LoadMap } from './LoadMap';
import { loadMap } from './loadCells';
import { ALL_CLEAR, NEED_LABEL, type Need, oneLine, usageParts, wallSessions } from './model';
import { QrCode } from './QrCode';
import { answerUrl } from './qr';
import { stationStats } from './stationStats';

export function NeedsArea({
  needs,
  stations,
  origin,
  now,
}: {
  readonly needs: readonly Need[];
  readonly stations: readonly StationView[];
  readonly origin: string;
  readonly now: number;
}) {
  return (
    <section className="fd-wall-area" data-area="needs" aria-label="Needs you">
      <h2 className="fd-wall-h">
        Needs you
        {needs.length === 0 ? null : (
          <span className="fd-count" data-tone="act">
            {needs.length}
          </span>
        )}
      </h2>
      {needs.length === 0 ? (
        // The calm state of the fleet and Watch headers (§4.4): green, the same words. The wall
        // shows no conflict, so it never needs the "Nothing needs you." of a fleet with one.
        <p className="fd-wall-note fd-wall-clear">{ALL_CLEAR}</p>
      ) : (
        <ol className="fd-wall-needs">
          {needs.map(({ card, request }) => {
            const s = card.summary;
            return (
              <li
                key={`${refOf(card)}/${request.request_id}`}
                className="fd-wall-need fd-glow"
                data-kind={request.request_kind}
              >
                <span className="fd-holder" data-status="needs_you" aria-hidden="true" />
                <div className="fd-wall-need-text">
                  <p className="fd-wall-need-head">
                    <strong className="fd-callsign">{s.callsign ?? s.title ?? s.session_id}</strong>{' '}
                    <span className="fd-wall-need-where">
                      {stationName(card.station_id, stations)}
                    </span>{' '}
                    <span className="fd-badge" data-tone="act">
                      {NEED_LABEL[request.request_kind] ?? 'Request'}
                    </span>
                  </p>
                  <p className="fd-wall-need-what">
                    <span className="fd-wall-need-q">{request.title}</span>
                  </p>
                  {request.details.trim() === '' ? null : (
                    <p className="fd-wall-need-sum">{oneLine(request.details, 240)}</p>
                  )}
                  <p className="fd-wall-note fd-wall-need-age">
                    <time dateTime={new Date(request.requested_at).toISOString()}>
                      {relativeAge(request.requested_at, now)}
                    </time>
                  </p>
                </div>
                <div className="fd-wall-need-qr">
                  <QrCode
                    url={answerUrl(origin, card.station_id, s.session_id, request.request_id)}
                  />
                  <p className="fd-wall-note">Scan to answer on your phone</p>
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}

export function TilesArea({
  cards,
  stations,
  timelines,
  freshness,
  failed,
  areaRef,
  grid = null,
}: {
  readonly cards: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly timelines: Readonly<Record<string, Timeline>> | undefined;
  readonly freshness: Freshness;
  readonly failed: ReadonlySet<string>;
  readonly areaRef: RefObject<HTMLElement | null>;
  /** The columns of the tiles (`tileGrid`); `null` before a measure. The growth of the type,
   * `--fd-wall-tile-k`, is set on the grid: the needs and the side areas grow with the tiles. */
  readonly grid?: { readonly columns: number; readonly k: number } | null;
}) {
  const style: CSSProperties | undefined =
    cards.length === 0
      ? { display: 'none' }
      : grid === null
        ? undefined
        : ({ '--fd-wall-tile-cols': grid.columns } as CSSProperties);
  return (
    <section className="fd-wall-area" data-area="tiles" aria-label="Sessions" ref={areaRef}>
      {cards.length === 0 ? <p className="fd-wall-note">No live session.</p> : null}
      {/* Always rendered: the wall reads the grid gap from it to count the tiles that fit. */}
      <ul className="fd-wall-tiles" data-grid={grid === null ? undefined : 'fill'} style={style}>
        {cards.map((card) => {
          const timeline = timelineOf(timelines, keyOf(card));
          return (
            <Tile
              key={refOf(card)}
              card={card}
              stations={stations}
              timeline={timeline}
              actions={undefined}
              freshness={freshness}
              focused={false}
              readOnly
              unavailable={failed.has(refOf(card)) || timeline?.error != null}
            />
          );
        })}
      </ul>
    </section>
  );
}

function StationRow({ station }: { readonly station: StationView }) {
  // A wall display gets `display_resources` (no disk label, no path) in place of `info`. The wall
  // shows the CPU, the memory and the fullest disk: a short line that a person reads across a room.
  const stats = stationStats(station.info?.resources ?? station.display_resources);
  return (
    <li className="fd-wall-station" data-reachable={station.reachable ? 'true' : 'false'}>
      {/* The reach glyph and the words of the board (fleet lanes, Stations page). */}
      <span
        className="fd-dot"
        data-status={station.reachable ? 'online' : 'unreachable'}
        aria-hidden="true"
      />
      <span className="fd-wall-station-name">{station.name}</span>
      {/* The glyph says "online" (§4.7, the station identity): the word is for a screen reader
          only. "Offline" shows, in --hazard. */}
      {station.reachable ? (
        <span className="fd-vh"> online</span>
      ) : (
        <span className="fd-wall-note">Offline</span>
      )}
      {/* A part keeps its words and its middle dot together: a second line starts with a part. */}
      {stats.length === 0 ? null : (
        <span className="fd-wall-station-stats">
          {stats.map((part, n) => (
            <Fragment key={part}>
              {n === 0 ? null : ' '}
              <span className="fd-wall-part">{n < stats.length - 1 ? `${part} ·` : part}</span>
            </Fragment>
          ))}
        </span>
      )}
    </li>
  );
}

export function SideArea({
  sessions,
  stations,
  now,
}: {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly now: number;
}) {
  const prs = usePrs();
  const projects = useProjects();
  const meter = tightest(rateMeters(sessions, stations));
  const usage = meter === null ? null : usageParts(meter);
  const live = sessions.filter(
    (c) => c.status !== 'ended' && c.status !== 'lost' && c.status !== 'unreachable',
  );
  const loads = loadMap(sessions, stations);
  const pulls = live
    .map((card) => ({ card, match: prOfSession(prs, projects, card) }))
    .filter((p) => p.match !== undefined);
  return (
    <section className="fd-wall-area" data-area="side" aria-label="Fleet">
      {meter === null || meter.usedPercent === null ? null : (
        <div className="fd-wall-block" data-part="usage">
          <h2 className="fd-wall-h">Usage</h2>
          <p>
            {usage?.label} · <span className="fd-wall-fuel">{usage?.percent}</span>
          </p>
          <span
            className="fd-meter"
            data-level={meter.usedPercent >= WARN_PERCENT ? 'high' : undefined}
            aria-hidden="true"
          >
            <span style={{ width: `${Math.min(100, Math.max(0, meter.usedPercent))}%` }} />
          </span>
          <p className="fd-wall-note">
            {stationName(meter.stationId, stations)}
            {meter.resetsAt === null ? '' : ` · ${resetsText(meter.resetsAt, now)}`}
          </p>
        </div>
      )}
      {stations.length === 0 ? null : (
        <div className="fd-wall-block" data-part="stations">
          <h2 className="fd-wall-h">Stations</h2>
          <ul className="fd-wall-list">
            {stations.map((st) => (
              <StationRow key={st.station_id} station={st} />
            ))}
          </ul>
        </div>
      )}
      {pulls.length === 0 ? null : (
        <div className="fd-wall-block" data-part="prs">
          <h2 className="fd-wall-h">Pull requests</h2>
          <ul className="fd-wall-list">
            {pulls.map(({ card, match }) =>
              match === undefined ? null : (
                <li key={refOf(card)} title={prChipTitle(match.pr)}>
                  <span className="fd-callsign">
                    {card.summary.callsign ?? card.summary.session_id}
                  </span>{' '}
                  · #{match.pr.number} {prStateText(match.pr)}
                  {match.pr.checks === 'none' ? '' : ` · checks ${match.pr.checks}`}
                </li>
              ),
            )}
          </ul>
        </div>
      )}
      {loads.cells.length === 0 ? null : <LoadMap map={loads} />}
    </section>
  );
}

/** A prose row: the first line of the text without its Markdown marks, cut to 160 characters. */
export const proseLine = (text: string): string =>
  oneLine(plainLine(oneLine(text, Number.POSITIVE_INFINITY)), 160);

/** The rows that the stream keeps. */
export const STREAM_ITEMS = 50;
/** The wait between two reads of the stream. */
export const STREAM_REFRESH_MS = 10_000;

export function StreamArea({
  views,
  sessions,
  filter,
  live,
  now,
  initialItems = null,
}: {
  readonly views: ViewsActions | undefined;
  /** The sessions of the board; the stream keeps the items of those that pass `filter`. */
  readonly sessions: readonly DeckSession[];
  /** The station and repository filter of the wall route. */
  readonly filter: Pick<WallRoute, 'stations' | 'repo'>;
  readonly live: boolean;
  readonly now: number;
  readonly initialItems?: readonly ActivityItem[] | null;
}) {
  const [items, setItems] = useState<readonly ActivityItem[] | null>(initialItems);
  const [error, setError] = useState<string | null>(null);
  const filterStations = filter.stations.join('\n');
  useEffect(() => {
    if (views === undefined || !live) return undefined;
    let on = true;
    const load = () => {
      views.activity({ limit: STREAM_ITEMS }).then(
        (result) => {
          if (!on) return;
          setItems(result.items);
          setError(null);
        },
        (e: unknown) => on && setError(e instanceof Error ? e.message : 'The stream did not load.'),
      );
    };
    load();
    const timer = setInterval(load, STREAM_REFRESH_MS);
    return () => {
      on = false;
      clearInterval(timer);
    };
    // The key stands for the filter: a new array of the same stations does not reload.
  }, [views, live, filterStations, filter.repo]);
  // The same filter as the session areas (`wallSessions`); the stream reads `repo` in another form.
  const passing = useMemo(
    () => new Set(wallSessions(sessions, filter).map(refOf)),
    // `filterStations` stands for `filter.stations`.
    [sessions, filterStations, filter.repo],
  );
  // The deck sends the newest first, and the list keeps that order: the newest row sits under the
  // heading, the oldest are cut at the bottom, and a screen reader reads the newest first. The same
  // event of one session several times in a row is one row with its count, as on the Activity
  // page (`foldLines` keeps the last of a run, so it folds the reversed list).
  const rows = foldLines<ActivityItem & { readonly repeat?: number }>(
    (items ?? [])
      .filter((item) => passing.has(`${item.station_id}/${item.session_id}`))
      .slice(0, STREAM_ITEMS)
      .reverse(),
    (a, b) =>
      a.station_id === b.station_id &&
      a.session_id === b.session_id &&
      a.kind === b.kind &&
      a.text === b.text,
  ).reverse();
  return (
    <section className="fd-wall-area" data-area="stream" aria-label="Activity">
      <h2 className="fd-wall-h">Activity</h2>
      {error !== null ? <p className="fd-wall-note">{error}</p> : null}
      {items === null ? (
        error === null ? (
          <p className="fd-wall-note">{live ? 'Loading…' : 'No activity yet.'}</p>
        ) : null
      ) : rows.length === 0 ? (
        <p className="fd-wall-note">No activity yet.</p>
      ) : (
        <ol className="fd-wall-stream">
          {rows.map((item, n) => {
            const text = oneLine(item.text, 160);
            // A tool call reads as the Activity page reads it (watch/tooltext): the tool name in
            // words (no raw MCP id, no "(ok)"), a command or a path in mono, words in sans, and a
            // failed or denied call ends in a hazard word (one form for one outcome).
            const tool = activityTool(item.kind, text);
            const name = item.session_title ?? item.session_id;
            // A provider error in a message reads as its cause; the raw words stay in the title.
            const said = item.kind === 'message' ? agentLine(item.text) : item.text;
            return (
              <li
                key={`${item.station_id}/${item.session_id}/${item.station_seq ?? `m${n}`}`}
                data-kind={item.kind}
                data-outcome={tool?.outcome ?? undefined}
              >
                <time dateTime={new Date(item.at).toISOString()}>{relativeAge(item.at, now)}</time>{' '}
                {/* The session is a column of its own (16ch, wall.css): every event starts at one x. */}
                <strong title={name}>{name}</strong>{' '}
                {/* The event: an ultra row wraps it under itself (wall.css). */}
                <span
                  className="fd-wall-event"
                  title={said === item.text ? undefined : item.text.trim()}
                >
                  {tool === null ? (
                    // Prose is one plain line, as on the tile above it: no Markdown marks.
                    proseLine(said)
                  ) : (
                    <>
                      {tool.lead === '' ? null : `${tool.lead} `}
                      <b className="fd-wall-tool">{tool.name}</b>
                      {tool.target === '' ? null : tool.form === 'words' ? (
                        <> {tool.target}</>
                      ) : (
                        <>
                          {' '}
                          <code>
                            {tool.form === 'path'
                              ? shortPath(homePath(tool.target), 60)
                              : homeShortIn(tool.target)}
                          </code>
                        </>
                      )}
                      {tool.outcome === null ? null : (
                        <>
                          {' '}
                          <span className="fd-wall-result">{tool.outcome}</span>
                        </>
                      )}
                    </>
                  )}
                  {(item.repeat ?? 1) > 1 ? (
                    <>
                      {' '}
                      <span className="fd-count">
                        {item.repeat}
                        <span className="fd-vh"> times</span>
                      </span>
                    </>
                  ) : null}
                </span>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}
