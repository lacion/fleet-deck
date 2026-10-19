// DL-172: the settings model of the board: the sections, the badges of a field, and the diff of
// an edit against the values that the deck showed. Pure functions; the components call them.
import type { FieldMeta, SettingsView } from '../gen';

/** The values that the deck showed, or the edits of the owner, by key. */
export type Values = Readonly<Record<string, unknown>>;

const SECTIONS: readonly { readonly title: string; readonly keys: readonly string[] }[] = [
  { title: 'General', keys: ['name'] },
  {
    title: 'Sessions',
    keys: [
      'max_sessions',
      'offline_approval_policy',
      'hibernate_after_min',
      'host',
      'shim_buffer_mib',
      'allow_bypass_permissions',
    ],
  },
  { title: 'Folders', keys: ['allowed_roots', 'repo_groups'] },
  { title: 'Shells and setup', keys: ['allow_shell', 'max_shells', 'allow_setup_cmd'] },
  { title: 'Providers', keys: ['provider_instance'] },
  {
    title: 'Model help',
    keys: [
      'model_help.enabled',
      'model_help.provider',
      'model_help.claude_model',
      'model_help.codex_model',
      'model_help.per_call_max_output_tokens',
      'model_help.daily_token_cap',
    ],
  },
  { title: 'Sharing', keys: ['member_deny'] },
  { title: 'Fleet control', keys: ['fleet_control', 'allow_fleet_control'] },
  {
    title: 'Network',
    keys: ['listen', 'link_listen', 'tailnet_hostname', 'tailscale_bin', 'tailnet_auto'],
  },
  { title: 'Notifications', keys: ['conflict_mail'] },
  { title: 'New sessions', keys: ['clone_host', 'clone_org', 'clone_transport'] },
  { title: 'Trackers', keys: ['trackers'] },
  {
    title: 'Fleet Git',
    keys: ['clone_root', 'traits', 'allow_check_cmd', 'allow_local_remotes', 'taildrive_mounts'],
  },
  // The owner knobs (knobs design section 4). One section lists the keys of the deck and of the
  // station; `groupFields` skips a key that the view of one file does not have.
  {
    title: 'Security limits',
    keys: [
      'security.min_password_chars',
      'security.login_idle_days',
      'security.login_max_days',
      'security.elevation_minutes',
      'security.api_token_max_days',
      'security.display_idle_days',
      'security.display_max_days',
      'security.invite_minutes',
      'security.desktop_screenshots_per_minute',
      'security.desktop_actions_per_minute',
    ],
  },
  {
    title: 'Time limits',
    keys: [
      'limits.delivery_hours',
      'limits.radar_minutes',
      'limits.shell_command_minutes',
      'limits.setup_minutes',
      'limits.land_check_minutes',
      'limits.clone_minutes',
      'limits.git_push_seconds',
      'limits.provider_stop_grace_seconds',
      'limits.reconnect_max_seconds',
      'limits.scrollback_lines',
      'limits.scrollback_mib',
    ],
  },
  {
    title: 'Retention',
    keys: [
      'retention.audit_days',
      'retention.audit_max_rows',
      'retention.history_days',
      'retention.event_days',
      'retention.raw_log_mib',
    ],
  },
  {
    title: 'Captures and recordings',
    keys: ['capture.quota_gib', 'capture.free_reserve_gib', 'capture.recording_max_seconds'],
  },
];

export type Group = { readonly title: string; readonly fields: readonly FieldMeta[] };

/**
 * The fields in the sections of the page, in order. A key that no section names goes to "Other".
 * A hidden field (a key that only the deck writes) is left out.
 */
export function groupFields(all: readonly FieldMeta[]): Group[] {
  const fields = all.filter((f) => f.hidden !== true);
  const used = new Set<string>();
  const groups: Group[] = [];
  for (const section of SECTIONS) {
    const inSection: FieldMeta[] = [];
    for (const key of section.keys) {
      const f = fields.find((x) => x.key === key);
      if (f !== undefined) {
        inSection.push(f);
        used.add(key);
      }
    }
    // A model help key that is not in the list above stays in its own section.
    if (section.title === 'Model help') {
      for (const f of fields) {
        if (f.key.startsWith('model_help.') && !used.has(f.key)) {
          inSection.push(f);
          used.add(f.key);
        }
      }
    }
    if (inSection.length > 0) groups.push({ title: section.title, fields: inSection });
  }
  const other = fields.filter((f) => !used.has(f.key));
  if (other.length > 0) groups.push({ title: 'Other', fields: other });
  return groups;
}

export type Badge = {
  readonly text: string;
  readonly tone?: 'act';
  readonly title?: string;
};

