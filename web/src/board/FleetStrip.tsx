import { type ReactNode, useLayoutEffect, useSyncExternalStore } from 'react';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import type { DeckSession, SessionExtras, StationView } from '../gen';
import { IssueChip } from '../integrations/IssueChip';
import { PrChip } from '../integrations/PrChip';
import { ProjectChip, useOwnProject } from '../integrations/ProjectChip';
import { useProjects } from '../integrations/context';
import type { Freshness } from '../runtime/state';
import { hasChanges } from '../session/changes';
import { runningSubagents } from '../session/timeline';
import { useDiffTotal } from '../session/workspace';
import { archived } from '../ui/archive';
import {
  addedText,
  compactCount,
  countText,
  isRiskyMode,
  modeLabel,
  modelFamily,
  removedText,
  shortModel,
} from '../ui/format';
import { digestLine, GeneratedMark } from '../ui/generated';
import {
  Archive,
  CloudOff,
  Folder,
  FolderGit2,
  GitBranch,
  Icon,
  ListTodo,
  type LucideIcon,
  Mail,
  Paperclip,
  ShieldAlert,
  SquareTerminal,
  Star,
  TriangleAlert,
  Workflow,
} from '../ui/icons';
import { priority, sessionRef } from '../ui/localSet';
import { UnreadMark, unreadOf } from '../views/marks';
import { CardNote, CompareChip } from './cardParts';
import type { Contested } from './fleet';
import { stationName } from './fleet';
import { API_PRICES, costText } from './usage';

// The changes column of the board (DESIGN-SYSTEM §4.1): absent until a strip of the board knows its
// changes, then there for the page life. A live event never takes it away again, so the where and
// now cells of every lane do not move when the last diff of the board goes.
let changesShown = false;
const changesListeners = new Set<() => void>();

/** Shows the changes column of the board from now on (a strip with known changes calls it). */
export function showChangesColumn(): void {
  if (changesShown) return;
  changesShown = true;
  for (const listener of [...changesListeners]) listener();
}

/** Whether the board has its changes column; re-renders when it comes. */
export function useChangesColumn(): boolean {
  return useSyncExternalStore(
    (listener) => {
      changesListeners.add(listener);
      return () => changesListeners.delete(listener);
    },
    () => changesShown,
    () => changesShown,
  );
}

/** The longest diff text that the changes column holds whole ("+12,345 −678"). */
const DIFF_CHARS = 12;

/**
 * The added and removed lines of a strip: `+1,286` `−400`, or the short form (`+18K` `−6.4K`) when
 * the whole numbers are longer than the column holds (§5). The title keeps the whole numbers.
 */
export function diffTexts(
  insertions: number,
  deletions: number,
): { readonly added: string; readonly removed: string; readonly whole: string } {
  const added = addedText(insertions);
  const removed = removedText(deletions);
  const whole = `${added} ${removed}`;
  if (whole.length <= DIFF_CHARS) return { added, removed, whole };
  return { added: `+${compactCount(insertions)}`, removed: `−${compactCount(deletions)}`, whole };
}

/**
 * One fact of the meta cell: a 12 px icon and an optional number (DESIGN-SYSTEM §4.1: the quiet
 * form of a counter or a flag; a chip that is a link of its own is a `.fd-badge`). The strip is a
 * link, so the fact is an image with a name ("Subagents running: 2"), not only a title.
 */
function Ico({
  kind,
  icon,
  title,
  n,
  tone,
  extra,
}: {
  readonly kind: string;
  readonly icon: LucideIcon;
  readonly title: string;
  readonly n?: number | undefined;
  readonly tone?: 'hazard' | undefined;
  readonly extra?: Record<string, number> | undefined;
}) {
  return (
    <span
      role="img"
      aria-label={n === undefined ? title : `${title}: ${n}`}
      className="fd-strip-ico"
      data-ico={kind}
      data-tone={tone}
      title={title}
      {...extra}
    >
      <Icon of={icon} size={12} />
      {n === undefined ? null : <span>{n}</span>}
    </span>
  );
}

