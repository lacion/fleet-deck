import { useCallback, useEffect, useState } from 'react';
import type { FileConflict, SessionKey, StationView } from '../gen';
import type { RadarAnswer } from '../integrations/actions';
import { useIntegrations, useProjects } from '../integrations/context';
import { Icon, TriangleAlert } from '../ui/icons';
import { agoText, type ConflictLoader } from './conflicts';
import { hasProjectGroups, RadarGroups, radarHasRows } from './RadarGroups';

type Props = {
  readonly list: readonly FileConflict[] | null;
  readonly problem: string | null;
  readonly stations: readonly StationView[];
  readonly onOpen: (key: SessionKey) => void;
};

/** How many conflicts get their own line in the strip. */
const SHOWN = 3;

/** Loads the conflicts again when `version` changes (the fleet changed). */
export function useConflicts(
  load: ConflictLoader | undefined,
  version: string,
): { readonly list: FileConflict[] | null; readonly problem: string | null } {
  const [list, setList] = useState<FileConflict[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const refresh = useCallback(() => {
    if (load === undefined) return;
    load().then(
      (next) => {
        setList(next);
        setProblem(null);
      },
      (e: unknown) => setProblem(e instanceof Error ? e.message : String(e)),
    );
  }, [load]);
  useEffect(refresh, [refresh, version]);
  return { list, problem };
}

/**
 * FG-7 (§10.5): the radar groups of the deck. It reads again when `version` changes (the conflict
 * list was read again). `null`: no answer yet, an older deck (404), or a failed read (the previous
 * answer stays).
 */
export function useRadar(version: unknown): RadarAnswer | null {
  const load = useIntegrations()?.actions.radar;
  const [radar, setRadar] = useState<RadarAnswer | null>(null);
  useEffect(() => {
    if (load === undefined) return undefined;
    let live = true;
    load().then(
      (next) => live && setRadar(next),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [load, version]);
  return radar;
}

/** Files that sessions on any station changed at the same time (DL-306). */
export function ConflictRadar({ list: given, problem, onOpen }: Props) {
  const now = Date.now();
  const radar = useRadar(given);
  const projects = useProjects();
  if (hasProjectGroups(radar) && radarHasRows(radar)) {
    return (
      <section className="fd-confstrip" aria-label="Conflict radar">
        {problem !== null ? (
          <p className="fd-confstrip-line" title={problem}>
            Conflict radar unavailable
          </p>
        ) : null}
        <RadarGroups radar={radar} projects={projects} onOpen={onOpen} now={now} />
      </section>
    );
  }
  const list = given ?? [];
  if (problem === null && list.length === 0) return null;
  return (
    <section
      className="fd-confstrip"
      aria-label="Conflict radar"
      data-state={list.length === 0 ? 'error' : undefined}
    >
      {problem !== null ? (
        <p className="fd-confstrip-line" title={problem}>
          Conflict radar unavailable
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
            <span className="fd-confstrip-hd">Conflict</span>
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
