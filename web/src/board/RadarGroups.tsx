// FG-7 (design §8.6, §10.5, §10.6): the radar panel grouped by project: a header per group, the
// "Will conflict" rows, the DL-306 "Conflict" rows of the group, and the footer notices.
import type { FileConflict, Project, SessionKey } from '../gen';
import type {
  RadarAnswer,
  RadarGroup,
  RadarNotice,
  SessionRef,
  WillConflict,
} from '../integrations/actions';
import { Icon, TriangleAlert } from '../ui/icons';
import { agoText } from './conflicts';

/** How many rows of each kind a group shows. */
const SHOWN = 3;

/** How many files a "Will conflict" row names. */
const FILES_SHOWN = 3;

type Props = {
  readonly radar: RadarAnswer;
  readonly projects: readonly Project[];
  readonly onOpen: (key: SessionKey) => void;
  /** Unix ms of "now" (tests). */
  readonly now?: number;
};

/** Whether the panel groups by project: some group belongs to a project. */
export const hasProjectGroups = (radar: RadarAnswer | null): radar is RadarAnswer =>
  radar?.groups.some((g) => g.project_id !== null) === true;

/** `<remote>/<default>` of a project, as the "Will conflict" row names the default branch. */
function defaultName(project: Project | undefined): string {
  if (project === undefined) return 'the default branch';
  const remote = project.git.push_remote;
  const branch = project.git.default_branch;
  return branch === null ? `the default branch of ${remote}` : `${remote}/${branch}`;
}

const refName = (r: SessionRef): string => r.callsign ?? r.title ?? r.session_id;

function Who({
  side,
  project,
  onOpen,
}: {
  readonly side: SessionRef | 'default';
  readonly project: Project | undefined;
  readonly onOpen: Props['onOpen'];
}) {
  if (side === 'default') return <span>{defaultName(project)}</span>;
  return (
    <button
      type="button"
      className="fd-namebtn"
      onClick={() => onOpen({ station_id: side.station_id, session_id: side.session_id })}
    >
      {refName(side)}
    </button>
  );
}

function WillRow({
  row,
  project,
  onOpen,
  now,
}: {
  readonly row: WillConflict;
  readonly project: Project | undefined;
  readonly onOpen: Props['onOpen'];
  readonly now: number;
}) {
  const more = row.files.length - FILES_SHOWN;
  return (
    <li className="fd-confstrip-line" aria-label={`Will conflict ${row.files[0] ?? ''}`}>
      <Icon of={TriangleAlert} size={14} />
      <span className="fd-badge" data-tone="hazard">
        Will conflict
      </span>
      <span className="fd-confstrip-file">{row.files.slice(0, FILES_SHOWN).join(', ')}</span>
      {more > 0 ? <span className="fd-confstrip-more">+{more}</span> : null}
      <span>—</span>
      <span className="fd-confstrip-who">
        <Who side={row.a} project={project} onOpen={onOpen} /> ×{' '}
        <Who side={row.b} project={project} onOpen={onOpen} />
      </span>
      <span className="fd-confstrip-age">{agoText(row.checked_at, now)}</span>
    </li>
  );
}

function SameRow({
  c,
  onOpen,
  now,
}: {
  readonly c: FileConflict;
  readonly onOpen: Props['onOpen'];
  readonly now: number;
}) {
  return (
    <li className="fd-confstrip-line" aria-label={`Conflict ${c.file}`}>
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
  );
}

/** The groups to show: the deck's groups, and a footer-only group for a project with a notice. */
function shownGroups(radar: RadarAnswer): RadarGroup[] {
  const groups = radar.groups.filter(
    (g) =>
      g.will_conflict.length > 0 ||
      g.same_files.length > 0 ||
      radar.notices.some((n) => n.project_id === g.project_id),
  );
  const have = new Set(radar.groups.map((g) => g.project_id));
  const extra = [...new Set(radar.notices.map((n) => n.project_id))]
    .filter((id) => !have.has(id))
    .map(
      (id): RadarGroup => ({ project_id: id, group_key: '', will_conflict: [], same_files: [] }),
    );
  return [...groups, ...extra];
}

/** Whether some group has a row or a notice to show. */
export const radarHasRows = (radar: RadarAnswer): boolean => shownGroups(radar).length > 0;

function Group({
  group,
  notices,
  projects,
  onOpen,
  now,
}: {
  readonly group: RadarGroup;
  readonly notices: readonly RadarNotice[];
  readonly projects: readonly Project[];
  readonly onOpen: Props['onOpen'];
  readonly now: number;
}) {
  const project = projects.find((p) => p.project_id === group.project_id);
  const title = group.project_id === null ? 'No project' : (project?.name ?? group.project_id);
  const will = group.will_conflict;
  const same = group.same_files;
  return (
    <>
      <li className="fd-confstrip-line fd-confstrip-hd">{title}</li>
      {will.slice(0, SHOWN).map((w) => (
        <WillRow
          key={`${w.a.session_id}/${w.b === 'default' ? 'default' : w.b.session_id}`}
          row={w}
          project={project}
          onOpen={onOpen}
          now={now}
        />
      ))}
      {will.length > SHOWN ? (
        <li className="fd-confstrip-line fd-confstrip-more">+{will.length - SHOWN} more</li>
      ) : null}
      {same.slice(0, SHOWN).map((c) => (
        <SameRow key={`${c.repo ?? ''}/${c.file}`} c={c} onOpen={onOpen} now={now} />
      ))}
      {same.length > SHOWN ? (
        <li className="fd-confstrip-line fd-confstrip-more">+{same.length - SHOWN} more</li>
      ) : null}
      {notices.map((n) => (
        <li key={n.code} className="fd-confstrip-line fd-confstrip-repo">
          {n.text}
        </li>
      ))}
    </>
  );
}

/** The grouped rows of the radar; `null` when no group has anything to show. */
export function RadarGroups({ radar, projects, onOpen, now = Date.now() }: Props) {
  const groups = shownGroups(radar);
  if (groups.length === 0) return null;
  const seen = new Set<string | null>();
  return (
    <ul className="fd-confstrip-lines">
      {groups.map((g) => {
        // A project with two groups shows its footer notices once, under its first group.
        const first = !seen.has(g.project_id);
        seen.add(g.project_id);
        return (
          <Group
            key={`${g.project_id ?? ''}/${g.group_key}`}
            group={g}
            notices={first ? radar.notices.filter((n) => n.project_id === g.project_id) : []}
            projects={projects}
            onOpen={onOpen}
            now={now}
          />
        );
      })}
    </ul>
  );
}
