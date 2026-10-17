// DL-176: the Sessions tab of a project: the live and the ended sessions that belong to it, as the
// cards of the fleet board, and "New session in this project".
import { isLive } from '../app/sessions';
import { FleetCard } from '../board/FleetBoard';
import type { DeckSession, Project, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { Icon, Plus } from '../ui/icons';

type Props = {
  readonly project: Project;
  readonly sessions: readonly DeckSession[];
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly now: number;
  /** Opens the spawn form with the project's station and path, and the worktree option on. */
  readonly onNewSession: (station: string, cwd: string) => void;
  readonly canDrive: boolean;
};

const byNewest = (a: DeckSession, b: DeckSession) => b.summary.updated_at - a.summary.updated_at;

function Cards({
  cards,
  stations,
  freshness,
  now,
}: Pick<Props, 'stations' | 'freshness' | 'now'> & { readonly cards: readonly DeckSession[] }) {
  return (
    <ul className="fd-int-sessions">
      {cards.map((card) => (
        <FleetCard
          key={`${card.station_id}/${card.summary.session_id}`}
          card={card}
          stations={stations}
          freshness={freshness}
          now={now}
          showStation={false}
        />
      ))}
    </ul>
  );
}

export function ProjectSessions({
  project,
  sessions,
  stations,
  freshness,
  now,
  onNewSession,
  canDrive,
}: Props) {
  const own = sessions.filter((s) => s.project_id === project.project_id);
  const live = own.filter(isLive).sort(byNewest);
  const ended = own.filter((s) => !isLive(s)).sort(byNewest);
  return (
    <div className="fd-int-tab" data-tab="sessions">
      {canDrive ? (
        <div className="fd-pane-row">
          <button
            type="button"
            className="fd-primary"
            onClick={() => onNewSession(project.station_id, project.path)}
          >
            <Icon of={Plus} /> New session in this project
          </button>
        </div>
      ) : null}
      {own.length === 0 ? (
        <p className="fd-empty">No sessions in this project yet.</p>
      ) : (
        <>
          {live.length === 0 ? null : (
            <section aria-label="Live sessions">
              <h3 className="fd-label">Live · {live.length}</h3>
              <Cards cards={live} stations={stations} freshness={freshness} now={now} />
            </section>
          )}
          {ended.length === 0 ? null : (
            <section aria-label="Ended sessions">
              <h3 className="fd-label">Ended · {ended.length}</h3>
              <Cards cards={ended} stations={stations} freshness={freshness} now={now} />
            </section>
          )}
        </>
      )}
    </div>
  );
}
