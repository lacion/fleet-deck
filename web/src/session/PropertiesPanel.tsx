// "Flight Deck" §4.5: the properties panel at the right of a session. It shows only what the
// session summary (and the timeline that the pane already read) knows; a missing fact is absent.
import { Fragment, type ReactNode, useEffect, useRef, useSyncExternalStore } from 'react';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import { costText, rateMeters, SCOPE_TEXT, WARN_PERCENT } from '../board/usage';
import type { DeckSession, DiffSummary, StationView } from '../gen';
import type { Freshness } from '../runtime/state';
import { isRiskyMode, modeLabel, shortModel } from '../ui/format';
import { ChevronRight, Icon, ShieldAlert } from '../ui/icons';
import { sessionRef } from '../ui/localSet';
import { useNow } from '../ui/useNow';
import { homeShort } from './CopyButton';
import { loadSessionChanges, turnForPath } from './changedFiles';
import { StatusFact } from './HeaderBadges';
import { FilePath } from './PathFit';
import {
  type FileActions,
  type FinalizedTurn,
  setDiffTotal,
  useDiffPaths,
  useTurnPaths,
} from './workspace';

// The open state lives for the page: every session opens the panel the way the last one left it.
let open = true;
const listeners = new Set<() => void>();

/** The open state of the properties panel and its toggle (not stored: a reload opens it). */
export function usePropsOpen(): readonly [boolean, () => void] {
  const now = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => open,
    () => open,
  );
  return [
    now,
    () => {
      open = !open;
      for (const l of listeners) l();
    },
  ];
}

