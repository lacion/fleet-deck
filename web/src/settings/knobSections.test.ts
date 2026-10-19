import { describe, expect, test } from 'bun:test';
import { meta } from './fixtures';
import { groupFields } from './model';

const DECK = {
  'Security limits': [
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
  'Time limits': ['limits.delivery_hours', 'limits.radar_minutes'],
  Retention: ['retention.audit_days', 'retention.audit_max_rows'],
};

const STATION = {
  'Time limits': [
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
  Retention: ['retention.history_days', 'retention.event_days', 'retention.raw_log_mib'],
  'Captures and recordings': [
    'capture.quota_gib',
    'capture.free_reserve_gib',
    'capture.recording_max_seconds',
  ],
};

const intField = (key: string) => meta(key, key, 'int');

describe('the owner knob sections (knobs design section 4)', () => {
  test('each knob lands in its section, for a deck view and for a station view', () => {
    for (const expected of [DECK, STATION]) {
      const groups = groupFields(Object.values(expected).flat().map(intField));
      expect(groups.map((g) => g.title)).toEqual(Object.keys(expected));
      for (const [title, keys] of Object.entries(expected)) {
        expect(groups.find((g) => g.title === title)?.fields.map((f) => f.key)).toEqual(keys);
      }
    }
  });

  test('the four sections come after the older ones; one section lists both files', () => {
    const keys = [...Object.values(DECK), ...Object.values(STATION), ['name']].flat();
    const groups = groupFields(keys.map(intField));
    expect(groups.map((g) => g.title)).toEqual([
      'General',
      'Security limits',
      'Time limits',
      'Retention',
      'Captures and recordings',
    ]);
    // The deck keys come first in "Time limits", then the station keys, in the table order.
    expect(groups[2]?.fields.map((f) => f.key)).toEqual([
      ...DECK['Time limits'],
      ...STATION['Time limits'],
    ]);
    expect(groups[3]?.fields.map((f) => f.key)).toEqual([...DECK.Retention, ...STATION.Retention]);
  });
});
