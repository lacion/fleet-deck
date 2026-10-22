// The Agent row of the New session sheet for a chosen station (DESIGN-SYSTEM.md §4.6): the provider
// instances as a segmented control, the fuel badge of a tight limit, then the model and the
// permission mode as compact select pills. Moved out of `SpawnForm.tsx` for the module size rule;
// the form keeps every state and every handler.
import type { ModelChoice, StationProvider } from '../gen';
import { ModelField } from './ModelField';
import { Seg } from './SpawnSheet';
import { providerLabel } from './spawn';

export function AgentFields({
  providers,
  instance,
  onInstance,
  badge,
  fieldKey,
  model,
  models,
  onModel,
  modes,
  mode,
  onMode,
}: {
  readonly providers: readonly StationProvider[];
  readonly instance: string;
  readonly onInstance: (instance: string) => void;
  /** The limit of the chosen instance from WARN_PERCENT on (R4.3), or `null`. */
  readonly badge: { readonly text: string; readonly title: string } | null;
  /** The station and the instance: a change of either resets the model field. */
  readonly fieldKey: string;
  readonly model: string;
  readonly models: readonly ModelChoice[] | null;
  readonly onModel: (model: string) => void;
  readonly modes: readonly string[];
  readonly mode: string;
  readonly onMode: (mode: string) => void;
}) {
  return (
    <>
      <Seg
        label="Provider"
        value={instance}
        options={providers.map((p) => ({ value: p.instance, label: providerLabel(p) }))}
        onChange={onInstance}
      />
      {badge === null ? null : (
        <span className="fd-badge" data-tone="fuel" title={badge.title}>
          {badge.text}
        </span>
      )}
      <span className="fd-spawn-pills">
        <ModelField key={fieldKey} value={model} models={models} onChange={onModel} />
        <select
          aria-label="Permission mode"
          className="fd-sm fd-spawn-pill"
          value={mode}
          onChange={(event) => onMode(event.target.value)}
        >
          <option value="">Station default</option>
          {modes.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </span>
    </>
  );
}
