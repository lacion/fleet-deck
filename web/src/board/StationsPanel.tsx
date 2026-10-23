import { useEffect, useRef, useState } from 'react';
import { formatHash } from '../app/routes';
import { DesktopPanel } from '../desktop/DesktopPanel';
import { relativeAge } from '../app/sessions';
import { useLayout } from '../app/viewport';
import type { DeckSession, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { Check, Copy, Ellipsis, Icon, Plus, Server } from '../ui/icons';
import { Menu } from '../ui/Menu';
import { EmptyState, ErrorNotice, Page } from '../ui/page';
import { useNow } from '../ui/useNow';
import { ShellPanel } from '../shell/ShellPanel';
import { PairingDialog } from './PairingDialog';
import {
  ConfigReferenceHeader,
  type DriftReference,
  StationConfigDrift,
  useDriftReference,
} from './StationConfigDrift';
import { StationBusiest, StationResources } from './StationResources';
import { stationsSummary } from './StationSummary';
import { StationTaildrive, TaildriveFact } from './StationTaildrive';
import { SlotMeter, StationTraits } from './StationTraits';
import { Worktrees } from './StationWorktrees';
import { authLabel, type StationActions } from './pairing';

export { WorktreeRow } from './StationWorktrees';
export { loadWidth } from './StationTraits';

type Props = {
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  readonly freshness: Freshness;
  readonly actions?: StationActions | undefined;
  /** Test seam: the config reference state in place of the deck's (`useDriftReference`). */
  readonly drift?: DriftReference | undefined;
};

/** The Connection fact of a link direction (Details); an unknown direction has no fact. */
const connectionText = (direction: string): string | null =>
  direction === 'station_dials'
    ? 'The station connects to this deck'
    : direction === 'deck_dials'
      ? 'This deck connects to the station'
      : null;

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** "8 h ago", or "now" within the minute. */
const agoText = (at: number, now: number): string => {
  const age = relativeAge(at, now);
  return age === 'now' ? age : `${age} ago`;
};

/** The exact time of a fact, as its tooltip; nothing for a value that is not a time. */
const isoTitle = (at: number): string | undefined =>
  Number.isNaN(new Date(at).getTime()) ? undefined : new Date(at).toISOString();

/**
 * A long id (a key fingerprint, a station id) shortened to its first 8 and last 4 characters, the
 * whole value in the tooltip, with a button that copies the whole value. The station id is a
 * prefix of the key: the end of the value tells the two apart.
 */
function Fingerprint({ value, label }: { readonly value: string; readonly label: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const short = value.length > 13 ? `${value.slice(0, 8)}…${value.slice(-4)}` : value;
  return (
    <>
      <span title={value}>{short}</span>
      <button
        type="button"
        className="fd-iconbtn fd-sm"
        aria-label={label}
        title={label}
        onClick={() => {
          void navigator.clipboard?.writeText(value).then(() => {
            setCopied(true);
            clearTimeout(timer.current);
            timer.current = setTimeout(() => setCopied(false), 1500);
          });
        }}
      >
        <Icon of={copied ? Check : Copy} size={14} />
      </button>
    </>
  );
}

/** Link health and board freshness are different facts. Never invent metadata. */
export function StationsPanel({ stations, sessions, freshness, actions, drift: given }: Props) {
  const [adding, setAdding] = useState(false);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [removeError, setRemoveError] = useState<string | null>(null);
  const now = useNow(undefined, 30000);
  // A phone card folds its session load (StationBusiest): one card fills less than a screen.
  const phone = useLayout() === 'phone';
  const canPair = actions?.canAdmin === true && freshness === 'live';
  const deckDrift = useDriftReference();
  const drift = given ?? deckDrift;
  const reference = stations.find((s) => s.station_id === drift.station);
  const referenceMenu =
    drift.offered && stations.length > 0 ? (
      <ConfigReferenceHeader
        stations={stations}
        reference={reference}
        canSet={actions?.canAdmin === true}
        onSet={(id) => void drift.set(id)}
      />
    ) : null;
  // A login without the admin scope reads why Add station is absent in the page sentence (§4.3).
  const summary = stationsSummary(stations, freshness);
  const scopeNote =
    actions === undefined || actions.canAdmin ? null : 'Pairing needs the admin scope.';
  return (
    <Page
      title="Stations"
      description={
        scopeNote === null ? (
          summary
        ) : summary === null ? (
          scopeNote
        ) : (
          <>
            {summary} {scopeNote}
          </>
        )
      }
      className="fd-stations"
      wide
      actions={
        actions?.canAdmin === true ? (
          <>
            {referenceMenu}
            <button
              type="button"
              className="fd-primary"
              disabled={!canPair}
              onClick={() => setAdding(true)}
            >
              <Icon of={Plus} /> Add station
            </button>
          </>
        ) : (
          referenceMenu
        )
      }
    >
      {adding && actions !== undefined ? (
        <PairingDialog actions={actions} stations={stations} onClose={() => setAdding(false)} />
      ) : null}
      {removeError === null ? null : <ErrorNotice text={removeError} />}
      {drift.offered && drift.error !== null ? <ErrorNotice text={drift.error} /> : null}
      {stations.length === 0 ? (
        <EmptyState
          icon={Server}
          title="No paired stations"
          body="Pair a station to start sessions on its machine."
        />
      ) : null}
      <ul className="fd-station-list">
        {stations.map((station) => {
          const cards = sessions.filter((card) => card.station_id === station.station_id);
          const active = cards.filter((card) =>
            ['starting', 'running', 'stopping'].includes(card.summary.state),
          ).length;
          const info = station.info ?? null;
          const up = freshness !== 'stale' && station.reachable;
          // `last_seen` is the time the last link came up: for a station that is up it is the
          // age of the link (the Linked fact in Details), never a "Last seen" (principle 7).
          const lastSeen = up ? null : station.last_seen;
          const linkedAt = up ? station.last_seen : null;
          const connection = connectionText(station.direction);
          return (
            <li
              className="fd-station fd-panel"
              key={station.station_id}
              data-station={station.station_id}
            >
              <div className="fd-station-head">
                <div className="fd-station-line">
                  <div className="fd-station-title">
                    <h3 className="fd-station-name">{station.name}</h3>
                    <StationTraits traits={info?.traits} />
                  </div>
                  <div className="fd-station-meta">
                    <span
                      className="fd-station-health fd-health"
                      data-health={
                        freshness === 'stale' ? 'stale' : station.reachable ? 'up' : 'down'
                      }
                    >
                      <span
                        className="fd-dot"
                        data-status={up ? 'online' : 'unreachable'}
                        aria-hidden="true"
                      />
                      {freshness === 'stale'
                        ? 'Stale link status'
                        : station.reachable
                          ? 'online'
                          : 'offline'}
                    </span>
                    <div className="fd-station-live">
                      {lastSeen === null ? null : (
                        <span title={isoTitle(lastSeen)}>Last seen {agoText(lastSeen, now)}</span>
                      )}
                      {drift.offered ? (
                        <StationConfigDrift station={station} reference={reference} />
                      ) : null}
                      {freshness === 'stale' ? (
                        <span>Last reported: {station.reachable ? 'online' : 'offline'}.</span>
                      ) : null}
                    </div>
                    {/* The notation of the fleet lane (§4.4): "3 of 12" with the ticks, or
                        "12 free" while the station runs no session. */}
                    <span className="fd-station-load">
                      {info?.max_sessions == null ? (
                        <span className="fd-station-load-count">{`${active} sessions`}</span>
                      ) : (
                        <SlotMeter
                          max={info.max_sessions}
                          used={active}
                          countClass="fd-station-load-count"
                        />
                      )}
                    </span>
                  </div>
                </div>
                {actions?.canAdmin === true ? (
                  <div className="fd-station-menu">
                    <Menu
                      summary={<Icon of={Ellipsis} />}
                      summaryClass="fd-iconbtn"
                      label="Station actions"
                    >
                      {(close) => (
                        <>
                          {drift.offered && reference?.station_id !== station.station_id ? (
                            <button
                              type="button"
                              onClick={() => {
                                close();
                                void drift.set(station.station_id);
                              }}
                            >
                              Use as config reference
                            </button>
                          ) : null}
                          <button
                            type="button"
                            className="fd-danger"
                            disabled={!canPair}
                            onClick={() => {
                              close();
                              setConfirm(station.name);
                            }}
                          >
                            Remove station…
                          </button>
                        </>
                      )}
                    </Menu>
                  </div>
                ) : null}
              </div>
              <div className="fd-station-props">
                <div className="fd-station-prop" data-prop="providers">
                  <h4 className="fd-station-prop-label">Providers</h4>
                  {info === null ? (
                    <p className="fd-note">Reported when the station links to this deck.</p>
                  ) : info.providers.length === 0 ? (
                    <p className="fd-note">No provider configured.</p>
                  ) : (
                    <ul className="fd-station-providers" aria-label="Configured providers">
                      {info.providers.map((p) => (
                        <li key={p.instance} data-instance={p.instance} title={p.instance}>
                          {/* The version is quiet: it matters only where it differs. */}
                          <span>
                            {capitalize(p.provider)}
                            {p.version === null ? null : (
                              <>
                                {' '}
                                <span className="fd-station-ver">{p.version}</span>
                              </>
                            )}
                          </span>
                          <span
                            className="fd-station-auth"
                            data-auth={
                              p.auth === 'signed_in'
                                ? 'ok'
                                : p.auth === 'signed_out'
                                  ? 'hazard'
                                  : 'muted'
                            }
                          >
                            <span
                              className="fd-dot"
                              data-status={p.auth === 'signed_in' ? 'online' : undefined}
                              aria-hidden="true"
                            />
                            {authLabel(p.auth)}
                          </span>
                          {/* A signed-out provider names the next step (§5): its station's
                              accounts in Settings, for the owner. */}
                          {p.auth === 'signed_out' && actions?.canAdmin === true ? (
                            <a
                              className="fd-station-signin fd-tap"
                              href={formatHash({
                                name: 'settings',
                                tab: 'accounts',
                                station: station.station_id,
                                finish: null,
                                oauthError: null,
                              })}
                            >
                              Sign in…
                            </a>
                          ) : null}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
                <StationResources resources={info?.resources} />
                <StationBusiest
                  resources={info?.resources}
                  station={station.station_id}
                  sessions={cards}
                  folded={phone}
                />
                <StationTaildrive taildrive={info?.taildrive} />
              </div>
              <div className="fd-station-folds">
                <details className="fd-station-details">
                  <summary className="fd-label" data-variant="toggle">
                    Details
                  </summary>
                  <dl className="fd-facts">
                    <dt>Station ID</dt>
                    <dd>
                      <Fingerprint value={station.station_id} label="Copy the station ID" />
                    </dd>
                    <dt>Key fingerprint</dt>
                    <dd>
                      <Fingerprint value={station.key} label="Copy the key fingerprint" />
                    </dd>
                    {connection === null ? null : (
                      <>
                        <dt>Connection</dt>
                        <dd className="fd-facts-text">{connection}</dd>
                      </>
                    )}
                    {/* A station that connects to the deck has no address here (principle 7). */}
                    {station.dial_url == null ? null : (
                      <>
                        <dt>Link address</dt>
                        <dd>{station.dial_url}</dd>
                      </>
                    )}
                    {linkedAt === null ? null : (
                      <>
                        <dt>Linked</dt>
                        <dd className="fd-facts-text" title={isoTitle(linkedAt)}>
                          {agoText(linkedAt, now)}
                        </dd>
                      </>
                    )}
                    {info?.version == null ? null : (
                      <>
                        <dt>Version</dt>
                        <dd>{info.version}</dd>
                      </>
                    )}
                    <TaildriveFact taildrive={info?.taildrive} />
                  </dl>
                </details>
                {actions?.canAdmin === true && actions.shells !== undefined ? (
                  <details className="fd-station-sec">
                    <summary className="fd-label" data-variant="toggle">
                      Shells
                    </summary>
                    {info?.allow_shell === false ? (
                      <section className="fd-shells" aria-label={`Shells on ${station.name}`}>
                        {/* A station setting is changed on the board, never in station.toml. */}
                        <p className="fd-note">
                          Shells are off on this station.{' '}
                          <a
                            className="fd-tap"
                            href={formatHash({
                              name: 'settings',
                              tab: 'station',
                              station: station.station_id,
                            })}
                          >
                            Turn them on in Settings
                          </a>
                          .
                        </p>
                      </section>
                    ) : (
                      <ShellPanel
                        station={station.name}
                        actions={actions.shells}
                        disabled={!canPair || !station.reachable}
                      />
                    )}
                  </details>
                ) : null}
                {actions?.worktrees === undefined ? null : (
                  <Worktrees
                    station={station.name}
                    stationId={station.station_id}
                    load={actions.worktrees}
                    act={actions.worktreeAction}
                    now={now}
                  />
                )}
                {/* A station without a desktop has no Desktop fold (§1 principle 7). */}
                {info?.desktop == null ? null : <DesktopPanel station={station} />}
              </div>
              {actions?.canAdmin === true && confirm === station.name ? (
                <div className="fd-station-foot">
                  <p>Remove {station.name}? Its link closes and its cards leave the board.</p>
                  <div className="fd-station-foot-actions">
                    <button
                      type="button"
                      className="fd-ghost fd-sm"
                      onClick={() => setConfirm(null)}
                    >
                      Cancel
                    </button>
                    <button
                      type="button"
                      className="fd-danger fd-sm"
                      disabled={!canPair}
                      onClick={() => {
                        setConfirm(null);
                        setRemoveError(null);
                        void actions.remove(station.name).catch((e: unknown) => {
                          setRemoveError(e instanceof Error ? e.message : 'Remove failed');
                        });
                      }}
                    >
                      Confirm remove
                    </button>
                  </div>
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
    </Page>
  );
}
