// FG-8 (design §11.11): the "Lands" section of the project page: the queue and the last ends,
// with the actions of one land (Cancel land, Stop checking, Forget, Send to session).
import { useEffect } from 'react';
import { formatHash } from '../app/routes';
import { sessionTitle } from '../app/sessions';
import type { DeckSession, Project, StationView } from '../gen';
import { LANDS_POLL_MS, LandActions, LandStatusLine, sendStateOf } from '../session/LandPanel';
import type { LandRow, ProjectLands as Lands } from './actions';
import { useIntegrations } from './context';
import { useLoad } from './useLoad';

type Props = {
  readonly project: Project;
  readonly stations: readonly StationView[];
  readonly sessions: readonly DeckSession[];
  /** The lands at the first render (tests). */
  readonly initial?: Lands | null;
};

const nameOf = (row: LandRow): string => row.callsign ?? row.title ?? row.session_id;

function Rows({
  rows,
  empty,
  project,
  stations,
  sessions,
  onChanged,
}: Pick<Props, 'project' | 'stations' | 'sessions'> & {
  readonly rows: readonly LandRow[];
  readonly empty: string;
  readonly onChanged: () => void;
}) {
  const env = useIntegrations();
  if (rows.length === 0) return <p className="fd-empty">{empty}</p>;
  return (
    <ul className="fd-int-list">
      {rows.map((row) => {
        const card = sessions.find(
          (s) => s.station_id === row.station_id && s.summary.session_id === row.session_id,
        );
        return (
          <li key={row.land_id} data-state={row.state}>
            <a
              href={formatHash({
                name: 'session',
                station: row.station_id,
                session: row.session_id,
                tab: 'chat',
              })}
            >
              {card === undefined ? nameOf(row) : sessionTitle(card)}
            </a>
            {row.branch === undefined ? null : <code>{row.branch}</code>}
            <LandStatusLine row={row} project={project} stations={stations} />
            {env === undefined ? null : (
              <LandActions
                row={row}
                stations={stations}
                actions={env.actions}
                send={sendStateOf(card)}
                onChanged={onChanged}
              />
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** The Lands section; nothing for an older deck (404) or a project that never landed. */
export function ProjectLands({ project, stations, sessions, initial }: Props) {
  const env = useIntegrations();
  const actions = env?.actions;
  const { state, reload } = useLoad<Lands | null>(
    async (id) => (actions === undefined ? null : actions.projectLands(id)),
    project.project_id,
    initial,
  );
  const lands = state.kind === 'ok' ? state.value : null;
  const busy = lands !== null && lands.queue.length > 0;
  useEffect(() => {
    if (!busy) return undefined;
    const timer = setInterval(reload, LANDS_POLL_MS);
    return () => clearInterval(timer);
  }, [busy, reload]);
  if (lands === null) return null;
  if (!project.git.sync && lands.queue.length === 0 && lands.recent.length === 0) return null;
  return (
    <section className="fd-panel fd-int-lands" aria-label="Lands">
      <h3 className="fd-label">Lands</h3>
      <h4 className="fd-label">Queue</h4>
      <Rows
        rows={lands.queue}
        empty="Nothing queued."
        project={project}
        stations={stations}
        sessions={sessions}
        onChanged={reload}
      />
      <h4 className="fd-label">Recent</h4>
      <Rows
        rows={lands.recent}
        empty="No lands yet."
        project={project}
        stations={stations}
        sessions={sessions}
        onChanged={reload}
      />
    </section>
  );
}
