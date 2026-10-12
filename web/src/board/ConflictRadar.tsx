import { useCallback, useEffect, useState } from 'react';
import type { FileConflict, SessionKey, StationView } from '../gen';
import { Icon, TriangleAlert } from '../ui/icons';
import { agoText, type ConflictLoader } from './conflicts';

type Props = {
  readonly load: ConflictLoader;
  readonly stations: readonly StationView[];
  /** Changes when the fleet changes, so the radar loads again. */
  readonly version: string;
  readonly onOpen: (key: SessionKey) => void;
};

/** How many conflicts get their own line in the strip. */
const SHOWN = 3;

/** Files that sessions on any station changed at the same time (DL-306). */
export function ConflictRadar({ load, version, onOpen }: Props) {
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
  const list = conflicts ?? [];
  if (problem === null && list.length === 0) return null;
  return (
    <section className="fd-confstrip" aria-label="Conflict radar">
      {problem !== null ? (
        <p className="fd-confstrip-line" role="alert">
          Conflict radar: {problem}
        </p>
      ) : null}
      <ul className="fd-confstrip-lines">
        {list.slice(0, SHOWN).map((c) => (
          <li
            key={`${c.repo ?? ''}/${c.file}`}
            className="fd-confstrip-line"
            aria-label={`Conflict ${c.file}`}
          >
            <Icon of={TriangleAlert} size={14} />
            <span className="fd-confstrip-hd">CONFLICT</span>
            <span className="fd-confstrip-file">{c.file}</span>
            {c.repo !== null ? <span className="fd-confstrip-repo">{c.repo}</span> : null}
            <span>—</span>
            <span className="fd-confstrip-who">
              {c.touches.map((t, i) => (
                <span key={`${t.station_id}/${t.session_id}`}>
                  {i > 0 ? ' × ' : null}
                  <button
                    type="button"
                    className="fd-namebtn"
                    onClick={() => onOpen({ station_id: t.station_id, session_id: t.session_id })}
                  >
                    {t.session_title ?? t.session_id}
                  </button>
                </span>
              ))}
            </span>
            <span className="fd-confstrip-age">
              {agoText(Math.max(...c.touches.map((t) => t.touched_at)), now)}
            </span>
          </li>
        ))}
        {list.length > SHOWN ? (
          <li className="fd-confstrip-line fd-confstrip-more">+{list.length - SHOWN} more</li>
        ) : null}
      </ul>
    </section>
  );
}
