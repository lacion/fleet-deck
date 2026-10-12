// DL-141: short display forms of session data.

const FAMILIES = ['opus', 'sonnet', 'haiku', 'fable', 'quill', 'comet'] as const;
export type ModelFamily = (typeof FAMILIES)[number];

/** The model family for the badge hue; `null` for a model outside the known families. */
export function modelFamily(model: string | null): ModelFamily | null {
  const m = (model ?? '').toLowerCase();
  return FAMILIES.find((f) => m.includes(f)) ?? null;
}

/** `claude-sonnet-4-5` → `Sonnet 4.5`; an unknown family stays as is; no model → `default`. */
export function shortModel(model: string | null): string {
  if (model === null || model === '') return 'default';
  const family = modelFamily(model);
  if (family === null) return model;
  const rest = model.toLowerCase().split(family)[1] ?? '';
  const version = rest
    .split(/[-_.]/)
    .filter((part) => /^\d{1,2}$/.test(part))
    .slice(0, 2);
  const name = family.charAt(0).toUpperCase() + family.slice(1);
  return version.length > 0 ? `${name} ${version.join('.')}` : name;
}
