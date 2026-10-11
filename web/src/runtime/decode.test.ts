import { expect, test } from 'bun:test';
import { boardMessage } from './decode';
import { card, envelope, key } from './testing';

test('the runtime rejects malformed projections before they enter the cache', () => {
  for (const v of [
    null,
    {},
    { type: 'snapshot', version: 1 },
    { type: 'snapshot', version: -1, stations: [], sessions: [] },
    {
      type: 'snapshot',
      version: 1,
      stations: [],
      sessions: [{ station_id: 'station', summary: null }],
    },
    { type: 'delta', version: 1, stations: null, sessions: [], removed: [{}] },
    { type: 'timeline_events', ...key, events: [{ ...envelope(1), station_seq: -1 }] },
  ]) {
    expect(() => boardMessage(JSON.stringify(v))).toThrow('Invalid board message');
  }
  const v = { type: 'snapshot', version: 1, stations: [], sessions: [card()] };
  expect(JSON.stringify(boardMessage(JSON.stringify(v)))).toBe(JSON.stringify(v));
});

test('canonical unknown event kinds remain available to a readable timeline', () => {
  const event = { ...envelope(1), event: { kind: 'future.kind', data: 'kept' } };
  const v = { type: 'timeline_events', ...key, events: [event] };
  expect(JSON.stringify(boardMessage(JSON.stringify(v)))).toBe(JSON.stringify(v));
});
