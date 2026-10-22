// DL-141: short display forms of session data.

const FAMILIES = ['opus', 'sonnet', 'haiku', 'fable', 'quill', 'comet'] as const;
export type ModelFamily = (typeof FAMILIES)[number];

/** The model family for the badge hue; `null` for a model outside the known families. */
export function modelFamily(model: string | null): ModelFamily | null {
  const m = (model ?? '').toLowerCase();
  return FAMILIES.find((f) => m.includes(f)) ?? null;
}

/**
 * `claude-sonnet-4-5` → `Sonnet 4.5`; a context suffix shows after the version
 * (`claude-opus-5-5[1m]` → `Opus 5.5 1M`); an unknown family stays as is; no model → `default`.
 */
export function shortModel(model: string | null): string {
  if (model === null || model === '') return 'default';
  const family = modelFamily(model);
  if (family === null) return model;
  // A `[1m]` suffix is not part of the version (it hid the minor version).
  const suffix = /\[([^\]]+)\]$/.exec(model)?.[1]?.toUpperCase();
  const base = suffix === undefined ? model : model.slice(0, model.lastIndexOf('['));
  const rest = base.toLowerCase().split(family)[1] ?? '';
  const version = rest
    .split(/[-_.]/)
    .filter((part) => /^\d{1,2}$/.test(part))
    .slice(0, 2);
  const name = family.charAt(0).toUpperCase() + family.slice(1);
  const short = version.length > 0 ? `${name} ${version.join('.')}` : name;
  return suffix === undefined ? short : `${short} ${suffix}`;
}

/** A line of chat as plain text: no heading marks, emphasis marks, code ticks or link syntax. */
export function plainLine(text: string): string {
  return text
    .replace(/^\s{0,3}#{1,6}\s+/, '')
    .replace(/^\s*>\s?/, '')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/(\*\*|__)(.+?)\1/g, '$2')
    .replace(/(^|\W)(\*|_)(?=\S)(.+?)(?<=\S)\2(?=\W|$)/g, '$1$3')
    .replace(/~~(.+?)~~/g, '$1')
    .replace(/`+([^`]*)`+/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
}

/** Reasons that a card can show in a sentence of their own. */
const REASONS: Readonly<Record<string, string>> = {
  already_stopping: 'the session was already stopping',
};

const capital = (text: string): string => text.charAt(0).toUpperCase() + text.slice(1);

/**
 * The error line of a card. A `code: detail` text keeps the detail only, in a sentence:
 * `not_dispatched: stop was not sent: already_stopping` gives `Stop was not sent: the session was
 * already stopping.` Any other text is returned as it is.
 */
export function errorSentence(text: string): string {
  if (!/^[a-z_]+: /.test(text)) return text;
  const detail = text.replace(/^[a-z_]+: /, '').trim();
  const cut = detail.lastIndexOf(': ');
  const reason = cut < 0 ? undefined : REASONS[detail.slice(cut + 2)];
  const shown = reason === undefined ? detail : `${detail.slice(0, cut)}: ${reason}`;
  return `${capital(shown)}${/[.!?]$/.test(shown) ? '' : '.'}`;
}

/** The permission modes that skip the approvals of the provider: the only badges in hazard red. */
const RISKY_MODES: readonly string[] = ['dontAsk', 'bypassPermissions', 'danger-full-access'];

export const isRiskyMode = (mode: string | null | undefined): boolean =>
  mode !== null && mode !== undefined && RISKY_MODES.includes(mode);

/** The sentence-case names of the permission modes; an unknown mode shows its raw id. */
const MODE_LABELS: Readonly<Record<string, string>> = {
  default: 'Default',
  plan: 'Plan',
  acceptEdits: 'Accept edits',
  dontAsk: "Don't ask",
  auto: 'Auto',
  bypassPermissions: 'Bypass permissions',
};

/** `acceptEdits` → `Accept edits`; a mode outside the list stays as it is. */
export const modeLabel = (mode: string): string => MODE_LABELS[mode] ?? mode;
