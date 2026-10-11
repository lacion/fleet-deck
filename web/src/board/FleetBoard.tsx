import type { SessionSummary } from '../gen';
import { groupByLane, LANES, providerLabel } from './lanes';

type Props = {
  readonly sessions: readonly SessionSummary[];
};

/** The fleet board: one column per lane, one card per session. */
export function FleetBoard({ sessions }: Props) {
  const groups = groupByLane(sessions);
  return (
    <main className="fd-board" aria-label="Fleet board">
      {LANES.map((lane) => {
        const cards = groups[lane.activity];
        return (
          <section
            key={lane.activity}
            className="fd-lane"
            data-lane={lane.activity}
            aria-label={lane.label}
          >
            <h2 className="fd-lane-title">
              {lane.label} <span className="fd-count">{cards.length}</span>
            </h2>
            {cards.length === 0 ? (
              <p className="fd-empty">No sessions</p>
            ) : (
              <ul className="fd-cards">
                {cards.map((s) => (
                  <li key={s.session_id} className="fd-card" data-session={s.session_id}>
                    <span className="fd-title">{s.title ?? s.session_id}</span>
                    <span className="fd-badge" data-provider={s.provider ?? 'unknown'}>
                      {providerLabel(s.provider)}
                    </span>
                    {s.model === null ? null : <span className="fd-model">{s.model}</span>}
                  </li>
                ))}
              </ul>
            )}
          </section>
        );
      })}
    </main>
  );
}
