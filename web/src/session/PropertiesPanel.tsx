// "Flight Deck" §4.5: the properties panel at the right of a session. It shows only what the
// session summary (and the timeline that the pane already read) knows; a missing fact is absent.
import { type ReactNode, useSyncExternalStore } from 'react';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import { costText, rateMeters, WARN_PERCENT } from '../board/usage';
import type { DeckSession, DiffSummary, StationView } from '../gen';
import { isRiskyMode, shortModel } from '../ui/format';
import { Icon, ShieldAlert } from '../ui/icons';
import { sessionRef } from '../ui/localSet';
import { useNow } from '../ui/useNow';
import { StatusFact } from './HeaderBadges';
import { runningSubagents } from './timeline';
import { type FinalizedTurn, useDiffTotal } from './workspace';

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

/** `five_hour` → `Five hour window` (sentence case). */
function scopeLabel(scope: string): string {
  const words = scope.replace(/_/g, ' ');
  return `${words.charAt(0).toUpperCase()}${words.slice(1)} window`;
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
};

/** The properties of one session: status, place, agent, changes, usage and subagents. */
export function PropertiesPanel({ session, stations, turns, subagents }: Props) {
  const s = session.summary;
  const now = useNow(undefined, 30000);
  const station = stations.find((st) => st.station_id === session.station_id);
  const total = changeTotal(useDiffTotal(sessionRef(session.station_id, s.session_id)), turns);
  const { input_tokens: input, output_tokens: output } = s.usage;
  const cost = costText(s.usage);
  const meters = rateMeters([session], stations);
  const running = runningSubagents(s);
  const agents = Math.max(subagents, running);
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
              <span className="fd-props-reach" aria-hidden="true" />
              {station.reachable ? 'online' : 'offline'}
            </span>
          )}
        </Row>
        {s.branch ? (
          <Row label="Branch">
            <code className="fd-props-mono">{s.branch}</code>
          </Row>
        ) : null}
        {s.cwd ? (
          <Row label="Folder">
            <code className="fd-props-mono" title={s.cwd}>
              {s.cwd}
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
            <span className="fd-props-mode" data-tone={isRiskyMode(s.mode) ? 'hazard' : undefined}>
              {isRiskyMode(s.mode) ? <Icon of={ShieldAlert} size={13} /> : null}
              {s.mode}
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
                <span>{scopeLabel(m.scope)}</span>
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
