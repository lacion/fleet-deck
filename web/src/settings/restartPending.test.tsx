// DL-500, DL-501: the saved keys that wait for a restart: the pending set, the marker on a field
// and the count of the notice.
import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { SettingsSaved } from '../gen';
import type { SettingsActions } from './actions';
import { FIELDS, meta, VALUES, view } from './fixtures';
import { badgesOf } from './model';
import { nextPending } from './restartPending';
import { noticesOfSaved, SettingsTab } from './SettingsTab';

const saved = (extra: Partial<SettingsSaved> = {}): SettingsSaved => ({
  values: VALUES,
  applied: [],
  restart_needed: [],
  unapplied: [],
  revision: 'rev-2',
  durability: 'confirmed',
  ...extra,
});

const before = { ...VALUES, listen: 'a' };

describe('the pending restart set', () => {
  test('nextPending adds each restart key of a save with its value before the save', () => {
    const next = nextPending({}, before, saved({ restart_needed: ['listen'] }), FIELDS);
    expect(next).toEqual({ listen: JSON.stringify('a') });
  });

  test('a later save of live keys keeps the pending keys', () => {
    const next = nextPending(
      { listen: JSON.stringify('a') },
      VALUES,
      saved({ restart_needed: [], applied: ['host'], values: { ...VALUES, listen: 'b' } }),
      FIELDS,
    );
    expect(Object.keys(next)).toEqual(['listen']);
  });

  test('a key changed back to its first value leaves the list', () => {
    const next = nextPending(
      { listen: '"a"' },
      { ...VALUES, listen: 'b' },
      saved({ values: { ...VALUES, listen: 'a' } }),
      FIELDS,
    );
    expect(next).toEqual({});
  });

  test('a secret field never leaves the list on an equal value', () => {
    const value = { set: true };
    const next = nextPending(
      {},
      { ...VALUES, provider_instance: value },
      saved({
        restart_needed: ['provider_instance'],
        values: { ...VALUES, provider_instance: value },
      }),
      FIELDS,
    );
    expect(Object.keys(next)).toEqual(['provider_instance']);
  });
});

describe('the marker', () => {
  test('badgesOf gives "Restart needed" with the info tone only to a pending restart field', () => {
    const listen = FIELDS.find((f) => f.key === 'listen');
    expect(listen).toBeDefined();
    if (listen === undefined) return;
    expect(badgesOf(listen, [], true)).toEqual([{ text: 'Restart needed', tone: 'info' }]);
    expect(badgesOf(listen, [])).toEqual([{ text: 'Applies after a restart' }]);
  });

  test('a live field gets no badge when pending', () => {
    expect(badgesOf(meta('name', 'Name', 'text'), [], true)).toEqual([]);
  });

  test('noticesOfSaved counts the pending keys, not the keys of the last save', () => {
    expect(noticesOfSaved(saved({ restart_needed: [] }), 2)).toEqual([
      { id: 'restart', tone: 'info', text: '2 settings apply after a restart.' },
    ]);
  });

  test('the tab marks a pending field and keeps the other restart fields neutral', () => {
    const actions = {} as SettingsActions;
    const html = renderToStaticMarkup(
      <SettingsTab
        stationId="st-alpha"
        name="Alpha"
        actions={actions}
        live
        initial={{ view: view(), pending: { listen: '[]' } }}
      />,
    );
    expect(html).toContain('<span class="fd-badge" data-tone="info">');
    expect(html).toContain('Restart needed');
    expect(html).toContain('Applies after a restart');
    expect(html).toContain(' Restart needed.');
  });
});
