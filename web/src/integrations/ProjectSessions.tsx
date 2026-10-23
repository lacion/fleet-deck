// DL-176: the Sessions tab of a project: the live and the ended sessions that belong to it, as the
// strips of the fleet board (DESIGN-SYSTEM §4.1), and "New session in this project".
import { isLive } from '../app/sessions';
import { stationName } from '../board/fleet';
import { FleetStrip } from '../board/FleetStrip';
import type { DeckSession, Project, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { homeShort } from '../ui/format';
import { Icon, Layers, Plus } from '../ui/icons';
import { EmptyState } from '../ui/page';
import { OwnProject } from './ProjectChip';

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

/** The strips of some sessions. A project has homes on several stations: each strip names its
 * station. The project chip is absent: the page is the project. */
function Strips({
  cards,
  stations,
  freshness,
  now,
}: Pick<Props, 'stations' | 'freshness' | 'now'> & { readonly cards: readonly DeckSession[] }) {
  return (
    <ul className="fd-int-sessions">
      {cards.map((card) => (
        <FleetStrip
          key={`${card.station_id}/${card.summary.session_id}`}
          card={card}
          stations={stations}
          freshness={freshness}
          now={now}
          showStation
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
  // The one general primary of the tab (§2.2): at the right of the Live row, or in the empty
  // state while no session runs (§4.7: the empty state holds the one primary).
  const newSession = (small: boolean) =>
    canDrive ? (
      <button
        type="button"
        className={small ? 'fd-primary fd-sm' : 'fd-primary'}
        onClick={() => onNewSession(project.station_id, project.path)}
      >
        <Icon of={Plus} /> New session in this project
      </button>
    ) : undefined;
  const endedFold =
    ended.length === 0 ? null : (
      // The fleet lane's Ended row: folded, "Ended" and its count.
      <section aria-label="Ended sessions">
        <details className="fd-lane-ended fd-int-ended">
          <summary>
            Ended <span className="fd-count">{ended.length}</span>
          </summary>
          <Strips cards={ended} stations={stations} freshness={freshness} now={now} />
        </details>
      </section>
    );
  const where = `${homeShort(project.path)} on ${stationName(project.station_id, stations)}`;
  return (
    <OwnProject id={project.project_id}>
      <div className="fd-int-tab" data-tab="sessions">
        {live.length === 0 ? (
          // No session runs: the empty state says what to do and holds the button; the ended
          // sessions fold under it.
          <EmptyState
            icon={Layers}
            title={own.length === 0 ? 'No sessions yet' : 'No live sessions'}
            body={
              canDrive
                ? `Start a session in ${where}.`
                : own.length === 0
                  ? 'The sessions of this project show here.'
                  : 'No session of this project runs now.'
            }
            action={newSession(false)}
          />
        ) : (
          <section aria-label="Live sessions">
            <div className="fd-pane-row fd-int-livehead">
              <h3 className="fd-label">
                Live <span className="fd-count">{live.length}</span>
              </h3>
              {newSession(true)}
            </div>
            <Strips cards={live} stations={stations} freshness={freshness} now={now} />
          </section>
        )}
        {endedFold}
      </div>
    </OwnProject>
  );
}
