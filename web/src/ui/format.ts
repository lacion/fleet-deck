// DL-141: short display forms of session data.

import type { ActivityKind, MonitorInfo } from '../gen';

const FAMILIES = ['opus', 'sonnet', 'haiku', 'fable', 'quill', 'comet'] as const;
export type ModelFamily = (typeof FAMILIES)[number];

/** The model family for the badge hue; `null` for a model outside the known families. */
export function modelFamily(model: string | null): ModelFamily | null {
  const m = (model ?? '').toLowerCase();
  return FAMILIES.find((f) => m.includes(f)) ?? null;
}

/** The `[1m]` context suffix of a model id: `claude-opus-5-5[1m]` → `1m`; none → `undefined`. */
const contextSuffix = (model: string): string | undefined => /\[([^\]]+)\]$/.exec(model)?.[1];

/**
 * `claude-sonnet-4-5` → `Sonnet 4.5`: the family and the version only. A context suffix is not
 * part of the name (`claude-opus-5-5[1m]` → `Opus 5.5`; `contextLabel` gives `1M context`), and a
 * bare `1M` read as a version or a count. An unknown family stays as is; no model → `default`.
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
  const base = contextSuffix(model) === undefined ? model : model.slice(0, model.lastIndexOf('['));
  const rest = base.toLowerCase().split(family)[1] ?? '';
  const version = rest
    .split(/[-_.]/)
    .filter((part) => /^\d{1,2}$/.test(part))
    .slice(0, 2);
  const name = family.charAt(0).toUpperCase() + family.slice(1);
  return version.length > 0 ? `${name} ${version.join('.')}` : name;
}

/** The model and the reasoning effort of a session: `Opus 5.5 · high`; the model alone when
 * the provider reports no effort (B18). */
export function modelText(model: string | null, effort?: string | null): string {
  const name = shortModel(model);
  return effort === undefined || effort === null || effort === '' ? name : `${name} · ${effort}`;
}

/** The `title` of a model with its effort: the raw model id, then the effort in words. */
export function modelTitle(model: string | null, effort?: string | null): string {
  const id = model ?? 'default model';
  return effort === undefined || effort === null || effort === ''
    ? id
    : `${id} · reasoning effort ${effort}`;
}

/**
 * The context window that a model id names: `claude-opus-5-5[1m]` → `1M context`, `[200k]` →
 * `200K context`; `null` when the id names none (the session shows no Context row then).
 */
