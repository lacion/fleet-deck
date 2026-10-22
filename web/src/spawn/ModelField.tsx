import { useState } from 'react';
import type { ModelChoice } from '../gen';

const OTHER = '__other__';

/** Where the list of models comes from: the title of the select. */
const FROM = 'From the last session of this provider on this station.';

/**
 * The model field of the spawn form (DL-151): a compact pill in the Agent row of the sheet
 * (DESIGN-SYSTEM §4.6). With a list of models that the provider reported, a select with "Default
 * model" and "Other…" (which shows the free-text input); without one, the free-text input with the
 * placeholder "Default model". The field name is the `aria-label`: the row has no captions. The
 * parent gives the component a `key` of the station and the provider instance, so a change of
 * either closes "Other…".
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
      <input
        className="fd-sm fd-spawn-pill fd-spawn-model"
        aria-label="Model (optional)"
        placeholder="Default model"
        spellCheck={false}
        autoCapitalize="off"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  }
  return (
    <>
      <select
        aria-label="Model (optional)"
        className="fd-sm fd-spawn-pill"
        title={FROM}
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
        <option value="">Default model</option>
        {models.map((m) => (
          <option key={m.value} value={m.value} title={m.description ?? undefined}>
            {m.label}
          </option>
        ))}
        <option value={OTHER}>Other…</option>
      </select>
      {other ? (
        <input
          className="fd-sm fd-spawn-pill fd-spawn-model"
          aria-label="Model id or alias"
          placeholder="Model id or alias"
          spellCheck={false}
          autoCapitalize="off"
          value={value}
          onChange={(event) => onChange(event.target.value)}
        />
      ) : null}
    </>
  );
}
