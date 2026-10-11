import type { DeckSession, SessionKey, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { runningSubagents } from '../session/timeline';
import { cardKey, stationName, STATUS_LANES, statusGroups, usageText } from './fleet';
import { providerLabel } from './lanes';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly onOpen?: ((key: SessionKey) => void) | undefined;
};

/** The deck projection owns the lane; cards retain the station-scoped identity. */
export function FleetBoard({ sessions, stations, freshness, onOpen }: Props) {
  const groups = statusGroups(sessions);
  return (
    <main className="fd-board" aria-label="Fleet board">
      {STATUS_LANES.map((lane) => {
        const cards = groups[lane.status];
        return (
          <section
            key={lane.status}
            className="fd-lane"
            data-lane={lane.status}
            aria-label={lane.label}
          >
            <h2 className="fd-lane-title">
              {lane.label} <span className="fd-count">{cards.length}</span>
            </h2>
            {cards.length === 0 ? (
              <p className="fd-empty">No sessions</p>
            ) : (
              <ul className="fd-cards">
                {cards.map((card) => {
                  const s = card.summary;
                  const subagents = runningSubagents(s);
                  return (
                    <li
                      key={cardKey(card)}
                      className="fd-card"
                      data-session={s.session_id}
                      data-station={card.station_id}
                    >
                      <span className="fd-title">{s.title ?? s.session_id}</span>
                      {onOpen ? (
                        <button
                          type="button"
                          className="fd-open"
                          aria-label={`Open session ${s.title ?? s.session_id}`}
                          onClick={() =>
                            onOpen({ station_id: card.station_id, session_id: s.session_id })
                          }
                        >
                          Open
                        </button>
                      ) : null}
                      <span className="fd-station-badge">
                        {stationName(card.station_id, stations)}
                      </span>
                      <span className="fd-badge" data-provider={s.provider ?? 'unknown'}>
                        {providerLabel(s.provider)}
                      </span>
                      {s.provider_instance === null ? null : (
                        <span className="fd-model">Instance: {s.provider_instance}</span>
                      )}
                      <span className="fd-model">Model: {s.model ?? 'unknown'}</span>
                      {freshness === 'stale' ? (
                        <span className="fd-health" data-health="stale">
                          Cached · stale
                        </span>
                      ) : null}
                      {card.status === 'unreachable' ? (
                        <span className="fd-health" data-health="down">
                          Last activity: {s.activity.replaceAll('_', ' ')}
                        </span>
                      ) : null}
                      {subagents > 0 ? (
                        <span className="fd-subagents" data-running-subagents={subagents}>
                          Subagents running: {subagents}
                        </span>
                      ) : null}
                      {s.background_tasks.length > 0 ? (
                        <span>Background tasks: {s.background_tasks.length}</span>
                      ) : null}
                      <span className="fd-card-line">Workspace: {s.cwd ?? 'unknown'}</span>
                      <span className="fd-card-line">Branch: {s.branch ?? 'not reported'}</span>
                      {s.running_tools.length === 0 ? (
                        <span className="fd-card-line">No current tool</span>
                      ) : (
                        <ul className="fd-tools" aria-label="Current tools">
                          {s.running_tools.map((tool) => (
                            <li key={tool.node_id}>
                              {tool.title} · {tool.input_summary}
                            </li>
                          ))}
                        </ul>
                      )}
                      <span className="fd-card-line fd-usage">{usageText(card)}</span>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        );
      })}
    </main>
  );
}