/** The badges of a field: restart, set by the command line, needs re-auth. */
export function badgesOf(meta: FieldMeta, overrides: readonly string[]): Badge[] {
  const out: Badge[] = [];
  if (meta.apply === 'restart') out.push({ text: 'Applies after a restart' });
  const flag = meta.cli_flag;
  if (overrides.includes(meta.key)) {
    out.push({
      text: flag === null ? 'Set by the command line' : `Set by the command line (${flag})`,
    });
  }
  if (meta.danger) {
    out.push({
      text: 'needs re-auth',
      tone: 'act',
      title: 'Saving this asks for your password or an elevation code',
    });
  }
  return out;
}

// --- the shapes of the values ---

export type Instance = {
  readonly id: string;
  readonly driver: string;
  readonly binary: string;
  readonly config_home: string | null;
  /** `{ set: true }` for a value that the station holds; `null` for a variable that the edit removes. */
  readonly env: Readonly<Record<string, { readonly set: true } | null>>;
};

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

export const asStrings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];

export const asText = (v: unknown): string => (typeof v === 'string' ? v : '');

export const asNumber = (v: unknown): number | null =>
  typeof v === 'number' && Number.isFinite(v) ? v : null;

export function asInstances(v: unknown): Instance[] {
  if (!Array.isArray(v)) return [];
  return v.filter(isRecord).map((o) => {
    const env: Record<string, { set: true } | null> = {};
    if (isRecord(o['env'])) {
      for (const [name, value] of Object.entries(o['env']))
        env[name] = value === null ? null : { set: true };
    }
    const home = o['config_home'];
    return {
      id: asText(o['id']),
      driver: asText(o['driver']),
      binary: asText(o['binary']),
      config_home: typeof home === 'string' && home !== '' ? home : null,
      env,
    };
  });
}

/** The repository groups as editable rows: the value is `{ path: group }`. */
export function asPairs(v: unknown): [string, string][] {
  if (Array.isArray(v)) {
    return v.filter(
      (x): x is [string, string] =>
        Array.isArray(x) && typeof x[0] === 'string' && typeof x[1] === 'string',
    );
  }
  return isRecord(v)
    ? Object.entries(v).flatMap(([p, g]) =>
        typeof g === 'string' ? [[p, g] as [string, string]] : [],
      )
    : [];
}

/** The value of a field as the deck takes it: `null` removes the key (a list or a text that was cleared). */
export function wire(meta: FieldMeta, value: unknown): unknown {
  switch (meta.kind) {
    case 'bool':
      return typeof value === 'boolean' ? value : null;
    case 'int':
      return asNumber(value);
    case 'text':
    case 'enum':
    case 'path':
      return typeof value === 'string' && value.trim() !== '' ? value : null;
    case 'paths':
    case 'texts':
    case 'addresses': {
      const list = asStrings(value).filter((s) => s.trim() !== '');
      return list.length === 0 ? null : list;
    }
    case 'repo_groups':
    case 'share_mounts': {
      const rows = asPairs(value).filter(([p]) => p.trim() !== '');
      return rows.length === 0 ? null : Object.fromEntries(rows);
    }
    case 'instances':
      // An empty list is "no instance"; a missing key would mean the default instances.
      return asInstances(value);
    case 'trackers':
      return value;
  }
}

/** Equal by content; the order of the keys of an object does not count. */
export function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((x, i) => same(x, b[i]));
  }
  if (isRecord(a) && isRecord(b)) {
    const keys = Object.keys(a);
    return keys.length === Object.keys(b).length && keys.every((k) => k in b && same(a[k], b[k]));
  }
  return false;
}

/** The value that a field shows: the edit when there is one, else the value of the deck. */
export function shown(view: SettingsView, edits: Values, key: string): unknown {
  return key in edits ? edits[key] : view.values[key];
}

/**
 * The changes to save: only the keys whose edit differs from the deck's value, each in the shape
 * that the deck takes, and `null` for a list or an optional value that the owner cleared.
 */
export function changesOf(view: SettingsView, edits: Values): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(edits)) {
    const meta = view.fields.find((f) => f.key === key);
    if (meta === undefined || meta.read_only || view.overrides.includes(key)) continue;
    const next = wire(meta, value);
    if (!same(next, wire(meta, view.values[key]))) out[key] = next;
  }
  return out;
}

/** The keys that the owner has changed, in the order of the field table. */
export function changedKeys(view: SettingsView, edits: Values): string[] {
  const changes = changesOf(view, edits);
  return view.fields.map((f) => f.key).filter((k) => k in changes);
}

/** The label of a key, for a list of danger changes. */
export function labelOf(fields: readonly FieldMeta[], key: string): string {
  return fields.find((f) => f.key === key)?.label ?? key;
}