/**
 * One session strip (DESIGN-SYSTEM §4.1): the holder, the callsign over the model, the title and
 * what the session does now, the facts, where it works, its changes and its age. The whole strip
 * is the link to the session; the chips that are links of their own lie over the where cell (a
 * second line under the branch, as in concept A). A strip without known changes has no changes
 * cell: its where cell spans the two columns, so the age stays in its column.
 */
export function FleetStrip({
  card,
  stations,
  freshness,
  now,
  showStation,
  contested,
  extra,
  batchCount = 0,
  hideMode = false,
}: {
  readonly card: DeckSession;
  readonly stations: readonly StationView[];
  readonly freshness: Freshness;
  readonly now: number;
  readonly showStation: boolean;
  readonly contested?: Contested | undefined;
  readonly extra?: SessionExtras | undefined;
  /** How many sessions of the card's batch are on the board (R1.4); the chip shows for two or more. */
  readonly batchCount?: number;
  /**
   * The lane head names the permission mode of every live strip of the lane (§4.1): the strip
   * shows no flag of its own and keeps the words for screen readers.
   */
  readonly hideMode?: boolean;
}) {
  const s = card.summary;
  const ref = sessionRef(card.station_id, s.session_id);
  const starred = priority.use().has(ref);
  const projects = useProjects();
  const cleared = archived.use().has(ref) && (card.status === 'ended' || card.status === 'lost');
  const subagents = runningSubagents(s);
  const total = useDiffTotal(ref);
  const digest = digestLine(s.digest);
  const cost = costText(s.usage);
  const files = extra?.files ?? [];
  const place =
    s.branch !== null && s.branch !== ''
      ? { text: s.branch, title: `Branch: ${s.branch}`, branch: true }
      : s.cwd !== null && s.cwd !== ''
        ? { text: basename(s.cwd), title: `Folder: ${basename(s.cwd)}`, branch: false }
        : null;
  const mail = extra?.mail_pending ?? 0;
  // The project of the session: a chip (a link of its own) on a wide strip; on a narrow strip a
  // quiet label of the meta line, so the strip stays two lines (§4.1, concept A phone). On the
  // page of its own project the strip names no project (the page is the project).
  const own = useOwnProject();
  const project = projects.find((p) => p.project_id === card.project_id && p.project_id !== own);
  const title = s.title ?? '';
  const changed = total !== undefined && hasChanges(total) ? total : null;
  const known = changed !== null;
  useLayoutEffect(() => {
    if (known) showChangesColumn();
  }, [known]);
  const diff = changed === null ? null : diffTexts(changed.insertions, changed.deletions);
  // A lane by repo or project names the station of each strip: the second line of the where
  // cell, under the branch (as the Needs you panel), with the link chips after it.
  const station = showStation ? stationName(card.station_id, stations) : null;
  const branchLine =
    place === null ? null : (
      <>
        <Icon of={place.branch ? GitBranch : Folder} size={12} />
        <span className="fd-strip-path">{place.text}</span>
      </>
    );
  const links: ReactNode[] = [
    <CompareChip key="compare" card={card} count={batchCount} />,
    <IssueChip key="issue" summary={s} projects={projects} station={card.station_id} />,
    <PrChip key="pr" card={card} />,
    <ProjectChip key="project" card={card} />,
  ];
  return (
    <li className="fd-stripwrap" data-status={card.status}>
      <a
        className="fd-strip"
        href={formatHash({
          name: 'session',
          station: card.station_id,
          session: s.session_id,
          tab: 'chat',
        })}
        data-session={s.session_id}
        data-station={card.station_id}
        data-status={card.status}
        data-contested={contested === undefined ? undefined : 'true'}
        data-archived={cleared ? 'true' : undefined}
        data-nochanges={changed === null ? 'true' : undefined}
        data-showstation={station === null ? undefined : 'true'}
      >
        <span className="fd-holder" data-status={card.status} aria-hidden="true" />
        <span className="fd-strip-id">
          <span className="fd-strip-call fd-callsign">
            {s.callsign ?? s.session_id.slice(0, 8)}
          </span>
          <span className="fd-strip-model" data-family={modelFamily(s.model) ?? undefined}>
            {shortModel(s.model)}
          </span>
        </span>
        <span className="fd-strip-main">
          <span
            className="fd-strip-title"
            title={digest === null ? undefined : `Summary: ${digest}`}
          >
            <span className="fd-dot" data-status={card.status} aria-hidden="true" />
            <span className="fd-strip-text">
              {title !== '' ? (
                title
              ) : place === null ? null : (
                <span className="fd-strip-place">{place.text}</span>
              )}
            </span>
            {s.title !== null && s.title_generated ? <GeneratedMark /> : null}
            {starred ? (
              <span className="fd-card-star" title="Priority">
                <Icon of={Star} size={12} />
              </span>
            ) : null}
            <UnreadMark count={unreadOf(card)} dot />
          </span>
          <span
            className="fd-strip-now"
            title={files.length === 0 ? undefined : `Touched: ${files.map(basename).join(', ')}`}
          >
            <CardNote card={card} focus />
          </span>
        </span>
        <span className="fd-strip-meta">
          {project === undefined ? null : (
            <span className="fd-strip-proj" title={`Project ${project.name}`}>
              <Icon of={FolderGit2} size={12} />
              <span className="fd-strip-proj-name">{project.name}</span>
            </span>
          )}
          {mail > 0 ? (
            <Ico kind="mail" icon={Mail} title="Mail waiting for delivery" n={mail} />
          ) : null}
          {subagents > 0 ? (
            <Ico
              kind="subagents"
              icon={Workflow}
              title="Subagents running"
              n={subagents}
              extra={{ 'data-running-subagents': subagents }}
            />
          ) : null}
          {s.artifacts_count > 0 ? (
            <Ico
              kind="artifacts"
              icon={Paperclip}
              title="Artifacts"
              n={s.artifacts_count}
              extra={{ 'data-artifacts': s.artifacts_count }}
            />
          ) : null}
          {s.background_tasks.length > 0 ? (
            <Ico
              kind="background"
              icon={ListTodo}
              title="Background tasks"
              n={s.background_tasks.length}
            />
          ) : null}
          {s.terminal != null ? (
            <Ico kind="terminal" icon={SquareTerminal} title="In terminal" />
          ) : null}
          {cleared ? <Ico kind="archived" icon={Archive} title="Archived" /> : null}
          {freshness === 'stale' ? <Ico kind="stale" icon={CloudOff} title="Stale" /> : null}
          {/* The hazard flags come last: they sit against the where cell in every strip, so a
              lane reads its risky sessions down one column; the counters grow to their left. */}
          {!isRiskyMode(s.mode) ? null : hideMode ? (
            <span className="fd-vh">Permission mode: {modeLabel(s.mode ?? '')}</span>
          ) : (
            <Ico
              kind="risky"
              icon={ShieldAlert}
              tone="hazard"
              title={`Permission mode: ${modeLabel(s.mode ?? '')}`}
            />
          )}
          {contested === undefined ? null : (
            <Ico
              kind="contested"
              icon={TriangleAlert}
              tone="hazard"
              title={`Contested with ${contested.with.join(', ')}`}
            />
          )}
        </span>
        {station === null ? (
          <span className="fd-strip-where" title={place?.title}>
            {branchLine}
          </span>
        ) : (
          <span className="fd-strip-where" title={place?.title} data-station="true">
            {place === null ? null : <span className="fd-strip-wline">{branchLine}</span>}
            <span className="fd-strip-station">{station}</span>
          </span>
        )}
        {changed === null || diff === null ? null : (
          <span className="fd-strip-changes">
            <span
              className="fd-strip-diff"
              title={`Changed since the session started: ${diff.whole}`}
            >
              <span className="fd-add">{diff.added}</span>{' '}
              <span className="fd-del">{diff.removed}</span>
            </span>
            <span className="fd-strip-files">
              {changed.files_changed === 1 ? '1 file' : `${countText(changed.files_changed)} files`}
            </span>
          </span>
        )}
        <span className="fd-strip-age" title={cost === null ? undefined : `${cost} ${API_PRICES}`}>
          {relativeAge(s.updated_at, now)}
        </span>
      </a>
      <span className="fd-strip-links">
        {station === null ? null : (
          // The chips start after the station name of the where cell: an invisible copy of the
          // name keeps its width (the visible name is in the link, for its accessible name).
          <span className="fd-strip-spacer" aria-hidden="true">
            {station}
          </span>
        )}
        {links}
      </span>
    </li>
  );
}
