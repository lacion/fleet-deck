// The board's own words for the settings registry: the labels, the help texts and the option names
// of the fields. The registry of each station binary carries its own copy; a key in these maps shows
// the board's copy instead, so that one field reads the same on every station version and never
// shows raw protocol values. The values on the wire stay the same.
import type { FieldMeta } from '../gen';
import { codexModeLabel } from '../ui/format';

/** The label of a field when the registry label repeats its section heading. */
const FIELD_LABELS: Readonly<Record<string, string>> = {
  fleet_control: 'Allow fleet control',
  'worker_grant.enabled': 'Give new sessions a grant',
  'worker_grant.stations': 'Stations',
  'worker_grant.spawn_cap': 'Sessions per worker',
  'worker_grant.claude_mode': 'Most permissive Claude mode',
  'worker_grant.codex_mode': 'Most permissive Codex mode',
  'worker_grant.lifetime_hours': 'Lifetime (hours)',
  'worker_grant.idle_minutes': 'Idle time (minutes)',
  hibernate_after_min: 'Stop idle sessions after',
};

/** The label of a field: the board's words, else the registry label. */
export const fieldLabel = (meta: Pick<FieldMeta, 'key' | 'label'>): string =>
  FIELD_LABELS[meta.key] ?? meta.label;

/** The help of a field when the registry help shows raw values or a syntax to type. */
const HELP: Readonly<Record<string, string>> = {
  host: 'Directly: sessions end when the station stops. In a shim: sessions keep running while the station restarts.',
  offline_approval_policy:
    'What this station does with a permission request while no deck is connected.',
  hibernate_after_min:
    'Stop a session that is idle this long. You can resume it in the board. Leave it empty to keep idle sessions running.',
  allow_bypass_permissions:
    'Offer the Bypass mode, which skips every permission check, for sessions on this station.',
  clone_transport: 'HTTPS uses your Git credential helper. SSH uses your SSH keys.',
  'worker_grant.stations': 'The stations where a worker grant works.',
  'worker_grant.claude_mode':
    'The most permissive mode that a worker can give to a session that it starts. Default: Accept edits.',
  'worker_grant.codex_mode':
    'The most permissive mode that a worker can give to a session that it starts. Default: Ask when needed · workspace write.',
};

/** The help of a field: the board's words, else the registry help. */
export const helpOf = (meta: Pick<FieldMeta, 'key' | 'help'>): string =>
  HELP[meta.key] ?? meta.help;

/** The words of an enum option, by field key and value. */
const OPTION_LABELS: Readonly<Record<string, Readonly<Record<string, string>>>> = {
  host: { direct: 'Directly', shim: 'In a shim (survives restarts)' },
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
    return `Default (${SHIM_SYSTEMS.includes(os.toLowerCase()) ? 'In a shim' : 'Directly'})`;
  }
  if (key === 'model_help.provider') return 'Default (the first signed-in provider)';
  const value = DEFAULT_VALUES[key];
  return value === undefined ? 'Default' : `Default (${optionLabel(key, value)})`;
}
