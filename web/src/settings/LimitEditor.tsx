// B27 §3.7: the control of a `limit` field: a number and a "No limit" checkbox. While the box is
// checked the number is off and the edit value is the word "none" (the word of the deck files).
import type { FieldMeta } from '../gen';
import { useRef } from 'react';
import { fieldLabel, unitOf } from './copy';
import { asNumber } from './model';

/** The word that means "no limit" on the wire of a limit field. */
export const NO_LIMIT_WORD = 'none';

/**
 * The value after the box changes: `none` when it is checked; the last number when it is cleared
 * (the lowest allowed number when the field never had one).
 */
export function limitAfterToggle(
  checked: boolean,
  last: number | null,
  min: number | null,
): number | string {
  if (checked) return NO_LIMIT_WORD;
  return last ?? min ?? 1;
}

export function LimitEditor({
  meta,
  value,
  disabled,
  onChange,
}: {
  readonly meta: FieldMeta;
  readonly value: unknown;
  readonly disabled: boolean;
  readonly onChange: (next: unknown) => void;
}) {
  const none = value === NO_LIMIT_WORD;
  const n = none ? null : asNumber(value);
  // The last number that the owner had, for the moment the box is cleared.
  const last = useRef<number | null>(null);
  if (n !== null) last.current = n;
  const unit = unitOf(meta);
  const label = fieldLabel(meta);
  return (
    <span className="fd-set-inline">
      <input
        type="number"
        inputMode="numeric"
        className={unit === null ? undefined : 'fd-set-unitnum'}
        aria-label={unit === null ? label : `${label} in ${unit.words}`}
        value={n ?? ''}
        min={meta.min ?? undefined}
        max={meta.max ?? undefined}
        disabled={disabled || none}
        onChange={(e) => onChange(e.target.value === '' ? null : Number(e.target.value))}
      />
      {unit === null ? null : (
        <span className="fd-set-unit" aria-hidden="true">
          {unit.short}
        </span>
      )}
      <label className="fd-set-check">
        <input
          type="checkbox"
          aria-label={`${label}: no limit`}
          checked={none}
          disabled={disabled}
          onChange={(e) => onChange(limitAfterToggle(e.target.checked, last.current, meta.min))}
        />
        No limit
      </label>
    </span>
  );
}
