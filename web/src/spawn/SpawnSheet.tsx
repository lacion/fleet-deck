// The presentational parts of the New session sheet (DESIGN-SYSTEM.md §4.6): the breadcrumb head,
// the segmented radio controls, the station pills, the clone fields and the footer. Moved out of
// `SpawnForm.tsx` for the module size rule; the form keeps every state and every handler.
import { type ReactNode, useEffect, useId, useState } from 'react';
import type { StationView } from '../gen';
import { ChevronRight, Icon, Server, Sparkles } from '../ui/icons';
import { submitKeyHint } from './keyHint';
import { AUTO, type SpawnOutcome } from './spawn';

/** The head of the sheet: a breadcrumb "<station> › New session" (the station when one is chosen). */
export function SheetHead({ station }: { readonly station: StationView | undefined }) {
  return (
    <div className="fd-spawn-head">
      {station === undefined ? null : (
        <>
          <span className="fd-badge">
            <Icon of={Server} />
            {station.name}
          </span>
          <span className="fd-spawn-crumb" aria-hidden="true">
            <Icon of={ChevronRight} size={14} />
          </span>
        </>
      )}
      <h2>New session</h2>
    </div>
  );
}

/** One choice of a segmented control: the value, the short text, and the long text as a tooltip. */
export type SegOption<T extends string> = {
  readonly value: T;
  readonly label: ReactNode;
  readonly title?: string | undefined;
};

/**
 * A segmented control of native radios (`.fd-seg`, DESIGN-SYSTEM §3): one `radiogroup` named by
 * `label`, the arrow keys move the choice.
 */
export function Seg<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  readonly label: string;
  readonly value: T;
  readonly options: readonly SegOption<T>[];
  readonly onChange: (value: T) => void;
}) {
  const name = useId();
  return (
    <div className="fd-seg fd-spawn-seg" role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <label key={o.value} title={o.title}>
          <input
            type="radio"
            name={name}
            value={o.value}
            checked={o.value === value}
            onChange={() => onChange(o.value)}
          />
          {o.label}
        </label>
      ))}
    </div>
  );
}

/** The tooltip of the Auto pill: the deck places by the needs, the preferences and the room. */
const AUTO_TITLE = 'The deck picks the station that fits best';

/** The free slots of a station, or `null` when it has no limit. */
function freeSlots(station: StationView): number | null {
  const max = station.info?.max_sessions ?? null;
  if (max === null) return null;
  return Math.max(0, max - Math.min(station.info?.live_sessions ?? 0, max));
}

/**
 * The stations as radio pills (name, reach, "9 free"), and "Auto (best station)" first when the
 * deck places (FG-6): a sparkles icon and a title say that it is no station of its own, and it
 * never wraps alone onto a new line after the stations. The chosen pill has the pressed `.fd-seg`
 * look.
 */
export function StationPills({
  targets,
  value,
  auto,
  onChange,
}: {
  readonly targets: readonly StationView[];
  readonly value: string;
  readonly auto: boolean;
  readonly onChange: (id: string) => void;
}) {
  const name = useId();
  const pill = (id: string, body: ReactNode, title?: string) => (
    <label key={id} className="fd-spawn-station" title={title}>
      <input
        type="radio"
        name={name}
        value={id}
        checked={value === id}
        onChange={() => onChange(id)}
      />
      {body}
    </label>
  );
  return (
    <div className="fd-spawn-stations" role="radiogroup" aria-label="Station">
      {auto
        ? pill(
            AUTO,
            <>
              <Icon of={Sparkles} size={14} />
              Auto<span className="fd-vh"> (best station)</span>
            </>,
            AUTO_TITLE,
          )
        : null}
      {targets.map((s) => {
        const free = freeSlots(s);
        return pill(
          s.station_id,
          <>
            <span className="fd-wide">{s.name}</span>
            <span
              className="fd-dot"
              data-status={s.reachable ? 'online' : 'unreachable'}
              aria-hidden="true"
            />
            {s.reachable ? null : <span className="fd-spawn-off">offline</span>}
            {free === null ? null : <span className="fd-spawn-free">{free} free</span>}
          </>,
        );
      })}
    </div>
  );
}

/**
 * The dim hint after "More options": what the fold holds, so that the owner finds a field without
 * opening it (concept A). `batch`: the fold holds the Batch switch (a phone); `setup`: the station
 * allows a setup command.
 */
