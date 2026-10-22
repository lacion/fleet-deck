import { type ReactNode, useId, useState } from 'react';
import type { ModelChoice } from '../gen';
import { Icon, type LucideIcon, Sparkles } from '../ui/icons';

const OTHER = '__other__';

/**
 * Where the list of models comes from: the tooltip of the select and, for a touch screen and a
 * screen reader, its accessible description (a hidden note).
 */
const FROM = 'From the last session of this provider on this station.';

/**
 * A select pill of the Agent row with a leading icon (DESIGN-SYSTEM §4.6, concept B): the icon
 * sits inside the pill, at its left edge; the select keeps its own name.
 */
export function PillWrap({
  icon,
  children,
}: {
  readonly icon: LucideIcon;
  readonly children: ReactNode;
}) {
  return (
    <span className="fd-spawn-pillwrap">
      <Icon of={icon} size={14} />
      {children}
    </span>
  );
}

/**
 * The model field of the spawn form (DL-151): a compact pill in the Agent row of the sheet
 * (DESIGN-SYSTEM §4.6). Without a reported list it is a text pill: the owner types a model id or
 * alias at once. With a list it is a select with "Default model", the models that the provider
 * reported and "Other…", which shows the free-text input. The field name is the `aria-label`: the
 * row has no captions. The parent gives the component a `key` of the station and the provider
 * instance, so a change of either closes "Other…".
 */
export function ModelField(props: {
  value: string;
  models: readonly ModelChoice[] | null;
  onChange: (model: string) => void;
}) {
  const { value, models, onChange } = props;
  const noteId = useId();
  const [otherChosen, setOther] = useState(false);
  if (models === null) {
    return (
      <PillWrap icon={Sparkles}>
        <input
          className="fd-spawn-pill"
          aria-label="Model (optional)"
          placeholder="Default model"
          spellCheck={false}
          autoCapitalize="off"
          value={value}
          onChange={(event) => onChange(event.target.value)}
        />
      </PillWrap>
    );
  }
  // A value that is not in the list (typed before the list came, or before a station change)
  // stays visible under "Other…".
  const other = otherChosen || (value !== '' && !models.some((m) => m.value === value));
  return (
    <>
      <PillWrap icon={Sparkles}>
        <select
          aria-label="Model (optional)"
          className="fd-spawn-pill"
          title={FROM}
          aria-describedby={noteId}
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
      </PillWrap>
      <span id={noteId} className="fd-vh">
        {FROM}
      </span>
      {other ? (
        <input
          className="fd-spawn-pill fd-spawn-model"
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