export function contextLabel(model: string | null): string | null {
  if (model === null) return null;
  const suffix = contextSuffix(model);
  return suffix === undefined || suffix === '' ? null : `${suffix.toUpperCase()} context`;
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

/** The words of a Codex mode `<approval policy>/<sandbox>` (crates/fleetdeck-providers codex). */
const CODEX_APPROVALS: Readonly<Record<string, string>> = {
  untrusted: 'Ask for untrusted commands',
  'on-request': 'Ask when needed',
  never: 'Never ask',
};
const CODEX_SANDBOXES: Readonly<Record<string, string>> = {
  'read-only': 'read only',
  'workspace-write': 'workspace write',
  'danger-full-access': 'full access',
};

/** `on-request/workspace-write` → `Ask when needed · workspace write`; an unknown id stays as it is. */
export function codexModeLabel(mode: string): string {
  const [approval, sandbox, ...rest] = mode.split('/');
  if (approval === undefined || sandbox === undefined || rest.length > 0) return mode;
  const a = CODEX_APPROVALS[approval];
  const s = CODEX_SANDBOXES[sandbox];
  return a === undefined || s === undefined ? mode : `${a} · ${s}`;
}

/** The one name of a permission mode on every surface (header, Properties, New session, the
 * fleet access forms, settings): `acceptEdits` → `Accept edits`, `on-request/workspace-write` →
 * `Ask when needed · workspace write`; a mode outside the lists stays as it is. */
export const modeLabel = (mode: string): string => MODE_LABELS[mode] ?? codexModeLabel(mode);

/** The tools whose input is a command or a path: the wall stream and the Activity page draw that
 * input in mono (`toolParts` here, `toolTarget` of `watch/tooltext.ts`). A tool with prose input
 * (a task, a search query) is not here. */
export const CODE_TOOLS: ReadonlySet<string> = new Set([
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
  // The shell tools that watch or stop a command: their input is a command or a shell id.
  'Monitor',
  'BashOutput',
  'KillShell',
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

// ── numbers (DESIGN-SYSTEM §5): one format for one kind of number on every surface ──

const COUNT = new Intl.NumberFormat('en-US');
// The compact notation rounds to two significant digits and never to fewer whole digits:
// 940, 9.4K, 52K, 236K, 1.3M (no trailing .0).
const COMPACT = new Intl.NumberFormat('en-US', { notation: 'compact' });
const USD = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' });

/** A count with thousands separators: `1286` → `1,286`, `7405` → `7,405`. */
export const countText = (n: number): string => COUNT.format(n);

/** A large count in short form (tokens): `940`, `9.4K`, `236K`, `1.3M`. */
export const compactCount = (n: number): string => COMPACT.format(n);

/** Money in US dollars with separators and cents: `$1.20`, `$14,876.41`. */
export const usdText = (n: number): string => USD.format(n);

/** The added lines of a diff: `1286` → `+1,286`. */
export const addedText = (n: number): string => `+${countText(n)}`;

/** The removed lines of a diff, with the minus sign (U+2212): `400` → `−400`. */
export const removedText = (n: number): string => `−${countText(n)}`;

// ── paths (DESIGN-SYSTEM §5): a shown path starts at `~/` under a home folder, else at `/` ──

/** A home folder of Linux or macOS at the start of a path: `/home/me`, `/Users/me`. */
const HOME = /^\/(?:home|Users)\/[^/]+(?=\/|$)/;

/** `/home/me/projects/x` → `~/projects/x` (a home folder of Linux or macOS); other paths stay. */
export function homeShort(path: string): string {
  return path.replace(HOME, '~');
}

/**
 * The home folder that a path starts with (`/home/me` for `/home/me/x` and `/home/me`), or `null`:
 * the part that `homeShort` writes as `~`. A folder picker shows it as one `~` crumb, not as a
 * root-less `home / me`.
 */
export function homeOf(path: string): string | null {
  return HOME.exec(path)?.[0] ?? null;
}

/**
 * Every home folder in a text (a command, a tool target) as `~`: `cat /home/me/a.txt` →
 * `cat ~/a.txt`, `--dir=/home/me/x` → `--dir=~/x`. A path must start the text or follow a space,
 * a quote, `=`, `:` or `(`; display only (a copy action copies the raw text).
 */
export function homeShortIn(text: string): string {
  return text.replace(/(^|[\s'"=:(])\/(?:home|Users)\/[^/\s'"()]+(?=[/\s'")]|$)/g, '$1~');
}

/**
 * A folder for a list row: its name and its parent folder, from the root (`~/` or `/`).
 * `/home/me/fleet/v2` → `{ base: 'v2', parent: '~/fleet/' }`; the home folder is `~` with no
 * parent; `/` is `/`. The full path stays for the title and the copy actions.
 */
export function pathParts(path: string): { readonly parent: string; readonly base: string } {
  const short = homeShort(path.replace(/(?<=.)\/+$/, ''));
  const cut = short.lastIndexOf('/');
  if (short === '/' || cut < 0) return { parent: '', base: short };
  return { parent: short.slice(0, cut + 1), base: short.slice(cut + 1) };
}

// ── platforms (DESIGN-SYSTEM §5): the names that owners know, not the target names of Rust ──

/** The owner's name of an architecture: `aarch64` → `arm64`, `amd64` → `x86_64`. */
const ARCH_LABELS: Readonly<Record<string, string>> = {
  aarch64: 'arm64',
  arm64: 'arm64',
  x86_64: 'x86_64',
  amd64: 'x86_64',
};

/**
 * The architecture of a station for a person: `aarch64` (the Rust target name of an Apple
 * silicon Mac) → `arm64`, `amd64` → `x86_64`; any other value stays as it is. The `title` of the
 * line keeps the raw value.
 */
export function archLabel(arch: string): string {
  return ARCH_LABELS[arch.toLowerCase()] ?? arch;
}

/** A size in decimal units: `512 B`, `12.3 KB`, `4.5 MB`, `1.2 GB`. */
export function sizeText(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  if (bytes < 1e6) return `${(bytes / 1e3).toFixed(1)} KB`;
  if (bytes < 1e9) return `${(bytes / 1e6).toFixed(1)} MB`;
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

// ── monitors (DESIGN-SYSTEM §5): the owner's words, not the raw EDID text of the station ──

/** A connector of a laptop or tablet panel: the screen of the machine itself. */
const BUILT_IN = /^(?:eDP|LVDS|DSI)-/i;
/** A hex product code in an EDID description ("Samsung Display Corp. 0x41AA"). */
const PRODUCT_CODE = /\s*\b0x[0-9a-f]+\b/gi;

/**
 * One option text of a monitor, the connector last: `Built-in display · eDP-1` for a laptop panel,
 * `Dell Inc. DELL U2720Q · DP-1` (the description without its hex product code), or only the
 * connector when the station knows nothing more. The option value and its title keep the raw name
 * (`monitorRaw`).
 */
export function monitorLabel(m: Pick<MonitorInfo, 'name' | 'description'>): string {
  const described = m.description.replace(PRODUCT_CODE, '').trim();
  const what = BUILT_IN.test(m.name) ? 'Built-in display' : described;
  return what === '' ? m.name : `${what} · ${m.name}`;
}

/** The raw text of a monitor (the title of its option): `<name> — <description>`. */
export const monitorRaw = (m: Pick<MonitorInfo, 'name' | 'description'>): string =>
  m.description === '' ? m.name : `${m.name} — ${m.description}`;