export function optionsHint({
  batch,
  setup,
}: {
  readonly batch: boolean;
  readonly setup: boolean;
}): string {
  const text = [batch ? 'batch' : null, setup ? 'setup' : null, 'coordinator', 'import']
    .filter((part) => part !== null)
    .join(', ');
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * The summary of "More options" with its hint; a press toggles the fold through `onToggle` (the
 * form keeps the fold open while an option holds a value). The hint goes while the fold is open.
 */
export function OptionsSummary({
  hint,
  onToggle,
}: {
  readonly hint: string;
  readonly onToggle: () => void;
}) {
  return (
    <summary
      className="fd-label"
      onClick={(event) => {
        event.preventDefault();
        onToggle();
      }}
    >
      <span>More options</span>
      <span className="fd-spawn-options-hint">{hint}</span>
    </summary>
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
      <p
        className="fd-note"
        id="fd-spawn-repo"
        role={repoError !== null && repoAlert ? 'alert' : undefined}
      >
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
 * True while the page can scroll further down. On a phone the page scrolls under the sticky footer
 * of the sheet; the footer then fades the form above its rule (`data-more`, spawn.css).
 */
function usePageMoreBelow(): boolean {
  const [more, setMore] = useState(false);
  useEffect(() => {
    const page = document.documentElement;
    const update = () => setMore(window.scrollY + window.innerHeight < page.scrollHeight - 1);
    update();
    window.addEventListener('scroll', update, { passive: true });
    window.addEventListener('resize', update);
    const sizes = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(update);
    sizes?.observe(document.body);
    return () => {
      window.removeEventListener('scroll', update);
      window.removeEventListener('resize', update);
      sizes?.disconnect();
    };
  }, []);
  return more;
}

/**
 * The Batch switch: one session per line of the task. The footer holds it on a desktop; a phone
 * shows it in "More options", so that its sticky footer is one row of buttons.
 */
export function BatchSwitch({
  on,
  onChange,
}: {
  readonly on: boolean;
  readonly onChange: (on: boolean) => void;
}) {
  return (
    <label className="fd-spawn-batch">
      <input
        type="checkbox"
        role="switch"
        className="fd-switch"
        checked={on}
        aria-checked={on}
        onChange={(event) => onChange(event.target.checked)}
      />
      Batch: one session per line
    </label>
  );
}

/**
 * The footer of the sheet, one line: the Batch switch at the left (a desktop); the outcome, and the
 * missing steps that have no field of their own to show them; Cancel and the primary start button
 * (kbd ⌘↵ or Ctrl+↵). The directory note stays under its field, also on a phone: there the footer
 * is one row of buttons above the bottom bar, and grows only for a reason that the sheet shows
 * nowhere else (the board or the scope, a missing step, the outcome).
 */
export function SpawnFoot({
  batchShown,
  batch,
  onBatch,
  missing,
  notes = [],
  grantProblem = null,
  describedBy,
  outcome,
  onOpenNew,
  disabled,
  label,
}: {
  /** A clone is one session, and a phone shows the switch in "More options": no Batch switch then. */
  readonly batchShown: boolean;
  readonly batch: boolean;
  readonly onBatch: (on: boolean) => void;
  /** The missing steps without a note at their field (empty while a start runs). */
  readonly missing: readonly string[];
  /**
   * The board and scope notes (`fd-spawn-live`, `fd-spawn-scope`) when the form shows them in the
   * footer (a phone), where the disabled button is in view.
   */
  readonly notes?: readonly { readonly id: string; readonly text: string }[];
  /** The first problem of the fleet grant fields: a hidden copy that the button description reads. */
  readonly grantProblem?: string | null;
  /** The ids of the notes that say why the button is off (the directory note, the missing line). */
  readonly describedBy: string | undefined;
  readonly outcome: SpawnOutcome | null;
  /** "Open the new session", when the outcome has a session to open. */
  readonly onOpenNew: (() => void) | null;
  readonly disabled: boolean;
  readonly label: string;
}) {
  const more = usePageMoreBelow();
  return (
    <div className="fd-spawn-foot" data-more={more ? 'true' : undefined}>
      {batchShown ? <BatchSwitch on={batch} onChange={onBatch} /> : null}
      {grantProblem === null ? null : (
        <span hidden id="fd-spawn-grant-error">
          {grantProblem}
        </span>
      )}
      <div className="fd-spawn-status">
        {notes.map((note) => (
          <p key={note.id} className="fd-note" id={note.id}>
            {note.text}
          </p>
        ))}
        {missing.length === 0 ? null : (
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
          aria-describedby={describedBy}
          aria-keyshortcuts="Meta+Enter Control+Enter"
        >
          {label}
          <span className="fd-kbd" aria-hidden="true">
            {submitKeyHint()}
          </span>
        </button>
      </div>
    </div>
  );
}
