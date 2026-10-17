// R13 P1 (design §4.2): the four areas of the mission mode: needs, tiles, side and stream.
// Everything here only reads. A part with no data is left out.
import { type RefObject, useEffect, useMemo, useState } from 'react';
import type { WallRoute } from '../app/routes';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import { gb, usedShare } from '../board/resources';
import { rateMeters, resetsText, tightest } from '../board/usage';
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
import { type Need, oneLine, wallSessions } from './model';
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
      <h2 className="fd-wall-h">Needs you</h2>
      {needs.length === 0 ? (
        <p className="fd-wall-note">Nothing needs you.</p>
      ) : (
        <ol className="fd-wall-needs">
          {needs.map(({ card, request }) => {
            const s = card.summary;
            return (
              <li key={`${refOf(card)}/${request.request_id}`} className="fd-wall-need">
                <div className="fd-wall-need-text">
                  <p className="fd-wall-need-head">
                    <strong>{s.callsign ?? s.title ?? s.session_id}</strong>{' '}
                    <span>{stationName(card.station_id, stations)}</span>
                  </p>
                  <p className="fd-wall-need-what">
                    <span className="fd-badge">{request.request_kind}</span> {request.title}
                  </p>
                  {request.details.trim() === '' ? null : (
                    <p className="fd-wall-need-sum">{oneLine(request.details)}</p>
                  )}
                  <p className="fd-wall-note">
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
}: {
  readonly cards: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly timelines: Readonly<Record<string, Timeline>> | undefined;
  readonly freshness: Freshness;
  readonly failed: ReadonlySet<string>;
  readonly areaRef: RefObject<HTMLElement | null>;
}) {
  return (
    <section className="fd-wall-area" data-area="tiles" aria-label="Sessions" ref={areaRef}>
      {cards.length === 0 ? <p className="fd-wall-note">No live session.</p> : null}
      {/* Always rendered: the wall reads the grid gap from it to count the tiles that fit. */}
      <ul className="fd-wall-tiles" style={cards.length === 0 ? { display: 'none' } : undefined}>
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

type DiskLike = {
  readonly total_bytes: number;
  readonly free_bytes: number;
  readonly label?: string;
};

function StationRow({ station }: { readonly station: StationView }) {
  // A wall display gets `display_resources` (no disk label, no path) in place of `info`.
  const r = station.info?.resources ?? station.display_resources;
  const disks: readonly DiskLike[] = r?.disks ?? [];
  const fullest = disks.reduce<DiskLike | null>(
    (best, d) =>
      best === null ||
      usedShare(d.total_bytes - d.free_bytes, d.total_bytes) >
        usedShare(best.total_bytes - best.free_bytes, best.total_bytes)
        ? d
        : best,
    null,
  );
  return (
    <li className="fd-wall-station" data-reachable={station.reachable ? 'true' : 'false'}>
      <span className="fd-wall-station-name">{station.name}</span>
      <span className="fd-wall-note">{station.reachable ? 'Link up' : 'Unreachable'}</span>
      {r?.cpu == null ? null : (
        <span className="fd-wall-note">
          CPU load {(r.cpu.load5_x100 / 100).toFixed(1)} on {r.cpu.cores} cores
        </span>
      )}
      {r?.memory == null ? null : (
        <span className="fd-wall-note">
          Memory {gb(r.memory.available_bytes)} free of {gb(r.memory.total_bytes)}
        </span>
      )}
      {fullest == null ? null : (
        <span className="fd-wall-note">
          Disk{fullest.label === undefined ? '' : ` ${fullest.label}`}: {gb(fullest.free_bytes)}{' '}
          free of {gb(fullest.total_bytes)}
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
            {meter.label} · {meter.scope}: {Math.round(meter.usedPercent)}% used
          </p>
          {meter.resetsAt === null ? null : (
            <p className="fd-wall-note">{resetsText(meter.resetsAt, now)}</p>
          )}
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
  // The deck sends the newest first; the wall shows the newest at the bottom.
  const rows = (items ?? [])
    .filter((item) => passing.has(`${item.station_id}/${item.session_id}`))
    .slice(0, STREAM_ITEMS)
    .reverse();
  return (
    <section className="fd-wall-area" data-area="stream" aria-label="Activity">
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
