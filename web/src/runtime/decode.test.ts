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

test('the people messages of a shared session pass the check (R11, DL-166)', () => {
  for (const v of [
    { type: 'presence', ...key, who: ['owner', 'member:Ana'] },
    { type: 'chat', ...key, message: { seq: 1, author: 'owner', text: 'hi', at: 5 } },
    { type: 'driver', ...key, driver: null },
    { type: 'driver', ...key, driver: 'member:Ana' },
    { type: 'drive_request', ...key, requests: [] },
  ]) {
    expect(JSON.stringify(boardMessage(JSON.stringify(v)))).toBe(JSON.stringify(v));
  }
  for (const v of [
    { type: 'presence', ...key, who: [1] },
    { type: 'presence', who: [] },
    { type: 'chat', ...key, message: { seq: -1, author: 'owner', text: 'hi', at: 5 } },
    { type: 'chat', ...key, message: null },
    { type: 'driver', ...key, driver: 3 },
    { type: 'drive_request', ...key, requests: 'x' },
  ]) {
    expect(() => boardMessage(JSON.stringify(v))).toThrow('Invalid board message');
  }
});
