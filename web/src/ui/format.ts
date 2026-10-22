// DL-141: short display forms of session data.

import type { ActivityKind } from '../gen';

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
  // An OpenAI id: "gpt-6.1-codex" → "GPT-6.1 Codex", "gpt-6" → "GPT-6".
  const gpt = /^gpt-(\d+(?:\.\d+)?)(?:-(\w+))?/i.exec(model);
  if (gpt) {
    const tail = gpt[2];
    return tail ? `GPT-${gpt[1]} ${tail.charAt(0).toUpperCase()}${tail.slice(1)}` : `GPT-${gpt[1]}`;
  }
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

/** The tools whose input is a command or a path: the wall stream and the Activity page draw that
 * input in mono. A tool with prose input (a task, a search query) is not here. */
const CODE_TOOLS: ReadonlySet<string> = new Set([
  'Bash',
  'Shell',
  'shell',
  'exec_command',
  'Read',
  'Edit',
  'MultiEdit',
  'Write',
  'NotebookEdit',
  'Grep',
  'Glob',
  'LS',
  'WebFetch',
  'apply_patch',
]);

/** "Bash cargo test", "Edit: src/a.ts", "Permission: Bash cargo test": an optional request
 * label, the tool name, a space or ": ", then the target. */
const TOOL_LINE = /^((?:Permission|Needs you): )?([A-Za-z_]+)(:? )(\S.*)$/;

/** A tool or approval row of an activity list split into the words (the label and the tool name, in
 * sans) and the target (the command or the path, in mono). `null`: the row is words only. The
 * two parts joined by a space give the text again. */
export function toolParts(
  kind: ActivityKind,
  text: string,
): { readonly lead: string; readonly target: string } | null {
  if (kind !== 'tool' && kind !== 'approval') return null;
  const m = TOOL_LINE.exec(text);
  const name = m?.[2];
  const target = m?.[4];
  if (m === null || name === undefined || target === undefined || !CODE_TOOLS.has(name)) {
    return null;
  }
  return { lead: `${m[1] ?? ''}${name}${(m[3] ?? ' ').trimEnd()}`, target };
}

/** A size in decimal units: `512 B`, `12.3 KB`, `4.5 MB`, `1.2 GB`. */
export function sizeText(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  if (bytes < 1e6) return `${(bytes / 1e3).toFixed(1)} KB`;
  if (bytes < 1e9) return `${(bytes / 1e6).toFixed(1)} MB`;
  return `${(bytes / 1e9).toFixed(1)} GB`;
}
