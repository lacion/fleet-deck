import { useCallback, useEffect, useState } from 'react';
import type { FileConflict, SessionKey, StationView } from '../gen';
import { type ConflictLoader, agoText } from './conflicts';
import { stationName } from './fleet';

type Props = {
  readonly load: ConflictLoader;
  readonly stations: readonly StationView[];
  /** Changes when the fleet changes, so the radar loads again. */
  readonly version: string;
  readonly onOpen: (key: SessionKey) => void;
};

/** Files that sessions on any station changed at the same time (DL-306). */
export function ConflictRadar({ load, stations, version, onOpen }: Props) {
  const [conflicts, setConflicts] = useState<FileConflict[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const refresh = useCallback(() => {
    load().then(
      (list) => {
        setConflicts(list);
        setProblem(null);
      },
      (e: unknown) => setProblem(e instanceof Error ? e.message : String(e)),
    );
  }, [load]);
  useEffect(refresh, [refresh, version]);
  const now = Date.now();
  return (
    <section className="fd-conflicts" aria-label="Conflict radar">
      <h2>Conflict radar</h2>
      {problem !== null ? <p role="alert">{problem}</p> : null}
      {conflicts === null ? null : conflicts.length === 0 ? (
        <p className="fd-note">No file was changed by two sessions in the last 30 minutes.</p>
      ) : (
        <ul>
          {conflicts.map((c) => (
            <li key={`${c.repo ?? ''}/${c.file}`} aria-label={`Conflict ${c.file}`}>
              <strong>{c.file}</strong>
              {c.repo !== null ? <span className="fd-dim"> in {c.repo}</span> : null}
              <ul>
                {c.touches.map((t) => (
                  <li key={`${t.station_id}/${t.session_id}`}>
                    <button
                      type="button"
                      onClick={() => onOpen({ station_id: t.station_id, session_id: t.session_id })}
                    >
                      {t.session_title ?? t.session_id}
                    </button>{' '}
                    on {stationName(t.station_id, stations)}, {agoText(t.touched_at, now)}
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
