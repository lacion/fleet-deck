// R13 P1 (design §4.2): the four areas of the mission mode: needs, tiles, side and stream.
// Everything here only reads. A part with no data is left out.
import { type CSSProperties, type RefObject, useEffect, useMemo, useState } from 'react';
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
import { Tile } from '../watch/WatchPage';
import { refOf } from '../watch/tiles';
import { keyOf, timelineOf } from './hooks';
import { LoadMap } from './LoadMap';
import { loadMap } from './loadCells';
import { NEED_LABEL, type Need, oneLine, usageLine, wallSessions } from './model';
import { QrCode } from './QrCode';
import { answerUrl } from './qr';

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
        <p className="fd-wall-note">Nothing needs you.</p>
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
   * `--fd-wall-tile-k`, is set on the grid: the needs area grows with the tiles. */
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

/** One decimal place of gigabytes, without the unit: "20.4". */
const gbNumber = (bytes: number): string => (bytes / 1e9).toFixed(1);

function StationRow({ station }: { readonly station: StationView }) {
  // A wall display gets `display_resources` (no disk label, no path) in place of `info`. The wall
  // shows the load and the memory only: one short line that a person reads across a room.
  const r = station.info?.resources ?? station.display_resources;
  const stats = [
    r?.cpu == null ? null : `CPU ${(r.cpu.load5_x100 / 100).toFixed(1)}/${r.cpu.cores}`,
    r?.memory == null
      ? null
      : `Mem ${gbNumber(r.memory.total_bytes - r.memory.available_bytes)}/${gbNumber(r.memory.total_bytes)} GB`,
  ].filter((t): t is string => t !== null);
  return (
    <li className="fd-wall-station" data-reachable={station.reachable ? 'true' : 'false'}>
      <span className="fd-wall-reach" aria-hidden="true" />
      <span className="fd-wall-station-name">{station.name}</span>
      <span className="fd-wall-note">{station.reachable ? 'Link up' : 'Unreachable'}</span>
      {stats.length === 0 ? null : (
        <span className="fd-wall-station-stats">{stats.join(' · ')}</span>
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
          <p>{usageLine(meter)}</p>
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
                  {card.summary.callsign ?? card.summary.session_id} · #{match.pr.number}{' '}
                  {prStateText(match.pr)}
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
  // The deck sends the newest first. The list runs oldest first, and `.fd-wall-stream` draws it
  // from the bottom up: the newest row sits under the heading, the oldest are cut at the bottom.
  const rows = (items ?? [])
    .filter((item) => passing.has(`${item.station_id}/${item.session_id}`))
    .slice(0, STREAM_ITEMS)
    .reverse();
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
          {rows.map((item, n) => (
            <li
              key={`${item.station_id}/${item.session_id}/${item.station_seq ?? `m${n}`}`}
              data-kind={item.kind}
            >
              <time dateTime={new Date(item.at).toISOString()}>{relativeAge(item.at, now)}</time>{' '}
              <strong>{item.session_title ?? item.session_id}</strong> {oneLine(item.text, 160)}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
