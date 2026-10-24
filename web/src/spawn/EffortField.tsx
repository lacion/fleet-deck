// B18 Phase 2: the Effort field of the Agent row of the New session sheet (docs/v2/design/effort.md
// §9.2 item 8). A select pill: "Default" (no `effort` in the spec), then the levels of the chosen
// model; with "Default model", the levels that every known model of the provider instance offers.
// It is hidden when the station cannot set the effort or no level is known.
import { useState } from 'react';
import type { ModelChoice, StationView } from '../gen';
import { choiceFor } from '../session/effortChoice';
import { Zap } from '../ui/icons';
import { PillWrap } from './ModelField';

/**
 * The levels that the form offers for `model` (the text of the model field). A model that the list
 * names gives its own levels. An empty model gives the levels that every model of the list
 * offers; a model that does not advertise its levels does not narrow them.
 */
export function spawnEffortLevels(
  models: readonly ModelChoice[] | null,
  model: string,
): readonly string[] {
  if (models === null || models.length === 0) return [];
  const chosen = model.trim();
  if (chosen !== '') return choiceFor(models, chosen)?.efforts ?? [];
  const lists = models.flatMap((m) => (m.efforts === undefined ? [] : [m.efforts]));
  const [first, ...rest] = lists;
  return (first ?? []).filter((level) => rest.every((list) => list.includes(level)));
}

/** The effort choice of the spawn form: the offered levels, the choice, and its setter. */
export type SpawnEffort = {
  readonly levels: readonly string[];
  /** The chosen level, or `''` (Default, or a level that the model does not offer). */
  readonly value: string;
  readonly set: (level: string) => void;
};

/**
 * The effort state of the spawn form. The levels are empty (the field is hidden and the spec
 * carries no effort) under Auto, or when the station does not offer `session_effort.v1`.
 */
export function useSpawnEffort(
  station: StationView | undefined,
  auto: boolean,
  models: readonly ModelChoice[] | null,
  model: string,
): SpawnEffort {
  const [chosen, set] = useState('');
  const levels = auto || station?.session_effort !== true ? [] : spawnEffortLevels(models, model);
  return { levels, value: levels.includes(chosen) ? chosen : '', set };
}

export function EffortField({ effort }: { readonly effort: SpawnEffort }) {
  if (effort.levels.length === 0) return null;
  return (
    <PillWrap icon={Zap}>
      <select
        aria-label="Effort"
        className="fd-spawn-pill"
        title="Reasoning effort"
        value={effort.value}
        onChange={(event) => effort.set(event.target.value)}
      >
        <option value="">Default</option>
        {effort.levels.map((level) => (
          <option key={level} value={level}>
            {level}
          </option>
        ))}
      </select>
    </PillWrap>
  );
}
