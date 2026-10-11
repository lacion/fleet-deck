import type { DeckSession, SessionKey, StationView } from '../gen';
import { stationName } from './fleet';

type Props = {
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly onOpen: (key: SessionKey) => void;
};

/** Open approvals of every session, the oldest first (PLAN §12 needs-you rail). */
export function NeedsYouRail({ sessions, stations, onOpen }: Props) {
  const waiting = sessions
    .flatMap((s) =>
      s.summary.open_requests.map((r) => ({
        key: { station_id: s.station_id, session_id: s.summary.session_id },
        title: s.summary.title ?? s.summary.session_id,
        request: r,
      })),
    )
    .sort((a, b) => a.request.requested_at - b.request.requested_at);
  return (
    <section className="fd-rail" aria-label="Needs you rail">
      <h2>
        Needs you <span className="fd-count">{waiting.length}</span>
      </h2>
      {waiting.length === 0 ? (
        <p className="fd-empty">Nothing waits for you.</p>
      ) : (
        <ul>
          {waiting.map((w) => (
            <li key={`${w.key.station_id}/${w.key.session_id}/${w.request.request_id}`}>
              <span>
                {w.title} · {stationName(w.key.station_id, stations)}: {w.request.title}
              </span>
              <button
                type="button"
                aria-label={`Answer ${w.request.title} in ${w.title}`}
                onClick={() => onOpen(w.key)}
              >
                Answer
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
