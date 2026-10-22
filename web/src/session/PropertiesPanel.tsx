// "Flight Deck" §4.5: the properties panel at the right of a session. It shows only what the
// session summary (and the timeline that the pane already read) knows; a missing fact is absent.
import { type ReactNode, useSyncExternalStore } from 'react';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import { costText, rateMeters, SCOPE_TEXT, WARN_PERCENT } from '../board/usage';
import type { DeckSession, DiffSummary, StationView } from '../gen';
import { isRiskyMode, modeLabel, shortModel } from '../ui/format';
import { ChevronRight, Icon, ShieldAlert } from '../ui/icons';
import { sessionRef } from '../ui/localSet';
import { useNow } from '../ui/useNow';
import { homeShort } from './CopyButton';
import { StatusFact } from './HeaderBadges';
import { runningSubagents } from './timeline';
import { type FinalizedTurn, useDiffPaths, useDiffTotal } from './workspace';

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

/** The size of the session's changes: the loaded session diff, else the sum of its kept turns. */
function changeTotal(
  loaded: DiffSummary | undefined,
  turns: readonly FinalizedTurn[],
): DiffSummary | null {
  if (loaded !== undefined) return loaded;
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

function Row({ label, children }: { readonly label: string; readonly children: ReactNode }) {
  return (
    <div className="fd-props-row">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly turns: readonly FinalizedTurn[];
  /** The subagents that the timeline shows. */
  readonly subagents: number;
  /** A changed file opens the Changes tab on that file. */
  readonly onOpenChange: (path: string) => void;
};

/** The changed files that the Changes row lists; more show as "+N more". */
export const PROPS_FILES_MAX = 5;

/** The first changed files of the loaded session diff: a press opens the Changes tab on one. */
export function ChangedList({
  paths,
  onOpen,
}: {
  readonly paths: readonly string[] | undefined;
  readonly onOpen: (path: string) => void;
}) {
  if (paths === undefined || paths.length === 0) return null;
  const more = paths.length - PROPS_FILES_MAX;
  return (
    <ul className="fd-props-files" aria-label="Changed files">
      {paths.slice(0, PROPS_FILES_MAX).map((path) => (
        <li key={path}>
          <button type="button" className="fd-ghost" title={path} onClick={() => onOpen(path)}>
            <code className="fd-props-file">
              <bdi>{path}</bdi>
            </code>
            <Icon of={ChevronRight} size={14} />
          </button>
        </li>
      ))}
      {more > 0 ? <li className="fd-props-more">+{more} more</li> : null}
    </ul>
  );
}

/** The properties of one session: status, place, agent, changes, usage and subagents. */
export function PropertiesPanel({ session, stations, turns, subagents, onOpenChange }: Props) {
  const s = session.summary;
  const now = useNow(undefined, 30000);
  const station = stations.find((st) => st.station_id === session.station_id);
  const ref = sessionRef(session.station_id, s.session_id);
  const total = changeTotal(useDiffTotal(ref), turns);
  const paths = useDiffPaths(ref);
  const { input_tokens: input, output_tokens: output } = s.usage;
  const cost = costText(s.usage);
  const meters = rateMeters([session], stations);
  const running = runningSubagents(s);
  const agents = Math.max(subagents, running);
  const provider = s.provider === null ? null : providerLabel(s.provider);
  return (
    <aside className="fd-props" aria-label="Properties">
      <h3 className="fd-props-title">Properties</h3>
      <dl className="fd-props-list">
        <Row label="Status">
          <StatusFact session={session} />
          <span className="fd-props-aside">{relativeAge(s.updated_at, now)}</span>
        </Row>
        <Row label="Station">
          <span className="fd-props-station">{stationName(session.station_id, stations)}</span>
          {station === undefined ? null : (
            <span className="fd-props-aside" data-up={station.reachable}>
              <span
                className="fd-dot"
                data-status={station.reachable ? 'online' : 'unreachable'}
                aria-hidden="true"
              />
              {station.reachable ? 'online' : 'offline'}
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
              <bdi>{homeShort(s.cwd)}</bdi>
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
        {total === null ? null : (
          <Row label="Changes">
            {total.insertions === 0 && total.deletions === 0 ? (
              <span className="fd-props-aside">No file changes</span>
            ) : (
              <>
                <span className="fd-add">+{total.insertions}</span>
                <span className="fd-del">−{total.deletions}</span>
                <span className="fd-props-aside">
                  {total.files_changed} {total.files_changed === 1 ? 'file' : 'files'}
                </span>
                <ChangedList paths={paths} onOpen={onOpenChange} />
              </>
            )}
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
        {agents === 0 ? null : (
          <Row label="Subagents">
            {agents}
            {running > 0 ? <span className="fd-props-aside">{running} running</span> : null}
          </Row>
        )}
      </dl>
    </aside>
  );
}
