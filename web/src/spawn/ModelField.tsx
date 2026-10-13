import { useState } from 'react';
import type { ModelChoice } from '../gen';

const OTHER = '__other__';

/**
 * The model field of the spawn form (DL-151). With a list of models that the
 * provider reported, a select with "Provider default" and "Other…" (which
 * shows the free-text input); without one, the free-text input. The parent
 * gives the component a `key` of the station and the provider instance, so a
 * change of either closes "Other…".
 */
export function ModelField(props: {
  value: string;
  models: readonly ModelChoice[] | null;
  onChange: (model: string) => void;
}) {
  const [other, setOther] = useState(false);
  const { value, models, onChange } = props;
  if (models === null) {
    return (
      <label>
        Model (optional)
        <input value={value} onChange={(event) => onChange(event.target.value)} />
      </label>
    );
  }
  return (
    <>
      <label>
        Model (optional)
        <select
          aria-label="Model (optional)"
          value={other ? OTHER : value}
          onChange={(event) => {
            const next = event.target.value;
            if (next === OTHER) {
              setOther(true);
              onChange('');
              return;
            }
            setOther(false);
            onChange(next);
          }}
        >
          <option value="">Provider default</option>
          {models.map((m) => (
            <option key={m.value} value={m.value} title={m.description ?? undefined}>
              {m.label}
            </option>
          ))}
          <option value={OTHER}>Other…</option>
        </select>
      </label>
      {other ? (
        <label>
          Model id or alias
          <input value={value} onChange={(event) => onChange(event.target.value)} />
        </label>
      ) : null}
      <p className="fd-note">From the last session of this provider on this station.</p>
    </>
  );
}
