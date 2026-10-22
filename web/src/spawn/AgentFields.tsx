// The Agent row of the New session sheet for a chosen station (DESIGN-SYSTEM.md §4.6): the provider
// instances as a segmented control with the limit of each instance (R4.3), the fuel badge of a
// tight limit, then the model and the permission mode as two select pills with a leading icon
// (sparkles, shield). Moved out of `SpawnForm.tsx` for the module size rule; the form keeps every
// state and every handler.
import type { RateMeter } from '../board/usage';
import type { ModelChoice, StationProvider } from '../gen';
import { Shield } from '../ui/icons';
import { ModelField, PillWrap } from './ModelField';
import { Seg } from './SpawnSheet';
import { providerChoice } from './spawn';

export function AgentFields({
  providers,
  instance,
  onInstance,
  limits,
  roomiest,
  now,
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
  /** The tightest limit of each instance of the station (R4.3). */
  readonly limits: ReadonlyMap<string, RateMeter | null>;
  /** The instance with the most room, or `null`. */
  readonly roomiest: string | null;
  readonly now: number;
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
  const options = providers.map((p) => {
    const choice = providerChoice(p, limits.get(p.instance) ?? null, now, p.instance === roomiest);
    return {
      value: p.instance,
      title: choice.title,
      label:
        choice.suffix === '' ? (
          choice.label
        ) : (
          <>
            <span className="fd-spawn-seg-name">{choice.label}</span>{' '}
            <span className="fd-spawn-seg-meta">{choice.suffix}</span>
          </>
        ),
    };
  });
  return (
    <>
      <Seg label="Provider" value={instance} options={options} onChange={onInstance} />
      {badge === null ? null : (
        <span className="fd-badge" data-tone="fuel" title={badge.title}>
          {badge.text}
        </span>
      )}
      <span className="fd-spawn-pills">
        <ModelField key={fieldKey} value={model} models={models} onChange={onModel} />
        <PillWrap icon={Shield}>
          <select
            aria-label="Permission mode"
            className="fd-spawn-pill"
            title="Permission mode"
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
        </PillWrap>
      </span>
    </>
  );
}
