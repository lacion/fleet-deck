import type { ReactNode } from 'react';
import { formatHash } from '../app/routes';
import { basename, relativeAge } from '../app/sessions';
import type { DeckSession, SessionExtras, StationView } from '../gen';
import { IssueChip } from '../integrations/IssueChip';
import { PrChip } from '../integrations/PrChip';
import { ProjectChip } from '../integrations/ProjectChip';
import { useProjects } from '../integrations/context';
import type { Freshness } from '../runtime/state';
import { hasChanges } from '../session/changes';
import { runningSubagents } from '../session/timeline';
import { useDiffTotal } from '../session/workspace';
import { archived } from '../ui/archive';
import { isRiskyMode, modelFamily, shortModel } from '../ui/format';
import { digestLine, GeneratedMark } from '../ui/generated';
import {
  Archive,
  CloudOff,
  Icon,
  Layers,
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
import { costText } from './usage';

/** One fact of the meta cell: a 13 px icon and an optional number. */
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
    <span className="fd-strip-ico" data-ico={kind} data-tone={tone} title={title} {...extra}>
      <Icon of={icon} size={13} />
      {n === undefined ? null : <span>{n}</span>}
    </span>
  );
}

/** One session strip: the whole strip is the link to the session (ui-pass §3). */
export function FleetStrip({
  card,
  stations,
  freshness,
  now,
  showStation,
  contested,
  extra,
  batchCount = 0,
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
      ? { text: s.branch, title: `Branch: ${s.branch}` }
      : s.cwd !== null && s.cwd !== ''
        ? { text: basename(s.cwd), title: `Folder: ${basename(s.cwd)}` }
        : null;
  const mail = extra?.mail_pending ?? 0;
  const title = s.title ?? '';
  const links: ReactNode[] = [
    <CompareChip key="compare" card={card} count={batchCount} />,
    <IssueChip key="issue" summary={s} projects={projects} station={card.station_id} />,
    <PrChip key="pr" card={card} />,
    <ProjectChip key="project" card={card} />,
  ];
  return (
    <li className="fd-stripwrap">
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
      >
        <span className="fd-strip-call" title={place?.title}>
          {s.callsign ?? s.session_id.slice(0, 8)}
        </span>
        <span className="fd-strip-title" title={digest === null ? undefined : `Summary: ${digest}`}>
          {showStation ? (
            <span className="fd-strip-station">{stationName(card.station_id, stations)}</span>
          ) : null}
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
          <UnreadMark count={unreadOf(card)} />
        </span>
        <span
          className="fd-strip-now"
          title={files.length === 0 ? undefined : `Touched: ${files.map(basename).join(', ')}`}
        >
          <CardNote card={card} focus />
        </span>
        <span className="fd-strip-meta">
          {isRiskyMode(s.mode) ? (
            <Ico
              kind="risky"
              icon={ShieldAlert}
              tone="hazard"
              title={`Permission mode: ${s.mode}`}
            />
          ) : null}
          {contested === undefined ? null : (
            <Ico
              kind="contested"
              icon={TriangleAlert}
              tone="hazard"
              title={`Contested with ${contested.with.join(', ')}`}
            />
          )}
          {total === undefined || !hasChanges(total) ? null : (
            <span className="fd-strip-diff" title="Changed since the session started">
              <span className="fd-add">+{total.insertions}</span>{' '}
              <span className="fd-del">−{total.deletions}</span>
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
              icon={Layers}
              title="Background tasks"
              n={s.background_tasks.length}
            />
          ) : null}
          {s.terminal != null ? (
            <Ico kind="terminal" icon={SquareTerminal} title="In terminal" />
          ) : null}
          {cleared ? <Ico kind="archived" icon={Archive} title="Archived" /> : null}
          {freshness === 'stale' ? <Ico kind="stale" icon={CloudOff} title="Stale" /> : null}
        </span>
        <span className="fd-strip-model" data-family={modelFamily(s.model) ?? undefined}>
          {shortModel(s.model)}
        </span>
        <span className="fd-strip-age" title={cost === null ? undefined : `Cost ${cost}`}>
          {relativeAge(s.updated_at, now)}
        </span>
      </a>
      <span className="fd-strip-links">{links}</span>
    </li>
  );
}
