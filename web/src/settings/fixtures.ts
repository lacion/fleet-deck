// Settings views for the tests: one field of each kind, with the values that the deck shows.
import type { FieldMeta, SettingsView } from '../gen';

export const meta = (
  key: string,
  label: string,
  kind: FieldMeta['kind'],
  extra: Partial<FieldMeta> = {},
): FieldMeta => ({
  key,
  label,
  help: `Help of ${label}.`,
  kind,
  min: null,
  max: null,
  options: [],
  apply: 'live',
  danger: false,
  secret: false,
  read_only: false,
  cli_flag: null,
  ...extra,
});

export const FIELDS: FieldMeta[] = [
  meta('name', 'Station name', 'text'),
  meta('max_sessions', 'Most sessions', 'int', { min: 1, max: 256 }),
  meta('allow_shell', 'Allow terminals', 'bool', { danger: true }),
  meta('host', 'Where sessions run', 'enum', {
    options: ['direct', 'shim'],
    apply: 'restart',
    danger: true,
  }),
  meta('allowed_roots', 'Allowed folders', 'paths', { danger: true }),
  meta('member_deny', 'Hidden file patterns', 'texts'),
  meta('listen', 'Listen addresses', 'addresses', {
    apply: 'restart',
    danger: true,
    cli_flag: '--listen',
  }),
  meta('repo_groups', 'Repository groups', 'repo_groups'),
  meta('provider_instance', 'Provider logins', 'instances', {
    apply: 'restart',
    danger: true,
    secret: true,
  }),
  meta('trackers', 'Issue trackers from the file', 'trackers', { read_only: true }),
  meta('model_help.enabled', 'Model help', 'bool'),
  meta('model_help.provider', 'Provider for help', 'enum', { options: ['claude', 'codex'] }),
  meta('zz_future', 'A future key', 'text'),
];

export const VALUES: Record<string, unknown> = {
  name: 'Alpha',
  max_sessions: 8,
  allow_shell: true,
  host: 'shim',
  allowed_roots: ['/work', '/srv'],
  member_deny: ['*.pem'],
  listen: ['127.0.0.1:7421'],
  repo_groups: { '/work/app': 'app' },
  provider_instance: [
    {
      id: 'claude',
      driver: 'claude',
      binary: 'claude',
      config_home: null,
      env: { ANTHROPIC_API_KEY: { set: true }, OTHER: { set: true } },
    },
  ],
  trackers: [
    {
      id: 'jira-main',
      kind: 'jira',
      site: 'acme.atlassian.net',
      email: null,
      token: { env: 'JIRA_TOKEN' },
    },
  ],
  'model_help.enabled': false,
  'model_help.provider': null,
  zz_future: 'x',
};

export function view(extra: Partial<SettingsView> = {}): SettingsView {
  return {
    values: VALUES,
    fields: FIELDS,
    revision: 'rev-1',
    overrides: ['listen'],
    ...extra,
  };
}
