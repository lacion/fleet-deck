// The board's own words for the settings registry: the labels, the help texts, the units and the
// option names of the fields. The registry of each station binary carries its own copy; a key in
// these maps shows the board's copy instead, so that one field reads the same on every station
// version and never shows raw protocol values. The values on the wire stay the same.
import type { FieldMeta } from '../gen';
import { codexModeLabel, countText } from '../ui/format';

/** The label of a field when the registry label repeats its section heading. */
const FIELD_LABELS: Readonly<Record<string, string>> = {
  fleet_control: 'Allow fleet control',
  'worker_grant.enabled': 'Give new sessions a grant',
  'worker_grant.stations': 'Stations',
  'worker_grant.spawn_cap': 'Sessions per worker',
  'worker_grant.claude_mode': 'Most permissive Claude mode',
  'worker_grant.codex_mode': 'Most permissive Codex mode',
  'worker_grant.lifetime_hours': 'Lifetime',
  'worker_grant.idle_minutes': 'Idle time',
  shim_buffer_mib: 'Output buffer',
  hibernate_after_min: 'Stop idle sessions after',
  allow_desktop_input_any: 'Agent input in other windows',
  // Two fields of Time limits keep apart without their units.
  'limits.scrollback_lines': 'Terminal history',
  'limits.scrollback_mib': 'Terminal history size',
};

/** The unit of a number field: its short form after the field and its words for the name. */
export type Unit = { readonly short: string; readonly words: string };

/** The units that a registry label names at its end: `Login idle time (days)`. */
const UNITS: Readonly<Record<string, Unit>> = {
  characters: { short: 'characters', words: 'characters' },
  lines: { short: 'lines', words: 'lines' },
  seconds: { short: 's', words: 'seconds' },
  minutes: { short: 'min', words: 'minutes' },
  hours: { short: 'hours', words: 'hours' },
  days: { short: 'days', words: 'days' },
  MiB: { short: 'MiB', words: 'MiB' },
  GiB: { short: 'GiB', words: 'GiB' },
};

const UNIT_END = /^(.*\S) \(([A-Za-z]+)\)$/;

/**
 * The unit of a number field, from the end of its registry label; `null` for a field without one.
 * The page shows it after the field (one notation for every unit), not in the label.
 */
export function unitOf(meta: Pick<FieldMeta, 'kind' | 'label'>): Unit | null {
  if (meta.kind !== 'int') return null;
  const m = UNIT_END.exec(meta.label);
  return m === null ? null : (UNITS[m[2] ?? ''] ?? null);
}

/** The label of a field: the board's words, else the registry label without its unit. */
export function fieldLabel(meta: Pick<FieldMeta, 'key' | 'label' | 'kind'>): string {
  const own = FIELD_LABELS[meta.key];
  if (own !== undefined) return own;
  return unitOf(meta) === null ? meta.label : (UNIT_END.exec(meta.label)?.[1] ?? meta.label);
}

/**
 * The help of a field when the registry help shows raw values, a syntax to type or another verb
 * form. Every switch says what it lets happen ("Lets …"). A key of one file only is `deck:<key>`.
 */
