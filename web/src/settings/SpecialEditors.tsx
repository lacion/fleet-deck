// The settings controls of single keys whose registry kind is plain text or a list of texts, but
// whose value has a fixed form: the offline approval policy (`wait` or `deny_after(<n><unit>)`) and
// the stations of a worker grant (`["all"]` or station names). The owner picks; the board writes the
// same wire value as before.
import type { StationView } from '../gen';

// --- the offline approval policy ---

export type Unit = 's' | 'm' | 'h';

/** `wait`, or `deny_after(<n><unit>)` (`n` is the typed digits, maybe empty while the owner types). */
export type Policy =
  | { readonly mode: 'wait' }
  | { readonly mode: 'deny'; readonly n: string; readonly unit: Unit };

const DENY = /^deny_after\((\d*)([smh])\)$/;

/** The policy of a value; an empty value is the default (`wait`), an unknown text is `null`. */
export function parsePolicy(value: string): Policy | null {
  const text = value.trim();
  if (text === '' || text === 'wait') return { mode: 'wait' };
  const m = DENY.exec(text);
  return m === null ? null : { mode: 'deny', n: m[1] ?? '', unit: m[2] as Unit };
}

/** The wire value of a policy: `wait` or `deny_after(5m)`. */
export const policyText = (p: Policy): string =>
  p.mode === 'wait' ? 'wait' : `deny_after(${p.n}${p.unit})`;

/** The deny time when the owner turns "Deny it after a time" on. */
const FIRST_DENY: Policy = { mode: 'deny', n: '5', unit: 'm' };

const UNITS: readonly { readonly value: Unit; readonly text: string }[] = [
  { value: 's', text: 'seconds' },
  { value: 'm', text: 'minutes' },
  { value: 'h', text: 'hours' },
];

/** The policy as a choice: keep the request open, or deny it after a time (a number and a unit). */
export function PolicyEditor({
  value,
  disabled,
  onChange,
}: {
  readonly value: string;
  readonly disabled: boolean;
  readonly onChange: (next: string) => void;
}) {
  // A text that the board does not know (a newer station) stays a text field.
  const policy = parsePolicy(value);
  if (policy === null) {
    return (
      <input
        value={value}
        disabled={disabled}
        autoComplete="off"
        spellCheck={false}
        data-mono="true"
        aria-label="Policy"
        onChange={(e) => onChange(e.target.value)}
      />
    );
  }
  const deny = policy.mode === 'deny' ? policy : null;
  return (
    <div className="fd-set-inline">
      <select
        aria-label="When no deck is connected"
        value={policy.mode}
        disabled={disabled}
        onChange={(e) =>
          onChange(policyText(e.target.value === 'wait' ? { mode: 'wait' } : FIRST_DENY))
        }
      >
        <option value="wait">Keep the request open</option>
        <option value="deny">Deny it after a time</option>
      </select>
      {deny === null ? null : (
        <>
          <input
            type="number"
            inputMode="numeric"
            className="fd-set-num"
            aria-label="Time"
            min={1}
            value={deny.n}
            disabled={disabled}
            onChange={(e) =>
              onChange(policyText({ ...deny, n: e.target.value.replace(/\D/g, '') }))
            }
          />
          <select
            aria-label="Unit"
            value={deny.unit}
            disabled={disabled}
            onChange={(e) => onChange(policyText({ ...deny, unit: e.target.value as Unit }))}
          >
            {UNITS.map((u) => (
              <option key={u.value} value={u.value}>
                {u.text}
              </option>
            ))}
          </select>
        </>
      )}
    </div>
  );
}

// --- the stations of a worker grant ---

/** The value that stands for every paired station. */
export const ALL = 'all';
/** The most names of a list (config_worker_grant.rs). */
const MOST = 16;

/** The rows of the picker: the paired stations, then the names of the value that are not paired. */
export function stationRows(
  stations: readonly StationView[],
  value: readonly string[],
): { readonly name: string; readonly paired: boolean }[] {
  const paired = stations.map((s) => s.name);
  const extra = value.filter((n) => n !== ALL && !paired.includes(n));
  return [
    ...paired.map((name) => ({ name, paired: true })),
    ...extra.map((name) => ({ name, paired: false })),
  ];
}

/**
 * "All paired stations", or one checkbox for each station. An empty list would mean the default
 * (every station), so the last checked station cannot be unchecked; turning "All" off checks every
 * paired station first.
 */
export function StationsEditor({
  value,
  stations,
  disabled,
  onChange,
}: {
  readonly value: readonly string[];
  readonly stations: readonly StationView[];
  readonly disabled: boolean;
  readonly onChange: (next: string[]) => void;
}) {
  const all = value.length === 0 || (value.length === 1 && value[0] === ALL);
  const rows = stationRows(stations, all ? [] : value);
  const chosen = all ? [] : value.filter((n) => n !== ALL);
  return (
    <div className="fd-set-stations">
      <label className="fd-set-check">
        <input
          type="checkbox"
          checked={all}
          disabled={disabled || (!all && stations.length === 0)}
          onChange={(e) =>
            onChange(e.target.checked ? [ALL] : stations.slice(0, MOST).map((s) => s.name))
          }
        />
        All paired stations
      </label>
      {all ? null : (
        <span className="fd-set-choices">
          {rows.map((row) => {
            const checked = chosen.includes(row.name);
            const last = checked && chosen.length === 1;
            const full = !checked && chosen.length >= MOST;
            return (
              <label
                key={row.name}
                className="fd-set-check"
                title={last ? 'A grant needs one station or more.' : undefined}
              >
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={disabled || last || full}
                  onChange={(e) =>
                    onChange(
                      e.target.checked
                        ? [...chosen, row.name]
                        : chosen.filter((n) => n !== row.name),
                    )
                  }
                />
                <span className="fd-wide">{row.name}</span>
                {row.paired ? null : <span className="fd-set-faint">not paired</span>}
              </label>
            );
          })}
        </span>
      )}
    </div>
  );
}

// --- a number of minutes where 0 turns the feature off ---

/** An empty field writes 0 (off); 0 shows as the empty field with the placeholder "Off". */
export function OffMinutes({
  label,
  value,
  min,
  max,
  disabled,
  onChange,
}: {
  /** The accessible name of the input: the registry label, which names the unit. */
  readonly label: string;
  readonly value: number | null;
  readonly min: number | null;
  readonly max: number | null;
  readonly disabled: boolean;
  readonly onChange: (next: number) => void;
}) {
  return (
    <span className="fd-set-inline">
      <input
        type="number"
        inputMode="numeric"
        className="fd-set-num fd-set-minutes"
        aria-label={label}
        value={value === null || value === 0 ? '' : value}
        placeholder="Off"
        min={min === null ? undefined : Math.max(min, 1)}
        max={max ?? undefined}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value === '' ? 0 : Number(e.target.value))}
      />
      <span className="fd-set-unit" aria-hidden="true">
        min
      </span>
    </span>
  );
}
