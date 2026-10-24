// B28: states and attempts of the provider update tests.
import type {
  CliUpdateAttempt,
  CliUpdateReport,
  CliUpdateRunAnswer,
  ProviderUpdateState,
} from '../gen';

export const NOW = 1_800_000_000_000;
export const HOURS_3 = 3 * 3_600_000;

export const attempt = (extra: Partial<CliUpdateAttempt> = {}): CliUpdateAttempt => ({
  seq: 1,
  attempt_id: 'a1',
  provider: 'claude',
  trigger: 'owner',
  method: 'native',
  channel: 'stable',
  from_version: '2.1.308',
  target_version: '2.1.310',
  to_version: '2.1.310',
  outcome: 'updated',
  reason: null,
  started_at: NOW - HOURS_3 - 1000,
  ended_at: NOW - HOURS_3,
  ...extra,
});

export const state = (extra: Partial<ProviderUpdateState> = {}): ProviderUpdateState => ({
  provider: 'claude',
  method: 'native',
  updatable: true,
  blocked: null,
  channel: 'stable',
  installed: '2.1.309',
  available: null,
  checked_at: NOW - HOURS_3,
  last: null,
  quarantined: null,
  ...extra,
});

export const report = (extra: Partial<CliUpdateReport> = {}): CliUpdateReport => ({
  enabled: false,
  window: '03:00-05:00 local',
  providers: [state()],
  running: null,
  last_seq: 0,
  ...extra,
});

export const answer = (extra: Partial<CliUpdateRunAnswer> = {}): CliUpdateRunAnswer => ({
  state: state(),
  attempt_id: null,
  refused: null,
  busy: [],
  ...extra,
});
