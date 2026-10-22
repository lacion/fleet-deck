// The presentational parts of the New session sheet (DESIGN-SYSTEM.md §4.6): the reach of the
// chosen station, the clone fields and the footer. Moved out of `SpawnForm.tsx` for the module
// size rule; the form keeps every state and every handler.
import type { ReactNode } from 'react';
import type { StationView } from '../gen';
import type { SpawnOutcome } from './spawn';

/** The most slot ticks that the sheet draws; a larger station shows the count only. */
const MAX_TICKS = 16;

/** The reach and the free slots of the chosen station, next to its select. */
export function StationReach({ station }: { readonly station: StationView | undefined }) {
  if (station === undefined) return null;
  const max = station.info?.max_sessions ?? null;
  const used = Math.min(station.info?.live_sessions ?? 0, max ?? Number.MAX_SAFE_INTEGER);
  const free = max === null ? null : Math.max(0, max - used);
  return (
    <span className="fd-spawn-reach" data-up={station.reachable}>
      <span className="fd-spawn-reach-dot" aria-hidden="true" />
      {station.reachable ? 'online' : 'offline'}
      {max !== null && max > 0 && max <= MAX_TICKS ? (
        <span className="fd-slots" aria-hidden="true">
          {Array.from({ length: max }, (_, i) => (
            <span key={i} data-slot={i < used ? 'used' : undefined} />
          ))}
        </span>
      ) : null}
      {free === null ? null : <span className="fd-spawn-free">{free} free</span>}
    </span>
  );
}

/** One property row of the sheet: the key at the left (Agent, Where), the fields at the right. */
export function PropRow({
  head,
  children,
}: {
  readonly head: string;
  readonly children: ReactNode;
}) {
  return (
    <div className="fd-spawn-prop">
      <h3 className="fd-label">{head}</h3>
      <div className="fd-spawn-fields">{children}</div>
    </div>
  );
}

/** The repository, its access check, and the new directory name of a clone start (DL-144). */
export function CloneFields({
  repoText,
  onRepo,
  onRepoSeen,
  access,
  repoError,
  repoAlert,
  cloneUrl,
  cloneName,
  onName,
}: {
  readonly repoText: string;
  readonly onRepo: (text: string) => void;
  readonly onRepoSeen: () => void;
  /** The "Check access" button and its result line. */
  readonly access: ReactNode;
  readonly repoError: string | null;
  /** The error line is an alert once the owner left the field or pressed Start. */
  readonly repoAlert: boolean;
  readonly cloneUrl: string;
  readonly cloneName: string;
  readonly onName: (name: string) => void;
}) {
  return (
    <div className="fd-spawn-clone">
      <div className="fd-spawn-url">
        <label>
          Repository
          <input
            value={repoText}
            placeholder="owner/name, host/owner/name or a URL"
            spellCheck={false}
            autoCapitalize="off"
            onChange={(event) => onRepo(event.target.value)}
            onBlur={onRepoSeen}
          />
        </label>
        {access}
      </div>
      <p className="fd-note" role={repoError !== null && repoAlert ? 'alert' : undefined}>
        {repoError ?? `Clones ${cloneUrl}`}
      </p>
      <label>
        New directory name
        <input value={cloneName} onChange={(event) => onName(event.target.value)} />
      </label>
    </div>
  );
}

/**
 * The footer of the sheet: the Batch switch, why the start is off (or the outcome), Cancel and the
 * primary start button.
 */
export function SpawnFoot({
  batchShown,
  batch,
  onBatch,
  missing,
  busy,
  outcome,
  onOpenNew,
  disabled,
  label,
}: {
  /** A clone is one session: no Batch switch then. */
  readonly batchShown: boolean;
  readonly batch: boolean;
  readonly onBatch: (on: boolean) => void;
  /** The missing steps: the button names them while it is off. */
  readonly missing: readonly string[];
  /** A start runs: the missing steps are not shown. */
  readonly busy: boolean;
  readonly outcome: SpawnOutcome | null;
  /** "Open the new session", when the outcome has a session to open. */
  readonly onOpenNew: (() => void) | null;
  readonly disabled: boolean;
  readonly label: string;
}) {
  return (
    <div className="fd-spawn-foot">
      {batchShown ? (
        <label className="fd-spawn-batch">
          <input
            type="checkbox"
            role="switch"
            className="fd-switch"
            checked={batch}
            aria-checked={batch}
            onChange={(event) => onBatch(event.target.checked)}
          />
          Batch: one session per line
        </label>
      ) : null}
      <div className="fd-spawn-status">
        {busy || missing.length === 0 ? null : (
          <p className="fd-note" id="fd-spawn-missing" data-missing>
            To start: {missing.join('; ')}.
          </p>
        )}
        {outcome === null ? null : (
          <div className="fd-outcome">
            <p role={outcome.ok ? 'status' : 'alert'}>{outcome.text}</p>
            {onOpenNew === null ? null : (
              <button type="button" className="fd-sm" onClick={onOpenNew}>
                Open the new session
              </button>
            )}
          </div>
        )}
      </div>
      <div className="fd-spawn-actions">
        <a className="fd-btn fd-ghost" href="#/">
          Cancel
        </a>
        <button
          type="submit"
          className="fd-primary"
          disabled={disabled}
          aria-describedby={missing.length === 0 ? undefined : 'fd-spawn-missing'}
        >
          {label}
        </button>
      </div>
    </div>
  );
}