const HELP: Readonly<Record<string, string>> = {
  'deck:name': 'The name that the board and the wall show for this deck.',
  host: 'Directly: sessions end when the station stops. Apart from the station: sessions keep running while the station restarts.',
  offline_approval_policy:
    'What this station does with a permission request while no deck is connected.',
  hibernate_after_min:
    'Stop a session that is idle this long. You can resume it in the board. Leave it empty to keep idle sessions running.',
  allow_bypass_permissions:
    'Lets sessions on this station use the Bypass mode, which skips every permission check.',
  allow_shell: 'Lets the board open a terminal on this station.',
  allow_setup_cmd:
    'Lets a new session run a shell command before it starts. The command runs on this machine.',
  allow_desktop_input:
    'Lets a coordinator session with a desktop grant type and move the pointer in the windows it may use. Off stops new input at once.',
  allow_desktop_click: 'Lets it also click, one click at a time. Needs Allow agent desktop input.',
  allow_desktop_input_any:
    'Lets it use other windows too, also terminals and signed-in browsers. Needs Allow agent desktop input.',
  capture_monitors:
    'The monitors that a screenshot may capture. Leave it empty to allow every monitor.',
  fleet_summary:
    'Lets the deck send a summary of the fleet (counts, the sessions that need you, usage) to this station for the Omarchy bar. Programs that run as you here can read it.',
  'model_help.enabled':
    'Lets this station ask your own provider login for short generated text, such as a title.',
  conflict_mail: 'Lets the deck tell both sessions once when two sessions change the same file.',
  desktop_control:
    'Lets the fleet run desktop actions and wake the walls. Off blocks actions that have not started.',
  tailnet_auto:
    'Lets the deck put the tailnet addresses of this machine in the address lists at each start. On by default.',
  tailscale_cert:
    'Lets the board get a certificate for its tailnet name that browsers trust. Turn on HTTPS certificates in the Tailscale admin console first. The name becomes public in Certificate Transparency logs.',
  'worker_grant.enabled':
    'Lets each session that you start from the board start a few helper sessions. Off stops new worker grants; grants that exist stay.',
  clone_transport: 'HTTPS uses your Git credential helper. SSH uses your SSH keys.',
  'worker_grant.stations': 'The stations where a worker grant works.',
  'worker_grant.claude_mode':
    'The most permissive mode that a worker can give to a session that it starts. Default: Accept edits.',
  'worker_grant.codex_mode':
    'The most permissive mode that a worker can give to a session that it starts. Default: Ask when needed · workspace write.',
};

/** `Default 200000.` → `Default: 200,000.`: one form of a default in every help. */
const withDefault = (help: string): string =>
  help.replace(/\bDefault (\d+)\b/g, (_, n: string) => `Default: ${countText(Number(n))}`);

/**
 * The help of a field: the board's words, else the registry help. `deck` is true on the deck tab,
 * where a key of the deck file can have its own words.
 */
export const helpOf = (meta: Pick<FieldMeta, 'key' | 'help'>, deck = false): string =>
  withDefault((deck ? HELP[`deck:${meta.key}`] : undefined) ?? HELP[meta.key] ?? meta.help);

/**
 * The fields whose switch works only while another one is on: the page indents them under it, and
 * the section lists them right after it (model.ts).
 */
export const DEPENDS_ON: Readonly<Record<string, string>> = {
  allow_desktop_click: 'allow_desktop_input',
  allow_desktop_input_any: 'allow_desktop_input',
};

/** The words of an enum option, by field key and value. */
const OPTION_LABELS: Readonly<Record<string, Readonly<Record<string, string>>>> = {
  host: { direct: 'Directly', shim: 'Apart from the station (survives restarts)' },
  clone_transport: { https: 'HTTPS', ssh: 'SSH' },
  'model_help.provider': { claude: 'Claude', codex: 'Codex' },
  'worker_grant.claude_mode': {
    default: 'Ask',
    plan: 'Plan',
    acceptEdits: 'Accept edits',
    dontAsk: "Don't ask",
    auto: 'Auto',
  },
};

/** The text of one option of an enum field: its words, or the value when it has none. */
export function optionLabel(key: string, value: string): string {
  if (key === 'worker_grant.codex_mode') return codexModeLabel(value);
  return OPTION_LABELS[key]?.[value] ?? value;
}

/** The value that an empty enum (the key is not in the file) means, when the board knows it. */
const DEFAULT_VALUES: Readonly<Record<string, string>> = {
  clone_transport: 'https',
  'worker_grant.claude_mode': 'acceptEdits',
  'worker_grant.codex_mode': 'on-request/workspace-write',
};

/** The systems where a station runs sessions in a shim by default (`host`; config.rs
 * `HostChoice::for_this_system`). */
const SHIM_SYSTEMS: readonly string[] = ['linux', 'omarchy'];

/**
 * The text of the empty option of an enum field: "Default (<what it means>)" when the board knows
 * the default, else "Default". `os` is the operating system of the station (`traits.os`).
 */
export function defaultOptionLabel(key: string, os?: string): string {
  if (key === 'host' && os !== undefined) {
    return `Default (${SHIM_SYSTEMS.includes(os.toLowerCase()) ? 'Apart from the station' : 'Directly'})`;
  }
  if (key === 'model_help.provider') return 'Default (the first signed-in provider)';
  const value = DEFAULT_VALUES[key];
  return value === undefined ? 'Default' : `Default (${optionLabel(key, value)})`;
}
