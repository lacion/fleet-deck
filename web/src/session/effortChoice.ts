// B18 Phase 2: the effort levels that a model offers (docs/v2/design/effort.md §9.1, §9.2 item 8).
// `baseModel` and `choiceFor` are the TypeScript twins of `fleetdeck_proto::effort::base_model` and
// `choice_for`: the same cases give the same answers (effortChoice.test.ts).
import type { EventEnvelope, ModelChoice, SessionSummary } from '../gen';
import type { SessionActions } from './commands';
import type { Controller } from './controller';
import { sessionModels } from './timeline';

/** The model id without a trailing context suffix: `claude-opus-5-5[1m]` → `claude-opus-5-5`. */
export function baseModel(id: string): string {
  if (!id.endsWith(']')) return id;
  const rest = id.slice(0, -1);
  const at = rest.lastIndexOf('[');
  return at > 0 ? rest.slice(0, at) : id;
}

/**
 * The advertised choice of `model`: in list order, the first choice whose `value` is `model`; else
 * whose `resolved` is `model`; else whose `value` or `resolved` has the same base model. `null`
 * when no choice matches.
 */
export function choiceFor(models: readonly ModelChoice[], model: string): ModelChoice | null {
  const base = baseModel(model);
  return (
    models.find((c) => c.value === model) ??
    models.find((c) => c.resolved === model) ??
    models.find(
      (c) =>
        baseModel(c.value) === base || (c.resolved !== undefined && baseModel(c.resolved) === base),
    ) ??
    null
  );
}

/** The effort levels of `model` (empty: not advertised, or the model has no effort setting). */
export function effortLevels(
  models: readonly ModelChoice[] | null,
  model: string | null,
): readonly string[] {
  if (models === null || model === null) return [];
  return choiceFor(models, model)?.efforts ?? [];
}

/** The model chip of the composer: the model picker, and the Effort group when it can be set. */
export type ComposerModelChoice = {
  readonly current: string | null;
  readonly models: readonly ModelChoice[] | null;
  readonly onSet: (model: string) => void;
  /** Absent: the provider cannot set the effort, or the model offers no level. */
  readonly effort?: {
    readonly current: string | null;
    readonly levels: readonly string[];
    readonly onSet: (level: string) => void;
  };
};

/** The `modelChoice` of the composer for a session that the owner drives. */
export function composerModelChoice(
  s: SessionSummary,
  events: readonly EventEnvelope[],
  actions: SessionActions | undefined,
  controller: Controller,
): ComposerModelChoice {
  const models = s.models ?? sessionModels(events);
  const levels = s.capabilities?.set_effort === true ? effortLevels(models, s.model) : [];
  return {
    current: s.model,
    models,
    onSet: (m) => actions && controller.act('model', () => actions.setModel(m)),
    ...(levels.length === 0
      ? {}
      : {
          effort: {
            current: s.effort ?? null,
            levels,
            onSet: (level: string) =>
              actions && controller.act('effort', () => actions.setEffort(level)),
          },
        }),
  };
}
