import { describe, expect, test } from 'bun:test';
import {
  authLabel,
  expiresText,
  invitationView,
  requestedName,
  stationActions,
  stationResult,
} from './pairing';

describe('pairing calls', () => {
  test('each action posts to its board method with an encoded station name', async () => {
    const calls: [string, unknown][] = [];
    const replies: unknown[] = [
      { invitation: 'fdinv1_x', expires_at: 1, join_command: 'fleetdeck station join' },
      { station_id: 's1', name: 'alpha' },
      { station_id: 's2', name: 'a b/c' },
    ];
    const actions = stationActions(async (path, body) => {
      calls.push([path, body]);
      return replies.shift();
    }, true);
    expect((await actions.invite()).invitation).toBe('fdinv1_x');
    expect((await actions.add({ invitation: 'fdinv1_y', name: null })).name).toBe('alpha');
    expect((await actions.remove('a b/c')).station_id).toBe('s2');
    expect(calls).toEqual([
      ['/api/stations/invite', {}],
      ['/api/stations', { invitation: 'fdinv1_y', name: null }],
      ['/api/stations/a%20b%2Fc/remove', {}],
    ]);
  });
  test('malformed responses are refused instead of shown', () => {
    expect(() => invitationView({ invitation: '', expires_at: 1, join_command: 'x' })).toThrow();
    expect(() => invitationView({ invitation: 'a', expires_at: 1.5, join_command: 'x' })).toThrow();
    expect(() => invitationView(null)).toThrow();
    expect(() => stationResult({ name: 'x' })).toThrow();
    expect(() => stationResult([])).toThrow();
  });
  test('a blank name asks for the station name and labels stay explicit', () => {
    expect(requestedName('  ')).toBeNull();
    expect(requestedName(' box ')).toBe('box');
    expect(authLabel('signed_in')).toBe('signed in');
    expect(authLabel('signed_out')).toBe('signed out');
    expect(authLabel('anything')).toBe('sign-in unknown');
    expect(expiresText(0)).toBe('1970-01-01 00:00 UTC');
  });
});
