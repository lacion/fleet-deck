import { useState } from 'react';
import type { DeckSession, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { lastSeen, observedProviders } from './fleet';
import { PairingDialog } from './PairingDialog';
import { authLabel, type StationActions } from './pairing';

type Props = {
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  readonly freshness: Freshness;
  readonly actions?: StationActions | undefined;
};

/** Link health and board freshness are different facts. Never invent metadata. */
export function StationsPanel({ stations, sessions, freshness, actions }: Props) {
  const [adding, setAdding] = useState(false);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [removeError, setRemoveError] = useState<string | null>(null);
  const canPair = actions?.canAdmin === true && freshness === 'live';
  return (
    <section className="fd-stations" aria-label="Stations">
      <h2>
        Stations <span className="fd-count">{stations.length}</span>
      </h2>
      {actions === undefined ? null : actions.canAdmin ? (
        <button type="button" disabled={!canPair} onClick={() => setAdding(true)}>
          Add station
        </button>
      ) : (
        <p className="fd-note">Pairing needs the admin scope.</p>
      )}
      {adding && actions !== undefined ? (
        <PairingDialog actions={actions} onClose={() => setAdding(false)} />
      ) : null}
      {removeError === null ? null : <p role="alert">{removeError}</p>}
      {stations.length === 0 ? <p>No paired stations.</p> : null}
      <ul className="fd-station-list">
        {stations.map((station) => {
          const cards = sessions.filter((card) => card.station_id === station.station_id);
          const providers = observedProviders(cards, station.station_id);
          const active = cards.filter((card) =>
            ['starting', 'running', 'stopping'].includes(card.summary.state),
          ).length;
          const info = station.info ?? null;
          return (
            <li className="fd-station" key={station.station_id} data-station={station.station_id}>
              <h3>{station.name}</h3>
              <p
                className="fd-health"
                data-health={freshness === 'stale' ? 'stale' : station.reachable ? 'up' : 'down'}
              >
                {freshness === 'stale'
                  ? 'Stale link status'
                  : station.reachable
                    ? 'Link up'
                    : 'Unreachable'}
              </p>
              {freshness === 'stale' ? (
                <p>Last reported: {station.reachable ? 'link up' : 'unreachable'}.</p>
              ) : null}
              <dl>
                <dt>Station ID</dt>
                <dd>{station.station_id}</dd>
                <dt>Key fingerprint</dt>
                <dd>{station.key}</dd>
                <dt>Direction</dt>
                <dd>
                  {station.direction === 'deck_dials'
                    ? 'Deck dials station'
                    : station.direction === 'station_dials'
                      ? 'Station dials deck'
                      : 'Unknown'}
                </dd>
                <dt>Link address</dt>
                <dd>{station.dial_url ?? 'Dial-only station; no stored address'}</dd>
                <dt>Last seen (UTC)</dt>
                <dd>{lastSeen(station.last_seen)}</dd>
                <dt>Event cursor</dt>
                <dd>{station.cursor}</dd>
                <dt>Observed active sessions</dt>
                <dd>{active}</dd>
                <dt>Configured capacity</dt>
                <dd>
                  {info?.max_sessions == null
                    ? 'Not reported'
                    : `${info.live_sessions} live of ${info.max_sessions} sessions`}
                </dd>
                <dt>Station version</dt>
                <dd>{info?.version ?? 'Not reported'}</dd>
                {info === null ? null : (
                  <>
                    <dt>Station name</dt>
                    <dd>{info.name}</dd>
                    <dt>Reported (UTC)</dt>
                    <dd>{lastSeen(station.info_at ?? null)}</dd>
                  </>
                )}
              </dl>
              <h4>Configured providers</h4>
              {info === null ? (
                <p>Not reported until the station links to this deck.</p>
              ) : info.providers.length === 0 ? (
                <p>No provider instances configured.</p>
              ) : (
                <ul className="fd-providers">
                  {info.providers.map((p) => (
                    <li key={p.instance} data-instance={p.instance}>
                      {p.instance} · {p.provider} · {p.version ?? 'version unknown'} ·{' '}
                      {authLabel(p.auth)}
                    </li>
                  ))}
                </ul>
              )}
              <h4>Providers observed in sessions</h4>
              {providers.length === 0 ? (
                <p>No provider sessions reported.</p>
              ) : (
                <ul>
                  {providers.map((provider) => (
                    <li key={provider}>{provider}</li>
                  ))}
                </ul>
              )}
              {actions?.canAdmin === true ? (
                confirm === station.name ? (
                  <p>
                    Remove {station.name}? Its link closes and its cards leave the board.{' '}
                    <button
                      type="button"
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
                    </button>{' '}
                    <button type="button" onClick={() => setConfirm(null)}>
                      Cancel
                    </button>
                  </p>
                ) : (
                  <button
                    type="button"
                    disabled={!canPair}
                    onClick={() => setConfirm(station.name)}
                  >
                    Remove station
                  </button>
                )
              ) : null}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