/** `182000` → `182 k`, `9400` → `9.4 k`, `1250000` → `1.3 M`. */
export function compactCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')} k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)} k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')} M`;
}

/** `five_hour` → `Claude 5-hour window`, `weekly` → `Claude weekly window`; no provider: `Weekly window`. */
export function scopeLabel(scope: string, provider: string | null): string {
  const words = SCOPE_TEXT[scope] ?? `${scope.replace(/_/g, ' ')} window`;
  if (provider === null) return `${words.charAt(0).toUpperCase()}${words.slice(1)}`;
  return `${provider} ${words}`;
}

/** A slot meter draws one tick per slot up to this many; a bigger station shows the count only. */
const MAX_TICKS = 16;

/** The used slots of a station as ticks and "3 of 12"; absent when the station does not say. */
function StationSlots({ station }: { readonly station: StationView }) {
  const max = station.info?.max_sessions ?? null;
  if (max === null || max <= 0) return null;
  const live = station.info?.live_sessions ?? 0;
  const used = Math.min(live, max);
  return (
    <span className="fd-props-slots">
      {max <= MAX_TICKS ? (
        <span className="fd-slots" aria-hidden="true">
          {Array.from({ length: max }, (_, i) => (
            <span key={i} data-slot={i < used ? 'used' : undefined} />
          ))}
        </span>
      ) : null}
      <span className="fd-props-aside">
        {live} of {max}
      </span>
    </span>
  );
}

/**
 * The size of the session's changes: the loaded session diff. While that diff loads (`pending`: the
 * session has a spawn base and the board can read its files) the size is not known, so it is
 * absent: the sum of the turn diffs misses the files of a shared folder and would contradict the
 * diff a moment later. Without a session diff, the sum of the kept turns.
 */
export function changeTotal(
  loaded: DiffSummary | undefined,
  turns: readonly FinalizedTurn[],
  pending: boolean,
): DiffSummary | null {
  if (loaded !== undefined) return loaded;
  if (pending) return null;
  const kept = turns.filter((t) => !t.reverted && t.diff !== null);
  if (kept.length === 0) return null;
  return kept.reduce<DiffSummary>(
    (sum, t) => ({
      files_changed: sum.files_changed + (t.diff?.files_changed ?? 0),
      insertions: sum.insertions + (t.diff?.insertions ?? 0),
      deletions: sum.deletions + (t.diff?.deletions ?? 0),
    }),
    { files_changed: 0, insertions: 0, deletions: 0 },
  );
}

/** One fact; `extra` is a second `<dd>` that takes the whole width of the panel under it. */
function Row({
  label,
  children,
  extra,
}: {
  readonly label: string;
  readonly children: ReactNode;
  readonly extra?: ReactNode;
}) {
  return (
    <div className="fd-props-row">
      <dt>{label}</dt>
      <dd>{children}</dd>
      {extra}
    </div>
  );
}

/**
 * A path that breaks only after a `/`: each folder name (with its slash) is one unbreakable piece,
 * so a line never ends inside a name (not even after a hyphen of `fleet-deck-worktrees`).
 */
function Breakable({ path }: { readonly path: string }) {
  const parts = path.split('/');
  return (
    <>
      {parts.map((part, i) => {
        const last = i === parts.length - 1;
        return (
          // The parts of one path never move: the position is stable.
          <Fragment key={i}>
            <span className="fd-props-seg">{last ? part : `${part}/`}</span>
            {last ? null : <wbr />}
          </Fragment>
        );
      })}
    </>
  );
}

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly turns: readonly FinalizedTurn[];
  /** The size of the session's changes (`changeTotal`, the same total as the Changes tab label). */
  readonly total: DiffSummary | null;
  /** The subagents of the session, and the running count of the Agents bar (it counts the running
      background commands too): the row and the bar show one number. */
  readonly agents: number;
  readonly runningAgents: number;
  /** A changed file opens the Changes tab on that file. */
  readonly onOpenChange: (path: string) => void;
  /** "+N more files" opens the Changes tab. */
  readonly onOpenAll?: (() => void) | undefined;
  /** The board connection: a stale board does not say that the station is online. */
  readonly freshness?: Freshness | undefined;
  /** The fleet-control fact of a coordinator session (a link that opens its answers). */
  readonly fleet?: ReactNode;
};

/** The changed files that the Changes row lists; the others are "+N more files". */
export const PROPS_FILES_MAX = 5;

/**
 * The first changed files of the loaded session diff: a press opens the Changes tab on one. `files`
 * is the count of the whole diff (a cut patch lists fewer paths): "+N more files" counts from it
 * and opens the Changes tab.
 */
export function ChangedList({
  paths,
  files = 0,
  onOpen,
  onOpenAll,
  opens,
}: {
  readonly paths: readonly string[] | undefined;
  readonly files?: number;
  readonly onOpen: (path: string) => void;
  readonly onOpenAll?: (() => void) | undefined;
  /** A file that no diff can open shows as plain text; absent: every file opens. */
  readonly opens?: ((path: string) => boolean) | undefined;
}) {
  if (paths === undefined || paths.length === 0) return null;
  const listed = paths.slice(0, PROPS_FILES_MAX);
  const more = Math.max(files, paths.length) - listed.length;
  const moreText = `+${more} more ${more === 1 ? 'file' : 'files'}`;
  return (
    <ul className="fd-props-files" aria-label="Changed files">
      {listed.map((path) => (
        <li key={path}>
          {opens === undefined || opens(path) ? (
            <button type="button" className="fd-ghost" title={path} onClick={() => onOpen(path)}>
              <code className="fd-props-file fd-dl-path">
                <FilePath path={path} />
              </code>
              <Icon of={ChevronRight} size={14} />
            </button>
          ) : (
            <code className="fd-props-file fd-dl-path" title={path}>
              <FilePath path={path} />
            </code>
          )}
        </li>
      ))}
      {more <= 0 ? null : (
        <li className="fd-props-more">
          {onOpenAll === undefined ? (
            moreText
          ) : (
            <button type="button" className="fd-link fd-props-link" onClick={() => onOpenAll()}>
              {moreText}
            </button>
          )}
        </li>
      )}
    </ul>
  );
}

/** `2 min` → `since 2 min`; a change under a minute ago: `just now`. */
export function sinceText(age: string): string {
  return age === 'now' ? 'just now' : `since ${age}`;
}

/**
 * Reads the session diff when the session opens, and again after each finalized turn: its size
 * (the Changes row and the Changes tab label) and its changed files (the list of the Changes row)
 * are there on every tab and every layout (the Files and Changes tabs read it too). No file
 * service, or no spawn base (the station has no session diff then): nothing.
 */
export function useSessionChanges(
  ref: string,
  files: FileActions | null | undefined,
  hasBase: boolean,
  turns: number,
): void {
  // The actions object changes on every render of the parent; the effect keys on "has files".
  const latest = useRef(files);
  latest.current = files;
  const has = files != null && hasBase;
  useEffect(() => {
    const current = latest.current;
    if (!has || !current) return undefined;
    let live = true;
    // A read of the full session diff replaces the paths, an empty result too.
    void loadSessionChanges(current).then((found) => {
      if (live && found !== null) setDiffTotal(ref, found.summary, found.all);
    });
    return () => {
      live = false;
    };
  }, [has, ref, turns]);
}

/** The properties of one session: status, place, agent, changes, usage and agents. */
export function PropertiesPanel({
  session,
  stations,
  turns,
  total,
  agents,
  runningAgents,
  onOpenChange,
  onOpenAll,
  freshness = 'live',
  fleet,
}: Props) {
  const s = session.summary;
  const now = useNow(undefined, 30000);
  const station = stations.find((st) => st.station_id === session.station_id);
  const up = freshness !== 'stale' && station?.reachable === true;
  const ref = sessionRef(session.station_id, s.session_id);
  const paths = useDiffPaths(ref);
  // Without a spawn base only a file that a loaded turn diff lists opens (that turn's diff).
  const turnPaths = useTurnPaths(ref);
  const hasBase = Boolean(s.base);
  const opens = (path: string) => hasBase || turnForPath(turns, turnPaths, path) !== undefined;
  const { input_tokens: input, output_tokens: output } = s.usage;
  const cost = costText(s.usage);
  const meters = rateMeters([session], stations);
  const provider = s.provider === null ? null : providerLabel(s.provider);
  return (
    <aside className="fd-props" aria-label="Properties">
      <h3 className="fd-props-title">Properties</h3>
      <dl className="fd-props-list">
        <Row label="Status">
          <StatusFact session={session} />
          <span className="fd-props-aside">{sinceText(relativeAge(s.updated_at, now))}</span>
        </Row>
        <Row label="Station">
          <span className="fd-props-station">{stationName(session.station_id, stations)}</span>
          {station === undefined ? null : (
            <span className="fd-props-aside" data-up={up}>
              <span
                className="fd-dot"
                data-status={up ? 'online' : 'unreachable'}
                aria-hidden="true"
              />
              {up ? 'online' : 'offline'}
            </span>
          )}
          {station === undefined ? null : <StationSlots station={station} />}
        </Row>
        {s.branch ? (
          <Row label="Branch">
            <code className="fd-props-mono">{s.branch}</code>
          </Row>
        ) : null}
        {s.cwd ? (
          <Row label="Folder">
            <code className="fd-props-mono" data-path="" title={s.cwd}>
              <Breakable path={homeShort(s.cwd)} />
            </code>
          </Row>
        ) : null}
      </dl>
      <dl className="fd-props-list">
        <Row label="Model">
          <span title={s.model ?? 'default model'}>{shortModel(s.model)}</span>
        </Row>
        {s.provider === null ? null : <Row label="Provider">{providerLabel(s.provider)}</Row>}
        {s.mode ? (
          <Row label="Mode">
            <span
              className="fd-props-mode"
              data-tone={isRiskyMode(s.mode) ? 'hazard' : undefined}
              title={`Permission mode: ${s.mode}`}
            >
              {isRiskyMode(s.mode) ? <Icon of={ShieldAlert} size={13} /> : null}
              {modeLabel(s.mode)}
            </span>
          </Row>
        ) : null}
      </dl>
      <dl className="fd-props-list">
        {total === null ? null : total.files_changed === 0 ? (
          <Row label="Changes">
            <span className="fd-props-aside">No file changes</span>
          </Row>
        ) : (
          <Row
            label="Changes"
            extra={
              paths === undefined || paths.length === 0 ? null : (
                <dd className="fd-props-filelist">
                  <ChangedList
                    paths={paths}
                    files={total.files_changed}
                    onOpen={onOpenChange}
                    onOpenAll={onOpenAll}
                    opens={opens}
                  />
                </dd>
              )
            }
          >
            {total.insertions > 0 ? <span className="fd-add">+{total.insertions}</span> : null}
            {total.deletions > 0 ? <span className="fd-del">−{total.deletions}</span> : null}
            <span className="fd-props-aside">
              {total.files_changed} {total.files_changed === 1 ? 'file' : 'files'}
            </span>
          </Row>
        )}
        {input === null && output === null ? null : (
          <Row label="Tokens">
            <span className="fd-props-fuel">
              {[
                input === null ? null : `${compactCount(input)} in`,
                output === null ? null : `${compactCount(output)} out`,
              ]
                .filter((part) => part !== null)
                .join(' · ')}
            </span>
          </Row>
        )}
        {cost === null ? null : (
          <Row label="Cost">
            <span className="fd-props-fuel">{cost}</span>
          </Row>
        )}
        {meters.map((m) =>
          m.usedPercent === null ? null : (
            <div key={m.key} className="fd-props-meter">
              <span className="fd-props-meter-label">
                <span>{scopeLabel(m.scope, provider)}</span>
                <span className="fd-props-fuel">{Math.round(m.usedPercent)}%</span>
              </span>
              <span
                className="fd-meter"
                data-level={m.usedPercent >= WARN_PERCENT ? 'high' : undefined}
                aria-hidden="true"
              >
                <span style={{ width: `${Math.min(100, Math.max(0, m.usedPercent))}%` }} />
              </span>
            </div>
          ),
        )}
        {agents === 0 && runningAgents === 0 ? null : (
          <Row label="Agents">
            {agents > 0 ? agents : null}
            {runningAgents > 0 ? (
              <span className="fd-props-aside">{runningAgents} running</span>
            ) : null}
          </Row>
        )}
        {fleet == null ? null : <Row label="Fleet control">{fleet}</Row>}
      </dl>
    </aside>
  );
}
